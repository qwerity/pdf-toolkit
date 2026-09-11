//! From a PDF's content streams to GeoJSON on a map.
//!
//! One pass per page, in three steps that each answer a different question:
//!
//! 1. **What is on the page?** `pdf-geometry` interprets the content stream and
//!    recovers the real vector paths and text runs a CAD exporter wrote.
//! 2. **Is it a drawing worth placing?** `pdf-classify` decides from cheap
//!    structural features — line density, layer count, cadastral codes.
//! 3. **Where on Earth is it?** `pdf-geo` fits a transform from page space to
//!    the ground and writes the geometry out as GeoJSON.
//!
//! # Two ways a page gets placed
//!
//! A survey sheet usually prints a table of boundary turning-point coordinates,
//! and that is enough on its own: the table is read from the text layer, the
//! same polygon is found in the drawing, and a similarity transform is fitted
//! between the two. Residuals come out in **millimetres**, which is the check
//! that the answer is right — a fit that does not agree with the drawing is
//! reported as a failure rather than guessed at.
//!
//! When a sheet prints no such table, nothing on the page says where it is. The
//! caller can then supply [`Reference`]s — a point in the drawing and where that
//! point actually is — and two of them settle position, rotation and scale
//! together.
//!
//! # A page is never silently dropped
//!
//! Every selected page yields a [`PageVector`] carrying GeoJSON, whether or not
//! it could be placed. An unplaced page's geometry is written in its own frame
//! and says so in `crs`: `local_metres` when the sheet's scale was readable,
//! `page_units` when it was not. The shapes are correct in both; only their
//! position on Earth is missing, which is exactly what [`Reference`]s supply.

use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;

use pdf_classify::{PageFeatures, heuristic_label};
use pdf_geo::ControlTable;
use pdf_geo::control::{declared_scale_denominators, table_candidates_from_text_layer};
use pdf_geo::fit::FitConstraints;
use pdf_geo::geojson::{ExportOptions, export_local, export_page};
use pdf_geo::local::{LocalFrame, Reference, ScaleSource, place};
use pdf_geo::pipeline::georeference_page;
use pdf_geo::pipeline::{GeoreferenceFailure, GeoreferenceResult, enforce_document_crs};
use pdf_geometry::PdfDocument;
use pdf_geometry::model::{BBox, PageContent};

pub mod report;

pub use report::{DocumentVector, GeoreferenceReport, PageVector, Placement};

/// How to run the extraction.
#[derive(Debug, Clone, Default)]
pub struct VectorOptions {
    /// Real-world references for pages that carry no coordinate table.
    ///
    /// Two or more place a drawing exported in its own frame. They apply to
    /// every page that could not be placed from its own text layer, so passing
    /// them alongside a multi-page selection only affects the pages that need
    /// them.
    pub references: Vec<Reference>,
    pub export: ExportOptions,
}

/// Where a page's placement came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlacementSource {
    /// A turning-point table printed on the sheet and readable as text.
    TextLayer,
    /// References supplied by the caller.
    References,
}

/// Runs the pipeline over selected pages of one document.
///
/// `pages` is one-based; an empty slice means every page.
pub fn extract_document(
    path: &Path,
    pages: &[usize],
    options: &VectorOptions,
) -> Result<DocumentVector> {
    let document =
        PdfDocument::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let document_id = document.id();
    let total = document.page_count();

    let wanted: Vec<usize> = if pages.is_empty() {
        (1..=total).collect()
    } else {
        pages
            .iter()
            .copied()
            .filter(|n| *n >= 1 && *n <= total)
            .collect()
    };

    let mut contents = Vec::with_capacity(wanted.len());
    let mut warnings = Vec::new();
    for page_number in wanted {
        match document.page_content(page_number) {
            Ok(content) => contents.push(content),
            // One unreadable page does not end the run; the rest still export.
            Err(error) => warnings.push(format!("page {page_number}: {error:#}")),
        }
    }

    // Fit every page on its own first, then let the document as a whole have a
    // say. A page that fitted to centimetres has established which coordinate
    // system this dossier is drawn in, and that is worth more than any single
    // page's tolerance: its siblings are re-tried against that CRS with a
    // looser bound, which places sheets whose corners are drawn half a
    // line-width off. Every other guard still applies.
    let tables: Vec<Vec<ControlTable>> = contents
        .iter()
        .map(table_candidates_from_text_layer)
        .collect();
    let constraints = FitConstraints::default();
    let mut fits: Vec<Result<GeoreferenceResult, GeoreferenceFailure>> = contents
        .iter()
        .zip(&tables)
        .map(|(page, candidates)| georeference_page(page, candidates, constraints))
        .collect();
    enforce_document_crs(&contents, &tables, &mut fits, constraints);

    let pages = contents
        .iter()
        .zip(&tables)
        .zip(fits)
        .map(|((page, candidates), fit)| export(page, candidates, fit, &document_id, options))
        .collect();

    Ok(DocumentVector {
        document: document_id,
        source: path.display().to_string(),
        page_count: total,
        pages,
        warnings,
    })
}

/// Classifies one page and exports whatever geometry it holds.
///
/// Georeferences the page on its own. [`extract_document`] is preferred where a
/// whole dossier is available, because a page that cannot be placed by itself
/// often can be once a sibling has established the document's CRS.
pub fn extract_page(page: &PageContent, document_id: &str, options: &VectorOptions) -> PageVector {
    let tables = table_candidates_from_text_layer(page);
    let fit = georeference_page(page, &tables, FitConstraints::default());
    export(page, &tables, fit, document_id, options)
}

/// Turns one page's fit — or its failure — into the reported result.
fn export(
    page: &PageContent,
    tables: &[ControlTable],
    fit: Result<GeoreferenceResult, GeoreferenceFailure>,
    document_id: &str,
    options: &VectorOptions,
) -> PageVector {
    let features = PageFeatures::compute(page);
    let verdict = heuristic_label(&features);
    let classification = report::classification(&verdict, &features);
    let exclude = exclusion_boxes(page, tables);

    match fit {
        // The sheet's own table wins over supplied references: it is the
        // draughtsman's statement of where the parcel is, and it can be checked
        // against the drawing, which a reference pair cannot.
        Ok(result) => {
            let export = export_page(
                page,
                &result.georeference,
                document_id,
                &exclude,
                options.export,
            );
            PageVector {
                page_number: page.page_number,
                classification,
                georeference: Some(GeoreferenceReport::placed(
                    PlacementSource::TextLayer,
                    &result,
                )),
                crs: "EPSG:4326".into(),
                geojson: export.collection,
                summary: export.summary,
            }
        }
        Err(failure) => {
            let frame = local_frame(page);
            let mut export = export_local(page, &frame, document_id, &exclude, options.export);

            // The page could not place itself. Supplied references can still
            // put it on the ground: the geometry is exported in the page's own
            // frame first and then moved, because the local frame is what the
            // references are stated in, and composing in that order is what
            // makes a reference mean what the caller thinks it means.
            match place(&options.references) {
                Some(placement) => {
                    let mut bounds = BBox::EMPTY;
                    for feature in &mut export.collection.features {
                        if let Some(geometry) = feature.geometry.as_mut() {
                            report::reproject(&mut geometry.value, &placement, &mut bounds);
                        }
                    }
                    PageVector {
                        page_number: page.page_number,
                        classification,
                        georeference: Some(GeoreferenceReport::from_references(
                            &placement, &bounds,
                        )),
                        crs: "EPSG:4326".into(),
                        geojson: export.collection,
                        summary: export.summary,
                    }
                }
                None => PageVector {
                    page_number: page.page_number,
                    classification,
                    georeference: Some(GeoreferenceReport::unplaced(failure)),
                    crs: frame.coordinate_system.to_string(),
                    geojson: export.collection,
                    summary: export.summary,
                },
            }
        }
    }
}

/// The frame an unplaced page's geometry is written in — metres when the sheet
/// prints a scale, raw page units when it does not.
fn local_frame(page: &PageContent) -> LocalFrame {
    let scale = declared_scale_denominators(page)
        .first()
        .map(|d| (*d, ScaleSource::Declared));
    LocalFrame::new(page.vector_bbox(), scale)
}

/// Regions to keep out of the export: a coordinate table is lettering, not
/// terrain, and drawing it would put the sheet's own text on the map.
fn exclusion_boxes(page: &PageContent, tables: &[pdf_geo::ControlTable]) -> Vec<BBox> {
    tables
        .iter()
        .filter_map(|table| pdf_geo::control::table_exclusion_bbox(page, table))
        .collect()
}

/// Re-exported so a caller can build references without depending on `pdf-geo`.
pub use pdf_geo::local::Reference as GeoReference;

/// Parses `local_x,local_y,longitude,latitude`.
///
/// The local pair is a point in the drawing's own frame — the coordinates an
/// unplaced page's GeoJSON is written in — and the second pair is where that
/// point actually is.
pub fn parse_reference(raw: &str) -> std::result::Result<Reference, String> {
    let values: Vec<f64> = raw
        .split(',')
        .map(|v| v.trim().parse::<f64>())
        .collect::<std::result::Result<_, _>>()
        .map_err(|_| format!("`{raw}` is not four comma-separated numbers"))?;

    let [lx, ly, lon, lat] = values[..] else {
        return Err(format!(
            "`{raw}` should be local_x,local_y,longitude,latitude ({} value(s) given)",
            values.len()
        ));
    };
    if !(-180.0..=180.0).contains(&lon) || !(-90.0..=90.0).contains(&lat) {
        return Err(format!(
            "`{raw}` has longitude/latitude outside the world: {lon}, {lat}"
        ));
    }
    Ok(Reference {
        local: [lx, ly],
        wgs84: [lon, lat],
    })
}

/// Rounds to `digits`, used everywhere a coordinate reaches the output.
pub(crate) fn round(v: f64, digits: i32) -> f64 {
    let f = 10f64.powi(digits);
    (v * f).round() / f
}

/// Convenience for callers that want the centre of a placed page.
pub(crate) fn centre(bounds: &BBox) -> [f64; 2] {
    let c = bounds.center();
    [round(c.x, 7), round(c.y, 7)]
}

/// File name one page's GeoJSON is written under, shared by `vector
/// --output` and `dossier --output` so the same page gets the same name
/// whichever command exported it.
pub fn page_geojson_file_name(document: &str, page_number: usize) -> String {
    format!("{document}_p{page_number:02}.geojson")
}

/// Writes a page's `FeatureCollection` as pretty-printed GeoJSON, streaming
/// rather than buffering — a dense CAD sheet's collection runs to tens of
/// megabytes.
pub fn write_page_geojson(
    path: &std::path::Path,
    geojson: &geojson::FeatureCollection,
) -> Result<()> {
    let file = std::fs::File::create(path)
        .with_context(|| format!("cannot write '{}'", path.display()))?;
    serde_json::to_writer_pretty(std::io::BufWriter::new(file), geojson)
        .with_context(|| format!("failed to serialize GeoJSON for '{}'", path.display()))
}
