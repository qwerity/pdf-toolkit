//! The `model` command, end to end: read the document, settle the
//! parameters, build the model, place it if the dossier allows, write the
//! outputs, report.
//!
//! The one seam for a hosted model is [`RunOptions::proposer`]: a callback
//! that, given what detection found, returns proposed parameters. The
//! proposal is **checked against the document** before it is used — the
//! pages it names must exist and read as plan sheets, the levels must
//! stack, and the building it yields must be building-sized — and a
//! proposal that fails falls back to the detected parameters with a
//! warning. Coordinates are never proposed; only these numbers are.

use std::path::Path;

use anyhow::{Context, Result};
use pdf_geo::local::Reference;
use pdf_geometry::PdfDocument;
use pdf_vector::{DocumentVector, VectorOptions, extract_document, write_pretty_json};
use serde_json::json;

use crate::detect::{Detection, detect};
use crate::geojson::{self, Frame};
use crate::params::{Params, ParamsSource};
use crate::place::{self, ModelPlacement};
use crate::report::{FloorSummary, ModelReport, RoofSummary};
use crate::{Reconstruction, mesh, reconstruct};

/// A ground-floor slab outside this range (m²) is not a building the
/// parameters described: the scale or the sheet is wrong.
const PLAUSIBLE_SLAB_M2: std::ops::RangeInclusive<f64> = 8.0..=20_000.0;

/// Proposes parameters, given what detection settled and the vector pass
/// over the document.
pub type Proposer<'a> = &'a dyn Fn(&Detection, &DocumentVector) -> Result<Params>;

pub struct RunOptions<'a> {
    /// A parameters file; when given, detection and the proposer are skipped.
    pub params_file: Option<&'a Path>,
    /// Caller-supplied references placing the model (two or more).
    pub references: Vec<Reference>,
    /// Options for the vector pass the placement reads (`--parcel`).
    pub vector_options: VectorOptions,
    pub output_dir: Option<&'a Path>,
    pub proposer: Option<Proposer<'a>>,
}

/// Runs the pipeline over one document.
pub fn run(path: &Path, options: &RunOptions<'_>) -> Result<ModelReport> {
    let document =
        PdfDocument::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let vector = extract_document(path, &[], &options.vector_options)?;
    let mut warnings: Vec<String> = vector.warnings.clone();

    // A proposal is checked by building the model; that build is the one used.
    let (params, params_notes, prebuilt) = match options.params_file {
        Some(file) => (Params::load(file)?, Vec::new(), None),
        None => {
            let detection = detect(&document, &vector, &Params::archicad_template())?;
            match options.proposer {
                Some(propose) => {
                    proposed_or_detected(&document, detection, &vector, propose, &mut warnings)
                }
                None => (detection.params, detection.notes, None),
            }
        }
    };

    let Reconstruction { model, sheets } = match prebuilt {
        Some(built) => built,
        None => reconstruct(&document, &params)?,
    };
    for sheet in &sheets {
        if sheet.origin_source == crate::OriginSource::AxisText {
            warnings.push(format!(
                "page {}: no circle around the grid labels; origin taken from the text position",
                sheet.page
            ));
        }
    }

    let placement: Option<ModelPlacement> = if options.references.len() >= 2 {
        place::from_references(&options.references)
    } else {
        place::from_document(&model, &vector)
    };
    if placement.is_none() {
        warnings.push(
            "not placed: no placed sheet draws the building outline and no references were \
             given; the model is in its own metres"
                .to_string(),
        );
    }

    let frame = Frame {
        placement: placement.as_ref().map(|p| &p.placement),
    };
    let collection = geojson::collection(
        &model,
        &frame,
        params.plinth_m,
        params.rail_height_m,
        json!({
            "document": vector.document,
            "source": vector.source,
            "params_source": params.source,
            "note": "footprints, walls and openings are the plan sheets' exact fills; \
                     levels and scale are the parameters",
        }),
    );

    let (model_file, glb_file, geojson_file, params_file) = match options.output_dir {
        Some(dir) => {
            let subdir = dir.join("model");
            std::fs::create_dir_all(&subdir)
                .with_context(|| format!("cannot create '{}'", subdir.display()))?;
            let name = |suffix: &str| format!("model/{}.{suffix}", vector.document);
            let model_name = name("model.json");
            write_pretty_json(&dir.join(&model_name), &model)?;
            let glb_name = name("glb");
            mesh::write_glb(
                &dir.join(&glb_name),
                &model,
                params.plinth_m,
                params.rail_height_m,
            )?;
            let geojson_name = name("geojson");
            write_pretty_json(&dir.join(&geojson_name), &collection)?;
            let params_name = name("params.json");
            write_pretty_json(&dir.join(&params_name), &params)?;
            (
                Some(model_name),
                Some(glb_name),
                Some(geojson_name),
                Some(params_name),
            )
        }
        None => (None, None, None, None),
    };

    Ok(ModelReport {
        document: vector.document.clone(),
        source: vector.source.clone(),
        crs: frame.crs().into(),
        placed: placement.is_some(),
        georeference: placement.as_ref().map(place::report),
        bounds: placement
            .as_ref()
            .and_then(|p| place::placed_bounds(&model, &p.placement)),
        sheets,
        floors: model.floors.iter().map(FloorSummary::of).collect(),
        roof: model
            .roof
            .as_ref()
            .zip(params.roof_page)
            .map(|(roof, page)| RoofSummary::of(roof, page)),
        stone_piers: model.stone.len(),
        warnings,
        model_file,
        glb_file,
        geojson_file,
        params_file,
        params,
        params_notes,
        llm_usage: None,
        model,
        geojson: collection,
    })
}

/// Asks the proposer, checks its answer against the document, and keeps
/// the detected parameters when the answer does not hold up. A confirmed
/// proposal comes back with the reconstruction that confirmed it.
fn proposed_or_detected(
    document: &PdfDocument,
    detection: Detection,
    vector: &DocumentVector,
    propose: Proposer<'_>,
    warnings: &mut Vec<String>,
) -> (Params, Vec<String>, Option<Reconstruction>) {
    let proposal = match propose(&detection, vector) {
        Ok(mut params) => {
            params.source = ParamsSource::Model;
            params
        }
        Err(error) => {
            warnings.push(format!(
                "parameter proposal failed: {error:#}; using detection"
            ));
            return (detection.params, detection.notes, None);
        }
    };
    match check_proposal(document, &proposal, vector) {
        Ok(built) => {
            let mut notes = detection.notes;
            notes
                .push("parameters proposed by the model and confirmed against the drawings".into());
            (proposal, notes, Some(built))
        }
        Err(reason) => {
            warnings.push(format!(
                "parameter proposal rejected: {reason}; using detection"
            ));
            (detection.params, detection.notes, None)
        }
    }
}

/// Whether a proposal describes this document: its pages exist and are
/// drawings, its levels stack, and the building it yields is building-sized.
/// Returns that building, so the caller need not build it again.
fn check_proposal(
    document: &PdfDocument,
    params: &Params,
    vector: &DocumentVector,
) -> Result<Reconstruction, String> {
    params.validate().map_err(|e| e.to_string())?;
    let is_drawing = |page: usize| {
        vector
            .pages
            .iter()
            .find(|p| p.page_number == page)
            .is_some_and(|p| p.classification.is_drawing)
    };
    for floor in &params.floors {
        if !is_drawing(floor.page) {
            return Err(format!("page {} is not a drawing", floor.page));
        }
    }
    if let Some(page) = params.roof_page
        && !is_drawing(page)
    {
        return Err(format!("roof page {page} is not a drawing"));
    }
    for pair in params.floors.windows(2) {
        if pair[1].level_m <= pair[0].level_m {
            return Err("floor levels do not ascend".into());
        }
    }
    let built = reconstruct(document, params).map_err(|e| format!("{e:#}"))?;
    let slab = built
        .model
        .floors
        .first()
        .map(|f| f.slab.area_m2())
        .unwrap_or(0.0);
    if !PLAUSIBLE_SLAB_M2.contains(&slab) {
        return Err(format!(
            "the proposal yields a {slab:.1} m² ground-floor slab, not a building"
        ));
    }
    Ok(built)
}
