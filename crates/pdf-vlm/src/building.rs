//! Building reconstruction: what the permit says the finished site must
//! look like, rebuilt on the map from the dossier's own geometry.
//!
//! A dossier's drawing set ends in a **visualization section**: a
//! development scheme (every house block on the parcel), facades and
//! sections with level marks (`+7.600`), a roof plan, an indicators table,
//! and 3D renders of the finished buildings. The model is shown those
//! sheets *once, in one request*, next to footprint candidates extracted
//! from the anchor sheet as exact geometry, and answers with parallel tool
//! calls: one `add_part` per built structure — each house block of a row
//! development, the perimeter fence, a roof — plus one `set_facts` with the
//! printed indicator numbers. Heights, colours and roof forms come from the
//! sheets; **coordinates never come from the model**. The output is a
//! map-native extrusion set (`height`/`base`/`color` properties, fences as
//! ring polygons, pitched roofs as stepped insets) that a Mapbox/MapLibre
//! `fill-extrusion` layer renders directly, georeferenced where the anchor
//! sheet was placed.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use pdf_classify::PageClass;
use pdfium_render::prelude::Pdfium;
use serde::Serialize;
use serde_json::{Value, json};

use llm_providers::{Client, Effort, Request, Tool};
use pdf_core::page::open_document;
use pdf_dossier::Answers;
use pdf_vector::{DocumentVector, VectorOptions, extract_document};

use crate::DRAWING_RENDER_WIDTH_PX;
use crate::measure::{self, Body, Parcel, ParcelReport, centroid, ring_area, ring_points, round7};

/// The most pages sent in the one request: the anchor, elevations, floor
/// plans, the roof plan, and the visualization/render sheets.
const PAGE_BUDGET: usize = 8;

const CANDIDATE_LIMIT: usize = 40;

/// How thick a `fence` part's wall ring is drawn, in metres (or the
/// anchor's units where the frame is not metric).
const FENCE_THICKNESS: f64 = 0.3;

/// Inset factors and rise fractions for the two stepped rings a pitched
/// roof is approximated with — fill-extrusion tops are flat, so a slope
/// becomes a two-step pyramid, which reads correctly at map scale.
const ROOF_STEPS: [(f64, f64); 2] = [(0.72, 0.55), (0.45, 1.0)];

/// The reconstruction result, plus the extruded collection itself.
#[derive(Debug, Serialize)]
pub struct BuildingReport {
    pub document: String,
    pub source: String,
    pub model: String,
    /// The frame the footprints are written in, from the anchor sheet.
    pub crs: String,
    pub placed: bool,
    /// The sheet the footprint candidates were extracted from.
    pub anchor_page: usize,
    /// Every page shown to the model, anchor included.
    pub pages_used: Vec<usize>,
    pub parts: Vec<PartReport>,
    /// The indicator numbers printed in the drawing set, as the model read
    /// them (height, storeys, areas). Absent when none were reported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub facts: Option<Value>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// Written under `--output` as `building/<document>.geojson`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub geojson_file: Option<String>,
    /// The extruded FeatureCollection, inline — same policy as `vector`.
    pub geojson: Value,
    /// The parcel the measurements are taken against. Absent when no ring
    /// could be identified, or the frame is not metric.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parcel: Option<ParcelReport>,
    /// Written under `--output` as `building/<document>.measurements.geojson`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub measurements_file: Option<String>,
    /// Side lengths, setbacks and clearances as labelled LineStrings in the
    /// same frame as `geojson` — a toggleable dimensions layer. Absent
    /// together with `parcel`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub measurements: Option<Value>,
}

/// One structure the model declared.
#[derive(Debug, Serialize)]
pub struct PartReport {
    pub name: String,
    /// `block` (a building volume) or `fence` (a thin perimeter wall drawn
    /// as a ring polygon).
    pub kind: String,
    /// Top of the walls above ground, metres. For a pitched roof this is
    /// the eaves; `ridge_m` is the top.
    pub height_m: f64,
    /// Bottom of the part above ground, metres. `0` for ground-bearing parts.
    pub base_m: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ridge_m: Option<f64>,
    /// `flat` when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roof: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub floors: Option<u64>,
    /// Facade colour read off the renders, `#rrggbb`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Which extracted footprint the part stands on.
    pub footprint_candidate: usize,
    /// Area of the extruded footprint. Square metres where the anchor
    /// sheet gave metric coordinates; the anchor's own units otherwise.
    pub footprint_area: f64,
    /// Present when the part extrudes the exact outline drawn on this
    /// sheet, aligned onto the candidate's position, instead of the
    /// candidate's simplified block.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outline_page: Option<usize>,
}

/// Reconstructs the built structures of one dossier. Progress goes to
/// stderr; the returned report is the stdout contract.
pub fn reconstruct(
    client: &Client,
    pdfium: &Pdfium,
    pdf_path: &Path,
    vector_options: &VectorOptions,
    answers: Option<&Answers>,
    output_dir: Option<&Path>,
) -> Result<BuildingReport> {
    let vector = extract_document(pdf_path, &[], vector_options)?;
    let selection = select_pages(&vector, answers)
        .ok_or_else(|| anyhow!("no drawing page to anchor a footprint on"))?;
    let anchor = vector
        .pages
        .iter()
        .find(|p| p.page_number == selection.anchor)
        .expect("selection returns an existing page");
    let anchor_collection = serde_json::to_value(&anchor.geojson)?;
    let placed = anchor.georeference.as_ref().is_some_and(|g| g.placed);
    let metric = placed || anchor.crs == "local_metres";
    let parcel_ring = anchor_parcel(&anchor_collection);
    let mut candidates = footprint_candidates(&anchor_collection, placed, metric);
    mark_parcel_candidates(&mut candidates, parcel_ring.as_ref(), placed);
    if candidates.is_empty() {
        bail!(
            "page {} carries no closed outline usable as a footprint candidate",
            selection.anchor
        );
    }

    eprintln!(
        "building: pages {:?} ({} footprint candidates on page {}) -> {}",
        selection.all(),
        candidates.len(),
        selection.anchor,
        client.model()
    );
    let document = open_document(pdfium, pdf_path).map_err(anyhow::Error::from)?;
    let pages = document.pages();
    let mut request = Request::new(prompt(&vector, &selection, &candidates));
    for &number in &selection.all() {
        request.images_png.push(crate::render_page_png(
            pages,
            number,
            DRAWING_RENDER_WIDTH_PX,
        )?);
    }
    // Reasoning tokens count against this cap on Gemini, and a reconstruction
    // over seven sheets thinks for ten thousand of them before the first
    // tool call: at 12k the reply was cut off mid-thought on half the
    // corpus and read as "the model proposed no structure".
    request.max_output_tokens = 48_000;
    request.effort = Some(Effort::High);
    request.tools = vec![add_part_tool(), set_facts_tool()];
    let reply = client
        .ask(&request)
        .with_context(|| format!("reconstruction request to '{}' failed", client.model()))?;

    let mut warnings: Vec<String> = vector.warnings.clone();
    if selection.missing_renders {
        warnings.push(
            "no sheet is labelled photo_montage/3d_view, so the 3D renders were not \
             shown to the model (pages are never uploaded on a guess); pass --answers \
             with labels to include them"
                .to_string(),
        );
    }
    let mut specs = Vec::new();
    let mut facts = None;
    for call in &reply.tool_calls {
        match call.name.as_str() {
            "add_part" => match spec_from(&call.arguments, &candidates, specs.len()) {
                Ok(spec) => specs.push(spec),
                Err(reason) => warnings.push(format!("ignored a part: {reason}")),
            },
            "set_facts" => facts = Some(known_facts(&call.arguments)),
            other => warnings.push(format!("ignored unknown tool call '{other}'")),
        }
    }
    if specs.is_empty() {
        // Every reason is in the message: the rejected calls say what the
        // model tried, and the prose says why it stopped.
        let rejected: Vec<&str> = warnings
            .iter()
            .filter(|w| w.starts_with("ignored"))
            .map(String::as_str)
            .collect();
        bail!(
            "the model proposed no structure{}{}",
            if rejected.is_empty() {
                String::new()
            } else {
                format!(" ({})", rejected.join("; "))
            },
            if reply.text.is_empty() {
                String::new()
            } else {
                format!(": {}", reply.text.chars().take(300).collect::<String>())
            }
        );
    }

    // Heights are metres; footprints should be too. Where the frame is
    // unscaled page units and the model read a printed sheet scale, the
    // conversion is pure arithmetic on that printed number.
    let mut crs = anchor.crs.clone();
    let declared_scale = facts
        .as_ref()
        .and_then(|f| f["anchor_scale_denominator"].as_f64())
        .filter(|d| (10.0..=100_000.0).contains(d));
    if let Some(denominator) = declared_scale
        && !metric
    {
        // The scale is a model-read number, so geometry confirms it before
        // it rewrites every coordinate: at the right scale the chosen
        // building footprints come out building-sized. A misread magnitude
        // (1:50 for 1:500) makes them absurd, and the frame then honestly
        // stays in page units.
        let factor = denominator * 0.0254 / 72.0;
        let plausible = specs
            .iter()
            .filter(|spec| spec.kind == "block")
            .all(|spec| {
                let area = candidates[spec.candidate_id].area * factor * factor;
                (4.0..=20_000.0).contains(&area)
            });
        if plausible {
            scale_candidates(&mut candidates, denominator);
            crs = "local_metres".to_string();
        } else {
            warnings.push(format!(
                "printed scale 1:{denominator} makes the chosen footprints \
                 implausibly sized; keeping page units"
            ));
        }
    }

    // Sheets that specs name as exact-outline sources, serialized once.
    let mut outline_sources: std::collections::HashMap<usize, (Value, String)> =
        std::collections::HashMap::new();
    for spec in &specs {
        if let Some(page) = spec.outline_page
            && !outline_sources.contains_key(&page)
            && let Some(source) = vector.pages.iter().find(|p| p.page_number == page)
        {
            outline_sources.insert(
                page,
                (serde_json::to_value(&source.geojson)?, source.crs.clone()),
            );
        }
    }

    let mut parts = Vec::new();
    let mut features = Vec::new();
    let mut bodies = Vec::new();
    for spec in &specs {
        let outline = match resolve_outline(spec, &outline_sources, &candidates, placed) {
            Ok(outline) => outline,
            Err(reason) => {
                warnings.push(format!("{}: {reason}; using the candidate ring", spec.name));
                None
            }
        };
        let (part, mut part_features) = realize_part(spec, &candidates, placed, outline.as_ref());
        // The footprint actually extruded: the exact outline where one
        // resolved, the candidate ring otherwise.
        let extruded = outline
            .as_ref()
            .map_or(&candidates[spec.candidate_id].ring, |(ring, _)| ring);
        if let Some(ring) = ring_points(extruded) {
            bodies.push(Body {
                name: part.name.clone(),
                kind: spec.kind,
                ring,
            });
        }
        parts.push(part);
        features.append(&mut part_features);
    }

    // Measurements need metres: a page-units frame the printed scale could
    // not settle is reported as such rather than measured in nonsense.
    let (parcel, measurements) = if placed || crs == "local_metres" {
        match resolve_parcel(parcel_ring, &bodies, &candidates) {
            Some(parcel) => {
                let (collection, report) = measure::measure(&parcel, &bodies, placed);
                (Some(report), Some(collection))
            }
            None => {
                warnings.push(
                    "no parcel ring identified (no survey parcel on the anchor, no fence, \
                     no candidate enclosing every building); measurements omitted"
                        .to_string(),
                );
                (None, None)
            }
        }
    } else {
        warnings.push(format!("frame is {crs}, not metres; measurements omitted"));
        (None, None)
    };

    let collection = json!({
        "type": "FeatureCollection",
        "features": features,
        "metadata": {
            "document": vector.document,
            "model": client.model(),
            "crs": crs,
            "placed": placed,
            "anchor_page": selection.anchor,
            "pages_used": selection.all(),
            "note": "heights, colours and roof forms read by the model from the \
                     drawing set; footprints are extracted geometry",
        },
    });
    let geojson_file = output_dir
        .map(|dir| write_building(dir, &format!("{}.geojson", vector.document), &collection))
        .transpose()?;
    let measurements_file = match (output_dir, &measurements) {
        (Some(dir), Some(collection)) => Some(write_building(
            dir,
            &format!("{}.measurements.geojson", vector.document),
            collection,
        )?),
        _ => None,
    };

    Ok(BuildingReport {
        document: vector.document.clone(),
        source: vector.source.clone(),
        model: client.model().to_string(),
        crs,
        placed,
        anchor_page: selection.anchor,
        pages_used: selection.all(),
        parts,
        facts,
        warnings,
        geojson_file,
        geojson: collection,
        parcel,
        measurements_file,
        measurements,
    })
}

/// The parcel statement the anchor sheet carries: the surveyor's own ring
/// first, else the caller's polygon the sheet was placed from (`--parcel`).
/// Both the candidate flagging and the measurements read this one answer.
fn anchor_parcel(anchor_collection: &Value) -> Option<(&'static str, Vec<(f64, f64)>)> {
    ["survey_parcel", "reference_parcel"]
        .into_iter()
        .find_map(|kind| {
            anchor_collection["features"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|f| f["properties"]["kind"] == kind)
                .and_then(|f| f["geometry"]["coordinates"][0].as_array())
                .and_then(|ring| ring_points(ring))
                .map(|ring| (kind, ring))
        })
}

/// The parcel ring the measurements are taken against, in the anchor
/// frame: the anchor's own parcel statement; else the ring the declared
/// perimeter fence follows; else the largest candidate ring that contains
/// every building. Nothing is guessed beyond that.
fn resolve_parcel(
    parcel: Option<(&'static str, Vec<(f64, f64)>)>,
    bodies: &[Body],
    candidates: &[Candidate],
) -> Option<Parcel> {
    if let Some((source, ring)) = parcel {
        return Some(Parcel { source, ring });
    }
    if let Some(fence) = bodies.iter().find(|b| b.kind == "fence") {
        return Some(Parcel {
            source: "fence",
            ring: fence.ring.clone(),
        });
    }
    let blocks: Vec<&Body> = bodies.iter().filter(|b| b.kind == "block").collect();
    if blocks.is_empty() {
        return None;
    }
    // Candidates are largest-first; the first that holds every building's
    // centroid is the tightest enclosure worth calling the parcel — but
    // only if it is bigger than the buildings themselves.
    candidates
        .iter()
        .filter_map(|candidate| ring_points(&candidate.ring).map(|ring| (candidate, ring)))
        .find(|(candidate, ring)| {
            blocks.iter().all(|block| {
                measure::contains(ring, centroid(&block.ring))
                    && candidate.area > ring_area(&block.ring, false).max(1.0) * 1.5
            })
        })
        .map(|(_, ring)| Parcel {
            source: "enclosing_ring",
            ring,
        })
}

/// Which pages the model gets to see.
#[derive(Debug, PartialEq, Eq)]
struct Selection {
    /// The sheet footprints are extracted from: a placed page where one
    /// exists, else the best plan-view drawing.
    anchor: usize,
    /// Elevations, sections, floor plans, the roof plan, and the
    /// visualization sheets.
    info: Vec<usize>,
    /// No sheet is labelled as a render/photo montage, so the visualization
    /// section was not shown to the model. Pages are never uploaded on a
    /// guess — dossiers routinely end in identity-document scans.
    missing_renders: bool,
}

impl Selection {
    fn all(&self) -> Vec<usize> {
        let mut pages = vec![self.anchor];
        pages.extend(&self.info);
        pages
    }
}

/// Picks the anchor and info sheets from the classification, letting a
/// reviewed or vision-pass answers file refine what the heuristics saw.
fn select_pages(vector: &DocumentVector, answers: Option<&Answers>) -> Option<Selection> {
    let labelled = |number: usize, wanted: &[&str]| {
        answers
            .and_then(|a| a.pages.get(&number))
            .is_some_and(|answer| answer.labels.iter().any(|l| wanted.contains(&l.as_str())))
    };

    // A plan view: the sheets that draw footprints. A floor plan's forty
    // closed rings are rooms, not buildings, so a sheet the labels call a
    // floor plan is never a plan view however rich it looks.
    let plan_view = |p: &pdf_vector::PageVector| {
        !labelled(
            p.page_number,
            &["floor_plan", "elevation_or_section", "roof_plan"],
        ) && (matches!(
            p.classification.class,
            PageClass::SitePlan | PageClass::CadastralPlan | PageClass::TopographicSurvey
        ) || labelled(
            p.page_number,
            &["site_plan", "general_plan", "cadastral_plan"],
        ))
    };
    // Serialising a sheet and stitching its outlines is the expensive part;
    // once per page, then the ranking reads a number.
    let richness: std::collections::HashMap<usize, usize> = vector
        .pages
        .iter()
        .map(|p| (p.page_number, anchor_richness(p)))
        .collect();
    let rich = |p: &pdf_vector::PageVector| richness[&p.page_number];
    let placed_anchor = vector
        .pages
        .iter()
        .filter(|p| p.georeference.as_ref().is_some_and(|g| g.placed))
        .max_by_key(|p| {
            let geo = p.georeference.as_ref().expect("filtered on placed");
            let design_scale = geo.fitted_scale_denominator.is_some_and(|d| d <= 1000.0);
            let confident = geo.rmse_m.is_some_and(|r| r <= 0.05);
            // Richness ranks above the plan-view label: a floor-plan sheet
            // with a site inset (dossier 11) holds the house and the fence
            // where the site plan holds one ring. The label breaks ties —
            // forty rooms and forty house blocks look alike to a count —
            // and confidence ranks below both: a 0.4 m fit on the sheet
            // that draws the houses puts them 0.4 m off, where the
            // millimetre-fitted survey sheet draws no house at all.
            (
                design_scale,
                rich(p),
                plan_view(p),
                confident,
                p.page_number,
            )
        });
    let richest_plan = vector
        .pages
        .iter()
        .filter(|p| plan_view(p))
        .max_by_key(|p| (rich(p), p.page_number));
    // A placed sheet wins outright — unless it offers no building outline
    // at all (a survey sheet drawing the parcel and nothing else) while an
    // unplaced plan sheet offers some: a reconstruction in the sheet's own
    // metres is worth more than a georeferenced refusal.
    let anchor = match (placed_anchor, richest_plan) {
        (Some(placed), Some(plan)) if rich(placed) == 0 && rich(plan) > 0 => Some(plan),
        (Some(placed), _) => Some(placed),
        (None, plan) => plan,
    }
    .or_else(|| vector.pages.iter().find(|p| p.classification.is_drawing))?
    .page_number;

    let mut elevations = Vec::new();
    let mut floors = Vec::new();
    let mut extras = Vec::new();
    let mut renders = Vec::new();
    for page in &vector.pages {
        let n = page.page_number;
        if n == anchor {
            continue;
        }
        if page.classification.class == PageClass::ElevationOrSection
            || labelled(n, &["elevation_or_section"])
        {
            elevations.push(n);
        } else if page.classification.class == PageClass::FloorPlan || labelled(n, &["floor_plan"])
        {
            floors.push(n);
        } else if labelled(n, &["general_plan", "roof_plan"]) {
            extras.push(n);
        } else if labelled(n, &["photo_montage", "3d_view"]) {
            renders.push(n);
        }
    }

    let missing_renders = renders.is_empty();
    let mut info: Vec<usize> = Vec::new();
    let mut seen: BTreeSet<usize> = BTreeSet::from([anchor]);
    for n in elevations
        .into_iter()
        .take(3)
        .chain(floors.into_iter().take(2))
        .chain(extras.into_iter().take(1))
        .chain(renders)
    {
        if info.len() + 1 >= PAGE_BUDGET {
            break;
        }
        if seen.insert(n) {
            info.push(n);
        }
    }
    Some(Selection {
        anchor,
        info,
        missing_renders,
    })
}

/// How many building-sized outlines a sheet offers that are not the parcel
/// boundary — the measure that separates a proposal plan full of house
/// blocks from a survey sheet holding the parcel ring and nothing else.
fn anchor_richness(page: &pdf_vector::PageVector) -> usize {
    // A placed page is in degrees: counted as if it were metres, every
    // ring came out smaller than a square metre and no placed sheet ever
    // scored — which is how a table-less plan used to outrank the sheet
    // that was actually on the map.
    let geographic = page.crs == "EPSG:4326";
    let Ok(collection) = serde_json::to_value(&page.geojson) else {
        return 0;
    };
    let mut candidates = footprint_candidates(
        &collection,
        geographic,
        geographic || page.crs == "local_metres",
    );
    mark_parcel_candidates(
        &mut candidates,
        anchor_parcel(&collection).as_ref(),
        geographic,
    );
    candidates.iter().filter(|c| !c.is_parcel).count()
}

/// A closed outline on the anchor sheet that a structure could stand on.
struct Candidate {
    /// The closed ring, verbatim from the extracted feature.
    ring: Vec<Value>,
    area: f64,
    corners: usize,
    centre: (f64, f64),
    /// This ring is the parcel boundary itself (the survey or reference
    /// parcel the anchor carries, or a ring of the same size on the same
    /// spot): a fence may follow it, a building may not stand on it.
    is_parcel: bool,
}

/// Flags the candidates that are the parcel boundary: the survey or
/// reference parcel the anchor carries, and any ring of the same area
/// (±10 %) about the same centre — the drawn boundary the survey ring was
/// fitted to. Nothing is flagged on an unplaced sheet, which carries no
/// parcel statement of its own.
fn mark_parcel_candidates(
    candidates: &mut [Candidate],
    parcel: Option<&(&'static str, Vec<(f64, f64)>)>,
    geographic: bool,
) {
    let Some((_, parcel)) = parcel else {
        return;
    };
    let parcel: &[(f64, f64)] = parcel;
    let area = ring_area(parcel, geographic);
    // Area centroids, not vertex means: the reference ring carries the
    // cadastre's collinear vertices, and a vertex mean drifts metres away
    // from where the same boundary drawn with four corners sits.
    let centre = area_centroid(parcel);
    let (sx, sy) = measure::scale_at(centre.1, geographic);
    for candidate in candidates {
        let same_size = (candidate.area - area).abs() <= 0.1 * area;
        let own = ring_points(&candidate.ring).map(|ring| area_centroid(&ring));
        let offset = own.map_or(f64::INFINITY, |(x, y)| {
            ((x - centre.0) * sx).hypot((y - centre.1) * sy)
        });
        // A sheet draws the boundary simplified — four corners for the
        // cadastre's nine — so the centroids differ by metres on a parcel
        // twenty metres across. A quarter of the parcel's width is still
        // nowhere a same-sized building could stand beside it.
        candidate.is_parcel = same_size && offset <= 0.25 * area.sqrt();
    }
}

/// The centroid of a closed ring's area (shoelace), in the ring's own
/// frame; the vertex mean for a degenerate ring.
fn area_centroid(ring: &[(f64, f64)]) -> (f64, f64) {
    let mut twice_area = 0.0;
    let (mut cx, mut cy) = (0.0, 0.0);
    for pair in ring.windows(2) {
        let cross = pair[0].0 * pair[1].1 - pair[1].0 * pair[0].1;
        twice_area += cross;
        cx += (pair[0].0 + pair[1].0) * cross;
        cy += (pair[0].1 + pair[1].1) * cross;
    }
    if twice_area.abs() < 1e-12 {
        return centroid(ring);
    }
    (cx / (3.0 * twice_area), cy / (3.0 * twice_area))
}

/// Every closed ring on the sheet big enough to be a structure, largest
/// first. `geographic` says coordinates are degrees; `metric` says areas
/// come out in square metres. CAD exports routinely draw a building's
/// outline as loose polylines rather than one closed path, so open
/// polylines whose ends meet are stitched into rings first — the same
/// conservative chaining the exact-outline pass uses.
fn footprint_candidates(collection: &Value, geographic: bool, metric: bool) -> Vec<Candidate> {
    let join_tolerance = if geographic {
        5e-7
    } else if metric {
        0.05
    } else {
        0.75
    };
    let mut rings: Vec<Vec<(f64, f64)>> = Vec::new();
    let mut open: Vec<Vec<(f64, f64)>> = Vec::new();
    for feature in collection["features"]
        .as_array()
        .map_or(&[][..], Vec::as_slice)
    {
        let geometry = &feature["geometry"];
        let line = match geometry["type"].as_str() {
            Some("LineString") => geometry["coordinates"].as_array(),
            Some("Polygon") => geometry["coordinates"][0].as_array(),
            _ => None,
        };
        let Some(points) = line.and_then(|line| ring_points(line)) else {
            continue;
        };
        if points.len() >= 4 && points.first() == points.last() {
            rings.push(points);
        } else if points.len() >= 2 {
            open.push(points);
        }
    }
    rings.extend(stitch_polylines(open, join_tolerance));

    let mut candidates: Vec<Candidate> = rings
        .into_iter()
        .filter_map(|points| {
            let area = ring_area(&points, geographic);
            (area >= if metric { 4.0 } else { 1.0 }).then(|| Candidate {
                ring: points.iter().map(|&(x, y)| json!([x, y])).collect(),
                area,
                corners: points.len() - 1,
                centre: centroid(&points),
                is_parcel: false,
            })
        })
        .collect();
    candidates.sort_by(|a, b| b.area.total_cmp(&a.area));
    candidates.truncate(CANDIDATE_LIMIT);
    candidates
}

/// A copy of the ring pulled toward its centroid: by a scale `factor`
/// (roof steps), or by a fixed distance in metres (fence thickness) when
/// `metres` is given. Exact for convex rings, good enough at map scale for
/// the near-convex footprints permits draw.
fn inset_ring(ring: &[Value], factor: f64, metres: Option<(f64, bool)>) -> Vec<Value> {
    let Some(points) = ring_points(ring) else {
        return ring.to_vec();
    };
    let (cx, cy) = centroid(&points);
    points
        .iter()
        .map(|&(x, y)| {
            let (dx, dy) = (x - cx, y - cy);
            let (nx, ny) = match metres {
                None => (cx + dx * factor, cy + dy * factor),
                Some((distance, geographic)) => {
                    let (sx, sy) = measure::scale_at(cy, geographic);
                    let length = ((dx * sx).powi(2) + (dy * sy).powi(2)).sqrt().max(1e-9);
                    let shrink = (1.0 - distance / length).max(0.0);
                    (cx + dx * shrink, cy + dy * shrink)
                }
            };
            json!([round7(nx), round7(ny)])
        })
        .collect()
}

/// The largest closed outline a sheet draws — on a floor plan, the
/// exterior wall outline. CAD exports routinely emit an outline as loose
/// polylines rather than one closed path, so open polylines are stitched
/// end-to-end first (a junction with more than one continuation stops the
/// chain rather than guessing).
fn largest_closed_ring(collection: &Value, join_tolerance: f64) -> Option<Vec<(f64, f64)>> {
    let mut rings: Vec<Vec<(f64, f64)>> = Vec::new();
    let mut open: Vec<Vec<(f64, f64)>> = Vec::new();
    for feature in collection["features"].as_array()?.iter() {
        let geometry = &feature["geometry"];
        let line: &Vec<Value> = match geometry["type"].as_str() {
            Some("LineString") => match geometry["coordinates"].as_array() {
                Some(line) => line,
                None => continue,
            },
            Some("Polygon") => match geometry["coordinates"][0].as_array() {
                Some(ring) => ring,
                None => continue,
            },
            _ => continue,
        };
        let Some(points) = ring_points(line) else {
            continue;
        };
        if points.len() >= 4 && points.first() == points.last() {
            rings.push(points);
        } else if points.len() >= 2 {
            open.push(points);
        }
    }
    rings.extend(stitch_polylines(open, join_tolerance));
    rings
        .into_iter()
        .map(|points| {
            let area = ring_area(&points, false);
            (points, area)
        })
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(points, _)| points)
}

/// Chains loose polylines whose endpoints coincide into closed rings.
/// Greedy but conservative: a chain only extends where exactly one
/// continuation exists within the tolerance, so a T-junction ends a chain
/// instead of picking a branch.
fn stitch_polylines(mut open: Vec<Vec<(f64, f64)>>, tolerance: f64) -> Vec<Vec<(f64, f64)>> {
    let near = |a: (f64, f64), b: (f64, f64)| (a.0 - b.0).hypot(a.1 - b.1) <= tolerance;
    let mut rings = Vec::new();
    while let Some(mut chain) = open.pop() {
        loop {
            let tail = *chain.last().expect("chains are non-empty");
            let matches: Vec<usize> = open
                .iter()
                .enumerate()
                .filter(|(_, p)| near(tail, p[0]) || near(tail, *p.last().unwrap()))
                .map(|(i, _)| i)
                .collect();
            let [only] = matches[..] else {
                break;
            };
            let mut next = open.swap_remove(only);
            if !near(tail, next[0]) {
                next.reverse();
            }
            chain.extend(next.into_iter().skip(1));
        }
        if chain.len() >= 4 && near(chain[0], *chain.last().unwrap()) {
            let head = chain[0];
            *chain.last_mut().expect("non-empty") = head;
            rings.push(chain);
        }
    }
    rings
}

/// Centres a ring on its centroid and expresses it in metres. `geographic`
/// converts degrees equirectangularly about the ring's own latitude;
/// otherwise coordinates are taken as metres already (any needed sheet
/// scale is applied by the caller).
fn centre_in_metres(points: &[(f64, f64)], geographic: bool) -> (Vec<(f64, f64)>, (f64, f64)) {
    let origin = centroid(points);
    let (sx, sy) = measure::scale_at(origin.1, geographic);
    let centred = points
        .iter()
        .map(|&(x, y)| ((x - origin.0) * sx, (y - origin.1) * sy))
        .collect();
    (centred, origin)
}

/// Orientation of a centred ring's dominant axis, from the vertex
/// covariance. Stable for the elongated outlines buildings actually have;
/// a near-square outline is settled by the alignment score instead.
fn principal_angle(centred: &[(f64, f64)]) -> f64 {
    let (mut sxx, mut syy, mut sxy) = (0.0, 0.0, 0.0);
    for &(x, y) in centred {
        sxx += x * x;
        syy += y * y;
        sxy += x * y;
    }
    0.5 * (2.0 * sxy).atan2(sxx - syy)
}

fn rotate(points: &[(f64, f64)], angle: f64) -> Vec<(f64, f64)> {
    let (sin, cos) = angle.sin_cos();
    points
        .iter()
        .map(|&(x, y)| (x * cos - y * sin, x * sin + y * cos))
        .collect()
}

fn point_segment_distance(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let length_sq = dx * dx + dy * dy;
    let s = if length_sq <= 1e-12 {
        0.0
    } else {
        (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / length_sq).clamp(0.0, 1.0)
    };
    ((p.0 - a.0 - s * dx).powi(2) + (p.1 - a.1 - s * dy).powi(2)).sqrt()
}

/// Mean distance from `from`'s vertices to the nearest segment of `onto`.
fn mean_outline_distance(from: &[(f64, f64)], onto: &[(f64, f64)]) -> f64 {
    if from.is_empty() || onto.len() < 2 {
        return f64::INFINITY;
    }
    let total: f64 = from
        .iter()
        .map(|&p| {
            onto.windows(2)
                .map(|s| point_segment_distance(p, s[0], s[1]))
                .fold(f64::INFINITY, f64::min)
        })
        .sum();
    total / from.len() as f64
}

/// Rotates a centred outline onto a centred target; the score is how far
/// the target's corners sit from the outline (metres).
///
/// The principal axis and its three quarter-turns give the coarse
/// orientation; a near-square outline makes that axis unstable, so each
/// coarse candidate is refined by a local angle search before the best is
/// kept.
fn align_outline(outline: &[(f64, f64)], target: &[(f64, f64)]) -> (Vec<(f64, f64)>, f64) {
    let base = principal_angle(target) - principal_angle(outline);
    let score_at = |angle: f64| mean_outline_distance(target, &rotate(outline, angle));

    let mut best_angle = base;
    let mut best_score = f64::INFINITY;
    for quarter in 0..4 {
        let coarse = base + f64::from(quarter) * std::f64::consts::FRAC_PI_2;
        let mut local = (coarse, score_at(coarse));
        for step in [1.5_f64, 0.25] {
            let centre = local.0;
            for k in -8..=8 {
                let angle = centre + f64::from(k) * step.to_radians();
                let score = score_at(angle);
                if score < local.1 {
                    local = (angle, score);
                }
            }
        }
        if local.1 < best_score {
            (best_angle, best_score) = local;
        }
    }
    (rotate(outline, best_angle), best_score)
}

/// The exact drawn outline for a part, aligned onto its candidate's
/// position, or the reason the simplified candidate ring must stand in.
fn resolve_outline(
    spec: &PartSpec,
    sources: &std::collections::HashMap<usize, (Value, String)>,
    candidates: &[Candidate],
    geographic: bool,
) -> std::result::Result<Option<(Vec<Value>, f64)>, String> {
    let Some(page) = spec.outline_page else {
        return Ok(None);
    };
    if spec.kind == "fence" {
        return Err(format!(
            "a fence follows its boundary ring; outline_page {page} ignored"
        ));
    }
    let (collection, crs) = sources
        .get(&page)
        .ok_or_else(|| format!("outline page {page} has no extracted geometry"))?;
    let join_tolerance = match crs.as_str() {
        "local_metres" => 0.05,
        "page_units" => 0.75,
        _ => 5e-7,
    };
    let ring = largest_closed_ring(collection, join_tolerance)
        .ok_or_else(|| format!("outline page {page} draws no closed outline"))?;

    // The outline sheet's own frame, in metres.
    let outline_m = match crs.as_str() {
        "local_metres" => ring,
        "page_units" => {
            let scale = spec.outline_scale.ok_or_else(|| {
                format!("outline page {page} is in page units and no printed scale was reported")
            })?;
            let factor = scale * 0.0254 / 72.0;
            ring.iter()
                .map(|&(x, y)| (x * factor, y * factor))
                .collect()
        }
        _ => centre_in_metres(&ring, true).0,
    };
    let (outline_centred, _) = centre_in_metres(&outline_m, false);

    let candidate = &candidates[spec.candidate_id];
    let target = ring_points(&candidate.ring).ok_or("candidate ring is malformed")?;
    let (target_centred, target_origin) = centre_in_metres(&target, geographic);

    let outline_area = ring_area(&outline_centred, false);
    let target_area = ring_area(&target_centred, false);
    let ratio = outline_area / target_area.max(1e-9);
    if !(0.45..=2.2).contains(&ratio) {
        return Err(format!(
            "outline page {page}: drawn outline is {outline_area:.1} m² against a {target_area:.1} m² candidate — not the same building"
        ));
    }

    let (aligned, score) = align_outline(&outline_centred, &target_centred);
    let allowance = (0.12 * target_area.sqrt()).max(0.6);
    if score > allowance {
        return Err(format!(
            "outline page {page}: outline does not align with the candidate (mean corner distance {score:.2} m)"
        ));
    }

    // Back into the anchor's frame, about the candidate's position.
    let (sx, sy) = measure::scale_at(target_origin.1, geographic);
    let placed_ring: Vec<Value> = aligned
        .iter()
        .map(|&(x, y)| {
            json!([
                round7(target_origin.0 + x / sx),
                round7(target_origin.1 + y / sy)
            ])
        })
        .collect();
    Ok(Some((placed_ring, outline_area)))
}

fn add_part_tool() -> Tool {
    Tool {
        name: "add_part".into(),
        description: "Declare one built structure. Call once per structure, in \
                      parallel: each house block of a row development is its own \
                      call on its own footprint candidate; the perimeter fence or \
                      wall is kind 'fence' on the boundary ring it follows."
            .into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "name": {"type": "string",
                         "description": "short name, e.g. 'house V1', 'perimeter fence'"},
                "kind": {"type": "string", "enum": ["block", "fence"],
                         "description": "block = a building volume; fence = a thin \
                                         wall extruded along the candidate ring"},
                "footprint_candidate": {"type": "integer",
                                        "description": "id from the candidate list"},
                "height_m": {"type": "number",
                             "description": "top of the walls above ground (the eaves \
                                             for a pitched roof; the parapet for a flat \
                                             one), from the facade/section level marks"},
                "base_m": {"type": "number",
                           "description": "bottom above ground; 0 unless the part \
                                           starts above a lower part"},
                "floors": {"type": "integer"},
                "roof": {"type": "string", "enum": ["flat", "gabled", "hipped", "shed"],
                         "description": "flat when the renders show a parapet roof"},
                "ridge_m": {"type": "number",
                            "description": "top of a pitched roof above ground; \
                                            required when roof is not flat"},
                "color": {"type": "string",
                          "description": "facade colour from the 3D renders, #rrggbb"},
                "roof_color": {"type": "string", "description": "#rrggbb"},
                "outline_page": {"type": "integer",
                    "description": "page number of the sheet that draws this part's \
                                    EXACT exterior outline (the ground-floor plan); \
                                    the toolkit extracts that outline and aligns it \
                                    onto the chosen candidate's position"},
                "outline_scale_denominator": {"type": "number",
                    "description": "printed scale of that sheet, e.g. 100 for 1:100"}
            },
            "required": ["footprint_candidate", "height_m"],
        }),
    }
}

fn set_facts_tool() -> Tool {
    Tool {
        name: "set_facts".into(),
        description: "Report the indicator numbers the drawing set prints — \
                      building height, storeys, footprint/total/roof areas — \
                      exactly as printed. Call once."
            .into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "height_m": {"type": "number"},
                "floors": {"type": "integer"},
                "footprint_area_m2": {"type": "number"},
                "total_area_m2": {"type": "number"},
                "roof_area_m2": {"type": "number"},
                "buildings": {"type": "integer",
                              "description": "how many separate buildings the set shows"},
                "anchor_scale_denominator": {"type": "number",
                    "description": "the printed scale of the FIRST image (the anchor \
                                    sheet), e.g. 500 for a sheet titled 1:500"},
                "notes": {"type": "string"}
            },
        }),
    }
}

/// The subset of a `set_facts` call the report carries — a model inventing
/// extra fields does not get them into the contract.
fn known_facts(arguments: &Value) -> Value {
    let mut facts = serde_json::Map::new();
    for key in [
        "height_m",
        "floors",
        "footprint_area_m2",
        "total_area_m2",
        "roof_area_m2",
        "buildings",
        "anchor_scale_denominator",
        "notes",
    ] {
        if let Some(value) = arguments.get(key)
            && !value.is_null()
        {
            facts.insert(key.to_string(), value.clone());
        }
    }
    Value::Object(facts)
}

fn prompt(vector: &DocumentVector, selection: &Selection, candidates: &[Candidate]) -> String {
    let sheets: Vec<String> = selection
        .all()
        .iter()
        .map(|&n| {
            let class = vector
                .pages
                .iter()
                .find(|p| p.page_number == n)
                .map(|p| format!("{:?}", p.classification.class))
                .unwrap_or_default();
            format!("page {n} ({class})")
        })
        .collect();
    let listed: Vec<String> = candidates
        .iter()
        .enumerate()
        .map(|(id, c)| {
            format!(
                "candidate {id}: area {:.1}, {} corners, centre ({:.6}, {:.6}){}",
                c.area,
                c.corners,
                c.centre.0,
                c.centre.1,
                if c.is_parcel {
                    " — THE PARCEL BOUNDARY: a fence may follow it, no building stands on it"
                } else {
                    ""
                }
            )
        })
        .collect();
    format!(
        "You are reconstructing, for a 3D map, exactly what an Armenian \
         building-permit drawing set says the finished site must look like.\n\
         The images are, in order: {}. The first is the anchor sheet; the closed \
         outlines extracted from it are your footprint candidates:\n{}\n\n\
         The set's visualization section — development scheme, facades and \
         sections with level marks (e.g. +7.600), roof plan, 3D renders, an \
         indicators table — tells you the truth about the finished buildings. \
         Read it, then declare every built structure through parallel add_part \
         calls:\n\
         - one call per building volume; a row of identical houses is one call \
           per house, each on its own candidate ring (match them on the \
           development scheme);\n\
         - the perimeter fence or yard wall, when the set shows one, as kind \
           'fence' on the boundary ring it follows;\n\
         - height_m from the level marks (parapet top for flat roofs, eaves \
           for pitched ones, ridge_m for the ridge), never guessed from storey \
           counts when marks are printed;\n\
         - facade and roof colours as #rrggbb, read off the 3D renders;\n\
         - when one of the images is the ground-floor plan drawing the part's \
           exact exterior outline, pass outline_page (that image's page \
           number) and outline_scale_denominator (its printed scale): the \
           toolkit then extrudes the exact drawn outline, aligned onto the \
           candidate's position, instead of the simplified site-plan block.\n\
         Also call set_facts once with the printed indicator numbers, always \
         including anchor_scale_denominator — the scale printed on the first \
         image (e.g. 500 for 1:500).\n\
         Do not invent geometry: every structure stands on a candidate ring. \
         If no candidate matches a structure, leave that structure out and say \
         so in your text reply.",
        sheets.join(", "),
        listed.join("\n")
    )
}

/// The validated intent of one `add_part` call, before any geometry is
/// built — geometry waits until the anchor's units are settled.
struct PartSpec {
    name: String,
    kind: &'static str,
    candidate_id: usize,
    height_m: f64,
    base_m: f64,
    roof: Option<String>,
    ridge_m: Option<f64>,
    color: Option<String>,
    roof_color: Option<String>,
    floors: Option<u64>,
    /// The sheet drawing this part's exact exterior outline (a floor
    /// plan), and that sheet's printed scale.
    outline_page: Option<usize>,
    outline_scale: Option<f64>,
}

/// Validates one `add_part` call, or says why it cannot be used.
fn spec_from(
    arguments: &Value,
    candidates: &[Candidate],
    index: usize,
) -> std::result::Result<PartSpec, String> {
    let candidate_id = arguments["footprint_candidate"]
        .as_u64()
        .ok_or("no footprint_candidate")? as usize;
    if candidate_id >= candidates.len() {
        return Err(format!("candidate {candidate_id} does not exist"));
    }
    // The model picks the parcel for the house more often than not when the
    // parcel is the largest ring on the sheet; the rule is geometry's.
    if candidates[candidate_id].is_parcel && arguments["kind"].as_str() != Some("fence") {
        return Err(format!(
            "candidate {candidate_id} is the parcel boundary, not a building footprint"
        ));
    }
    let height_m = arguments["height_m"].as_f64().ok_or("no height_m")?;
    if !(height_m > 0.0 && height_m < 500.0) {
        return Err(format!("implausible height {height_m}"));
    }
    let base_m = arguments["base_m"].as_f64().unwrap_or(0.0).max(0.0);
    if base_m >= height_m {
        return Err(format!("base {base_m} at or above top {height_m}"));
    }
    let color = hex_color(&arguments["color"]);
    Ok(PartSpec {
        name: arguments["name"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| format!("part {}", index + 1)),
        kind: match arguments["kind"].as_str().unwrap_or("block") {
            "fence" => "fence",
            _ => "block",
        },
        candidate_id,
        height_m,
        base_m,
        roof: arguments["roof"]
            .as_str()
            .filter(|r| ["gabled", "hipped", "shed"].contains(r))
            .map(str::to_string),
        ridge_m: arguments["ridge_m"].as_f64().filter(|r| *r > height_m),
        roof_color: hex_color(&arguments["roof_color"]).or_else(|| color.clone()),
        color,
        floors: arguments["floors"].as_u64(),
        outline_page: arguments["outline_page"].as_u64().map(|n| n as usize),
        outline_scale: arguments["outline_scale_denominator"]
            .as_f64()
            .filter(|d| (10.0..=100_000.0).contains(d)),
    })
}

/// Turns one validated part into a report row and its GeoJSON features. A
/// `fence` becomes one ring polygon; a pitched roof adds stepped inset
/// features above the walls. Candidates are in their final units here, so
/// the fence thickness comes out in real metres wherever the frame is
/// metric.
fn realize_part(
    spec: &PartSpec,
    candidates: &[Candidate],
    geographic: bool,
    outline: Option<&(Vec<Value>, f64)>,
) -> (PartReport, Vec<Value>) {
    let candidate = &candidates[spec.candidate_id];
    let (ring, area) = match outline {
        Some((ring, area)) => (ring, *area),
        None => (&candidate.ring, candidate.area),
    };
    let properties = |height: f64, base: f64, colour: &Option<String>, role: &str| {
        let mut properties = json!({
            "name": spec.name,
            "kind": spec.kind,
            "role": role,
            "height": height,
            "base": base,
            "footprint_candidate": spec.candidate_id,
        });
        if let Some(colour) = colour {
            properties["color"] = colour.clone().into();
        }
        if let Some(floors) = spec.floors {
            properties["floors"] = floors.into();
        }
        properties
    };

    let mut features = Vec::new();
    if spec.kind == "fence" {
        // A filled parcel polygon would flood the yard; the wall is the
        // ring itself, drawn as outer ring + inset hole.
        let mut hole = inset_ring(&candidate.ring, 1.0, Some((FENCE_THICKNESS, geographic)));
        hole.reverse();
        features.push(json!({
            "type": "Feature",
            "geometry": {"type": "Polygon", "coordinates": [candidate.ring, hole]},
            "properties": properties(spec.height_m, spec.base_m, &spec.color, "walls"),
        }));
    } else {
        features.push(json!({
            "type": "Feature",
            "geometry": {"type": "Polygon", "coordinates": [ring]},
            "properties": properties(spec.height_m, spec.base_m, &spec.color, "walls"),
        }));
        if let (Some(_), Some(ridge)) = (&spec.roof, spec.ridge_m) {
            for (factor, rise) in ROOF_STEPS {
                let step_ring = inset_ring(ring, factor, None);
                let step_top = spec.height_m + (ridge - spec.height_m) * rise;
                features.push(json!({
                    "type": "Feature",
                    "geometry": {"type": "Polygon", "coordinates": [step_ring]},
                    "properties": properties(step_top, spec.height_m, &spec.roof_color, "roof"),
                }));
            }
        }
    }

    (
        PartReport {
            name: spec.name.clone(),
            kind: spec.kind.to_string(),
            height_m: spec.height_m,
            base_m: spec.base_m,
            ridge_m: spec.roof.as_ref().and(spec.ridge_m),
            roof: spec.roof.clone(),
            floors: spec.floors,
            color: spec.color.clone(),
            footprint_candidate: spec.candidate_id,
            footprint_area: (area * 10.0).round() / 10.0,
            outline_page: outline.and(spec.outline_page),
        },
        features,
    )
}

/// Multiplies every candidate into metres using a sheet scale the model
/// read off the anchor: one page unit at 1:N is N points of world, i.e.
/// `N * 0.0254 / 72` metres.
fn scale_candidates(candidates: &mut [Candidate], denominator: f64) {
    let factor = denominator * 0.0254 / 72.0;
    for candidate in candidates {
        candidate.ring = candidate
            .ring
            .iter()
            .filter_map(|point| {
                Some(json!([
                    round7(point[0].as_f64()? * factor),
                    round7(point[1].as_f64()? * factor)
                ]))
            })
            .collect();
        candidate.area *= factor * factor;
        candidate.centre = (candidate.centre.0 * factor, candidate.centre.1 * factor);
    }
}

/// A `#rrggbb` string, or nothing — a model's prose colour never reaches
/// the output.
fn hex_color(value: &Value) -> Option<String> {
    let text = value.as_str()?;
    let hex = text.strip_prefix('#')?;
    (hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| format!("#{}", hex.to_ascii_lowercase()))
}

fn write_building(dir: &Path, file_name: &str, collection: &Value) -> Result<String> {
    let subdir = dir.join("building");
    std::fs::create_dir_all(&subdir)
        .with_context(|| format!("cannot create output directory {}", subdir.display()))?;
    let name = format!("building/{file_name}");
    let path = dir.join(&name);
    let mut file = std::fs::File::create(&path)
        .with_context(|| format!("cannot create {}", path.display()))?;
    serde_json::to_writer_pretty(&mut file, collection)?;
    file.flush()?;
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 20 m x 10 m rectangle at Yerevan's latitude, drawn in degrees.
    fn rectangle() -> Vec<(f64, f64)> {
        let (lon, lat) = (44.5_f64, 40.09_f64);
        let (per_lon, per_lat) = measure::metres_per_degree(lat);
        let (dlon, dlat) = (20.0 / per_lon, 10.0 / per_lat);
        vec![
            (lon, lat),
            (lon + dlon, lat),
            (lon + dlon, lat + dlat),
            (lon, lat + dlat),
            (lon, lat),
        ]
    }

    fn ring(points: &[(f64, f64)]) -> Vec<Value> {
        points.iter().map(|(x, y)| json!([x, y])).collect()
    }

    fn one_candidate() -> Vec<Candidate> {
        vec![Candidate {
            ring: ring(&rectangle()),
            area: 200.0,
            corners: 4,
            centre: (44.5, 40.09),
            is_parcel: false,
        }]
    }

    #[test]
    fn geographic_ring_area_comes_out_in_square_metres() {
        let area = ring_area(&rectangle(), true);
        assert!((area - 200.0).abs() < 1.0, "20x10 m ≈ 200 m², got {area}");
        let local = ring_area(
            &[(0.0, 0.0), (4.0, 0.0), (4.0, 3.0), (0.0, 3.0), (0.0, 0.0)],
            false,
        );
        assert_eq!(local, 12.0);
    }

    #[test]
    fn candidates_keep_only_closed_structure_sized_rings_largest_first() {
        let collection = json!({"features": [
            {"geometry": {"type": "LineString", "coordinates": ring(&rectangle())}},
            {"geometry": {"type": "LineString",
                          "coordinates": [[44.5, 40.09], [44.6, 40.09]]}},
            {"geometry": {"type": "LineString",
                          "coordinates": [[44.5, 40.09], [44.500001, 40.09],
                                          [44.500001, 40.090001], [44.5, 40.09]]}},
        ]});
        let candidates = footprint_candidates(&collection, true, true);
        assert_eq!(
            candidates.len(),
            1,
            "open paths and door-sized rings are out"
        );
        assert_eq!(candidates[0].corners, 4);
        assert!((candidates[0].area - 200.0).abs() < 1.0);
    }

    fn realize(arguments: Value, candidates: &[Candidate]) -> (PartReport, Vec<Value>) {
        let spec = spec_from(&arguments, candidates, 0).expect("a sound part");
        realize_part(&spec, candidates, true, None)
    }

    #[test]
    fn a_block_extrudes_its_candidate_with_colour_and_refuses_nonsense() {
        let candidates = one_candidate();
        let (part, features) = realize(
            json!({"footprint_candidate": 0, "height_m": 7.6, "floors": 2,
                    "roof": "flat", "color": "#F2F2EF", "name": "house V1"}),
            &candidates,
        );
        assert_eq!(
            (part.kind.as_str(), part.name.as_str()),
            ("block", "house V1")
        );
        assert_eq!((part.height_m, part.base_m), (7.6, 0.0));
        assert_eq!(part.color.as_deref(), Some("#f2f2ef"), "normalised hex");
        assert!(part.roof.is_none(), "flat roof adds nothing");
        assert_eq!(features.len(), 1);
        assert_eq!(features[0]["properties"]["height"], 7.6);
        assert_eq!(features[0]["properties"]["color"], "#f2f2ef");

        for bad in [
            json!({"footprint_candidate": 9, "height_m": 12.0}),
            json!({"footprint_candidate": 0, "height_m": -3.0}),
            json!({"footprint_candidate": 0, "height_m": 10.0, "base_m": 11.0}),
            json!({"height_m": 10.0}),
        ] {
            assert!(spec_from(&bad, &candidates, 0).is_err());
        }
    }

    #[test]
    fn a_pitched_roof_becomes_stepped_insets_above_the_eaves() {
        let (part, features) = realize(
            json!({"footprint_candidate": 0, "height_m": 5.5, "roof": "hipped",
                    "ridge_m": 8.0, "roof_color": "#803020"}),
            &one_candidate(),
        );
        assert_eq!(part.roof.as_deref(), Some("hipped"));
        assert_eq!(part.ridge_m, Some(8.0));
        assert_eq!(features.len(), 3, "walls + two roof steps");
        let step1 = &features[1]["properties"];
        let step2 = &features[2]["properties"];
        assert_eq!(step1["base"], 5.5);
        assert!((step1["height"].as_f64().unwrap() - 6.875).abs() < 1e-9);
        assert_eq!(step2["height"], 8.0);
        assert_eq!(step2["color"], "#803020");
        assert_eq!(step2["role"], "roof");
    }

    #[test]
    fn a_fence_is_a_thin_ring_not_a_filled_parcel() {
        let (part, features) = realize(
            json!({"footprint_candidate": 0, "kind": "fence", "height_m": 2.4,
                    "name": "perimeter fence", "color": "#c8c8c4"}),
            &one_candidate(),
        );
        assert_eq!(part.kind, "fence");
        let rings = features[0]["geometry"]["coordinates"].as_array().unwrap();
        assert_eq!(rings.len(), 2, "outer ring plus inset hole");
        let outer = ring_points(rings[0].as_array().unwrap()).unwrap();
        let hole = ring_points(rings[1].as_array().unwrap()).unwrap();
        let wall_area = ring_area(&outer, true) - ring_area(&hole, true);
        assert!(
            (10.0..30.0).contains(&wall_area),
            "a 0.3 m wall around 20x10 m is ~17.6 m², got {wall_area}"
        );
    }

    #[test]
    fn a_printed_sheet_scale_turns_page_units_into_metres() {
        // A 40 x 34.5 page-unit rectangle at 1:500 is 7.06 x 6.09 m — the
        // typical-house footprint of the corpus's three-house dossier.
        let mut candidates = vec![Candidate {
            ring: ring(&[
                (0.0, 0.0),
                (40.0, 0.0),
                (40.0, 34.5),
                (0.0, 34.5),
                (0.0, 0.0),
            ]),
            area: 1380.0,
            corners: 4,
            centre: (20.0, 17.25),
            is_parcel: false,
        }];
        scale_candidates(&mut candidates, 500.0);
        let factor = 500.0 * 0.0254 / 72.0;
        assert!((candidates[0].area - 1380.0 * factor * factor).abs() < 1e-6);
        let first = candidates[0].ring[1][0].as_f64().unwrap();
        assert!(
            (first - 40.0 * factor).abs() < 1e-6,
            "coordinates scale too"
        );
        assert!(
            (42.0..44.0).contains(&candidates[0].area),
            "the house comes out house-sized: {} m²",
            candidates[0].area
        );
    }

    /// An asymmetric 6-corner outline in metres: a 10x6 block with a 4x2
    /// porch jutting from one corner.
    fn l_outline() -> Vec<(f64, f64)> {
        vec![
            (0.0, 0.0),
            (10.0, 0.0),
            (10.0, 6.0),
            (4.0, 6.0),
            (4.0, 8.0),
            (0.0, 8.0),
            (0.0, 0.0),
        ]
    }

    #[test]
    fn an_outline_aligns_onto_a_rotated_translated_copy_of_itself() {
        let (outline, _) = centre_in_metres(&l_outline(), false);
        let target_points: Vec<(f64, f64)> = rotate(&outline, 0.6)
            .iter()
            .map(|&(x, y)| (x + 40.0, y - 25.0))
            .collect();
        let (target_centred, _) = centre_in_metres(&target_points, false);
        let (aligned, score) = align_outline(&outline, &target_centred);
        assert!(score < 0.01, "an exact copy aligns exactly, got {score}");
        assert!(
            mean_outline_distance(&aligned, &target_centred) < 0.01,
            "and the aligned outline lies on the target"
        );
    }

    #[test]
    fn resolve_outline_scales_aligns_and_guards() {
        // The outline sheet draws the exact shape at 1:100 in page units;
        // the candidate is the same shape, simplified to its bounding
        // block, rotated 30 degrees, in local metres.
        let factor = 100.0 * 0.0254 / 72.0;
        let sheet_ring: Vec<Value> = l_outline()
            .iter()
            .map(|&(x, y)| json!([x / factor, y / factor]))
            .collect();
        let sources = std::collections::HashMap::from([(
            12_usize,
            (
                json!({"features": [
                    {"geometry": {"type": "LineString", "coordinates": sheet_ring}}]}),
                "page_units".to_string(),
            ),
        )]);
        let block: Vec<(f64, f64)> = rotate(
            &centre_in_metres(
                &[(0.0, 0.0), (10.0, 0.0), (10.0, 8.0), (0.0, 8.0), (0.0, 0.0)],
                false,
            )
            .0,
            std::f64::consts::FRAC_PI_6,
        )
        .iter()
        .map(|&(x, y)| (x + 100.0, y + 60.0))
        .collect();
        // `block` already carries its closing point through centre/rotate.
        let block_ring: Vec<Value> = block.iter().map(|&(x, y)| json!([x, y])).collect();
        let candidates = vec![Candidate {
            ring: block_ring,
            area: 80.0,
            corners: 4,
            centre: (100.0, 60.0),
            is_parcel: false,
        }];
        let spec = |page: Option<usize>, scale: Option<f64>| PartSpec {
            name: "house".into(),
            kind: "block",
            candidate_id: 0,
            height_m: 7.6,
            base_m: 0.0,
            roof: None,
            ridge_m: None,
            color: None,
            roof_color: None,
            floors: None,
            outline_page: page,
            outline_scale: scale,
        };

        let (ring, area) =
            resolve_outline(&spec(Some(12), Some(100.0)), &sources, &candidates, false)
                .expect("resolves")
                .expect("an outline");
        assert!(
            (area - 68.0).abs() < 0.5,
            "the exact outline's area, not the block's: {area}"
        );
        let placed = ring_points(&ring).unwrap();
        let (centred, origin) = centre_in_metres(&placed, false);
        assert!(
            (origin.0 - 100.0).abs() < 0.5 && (origin.1 - 60.0).abs() < 0.5,
            "placed at the candidate's position: {origin:?}"
        );
        assert_eq!(centred.len(), 7, "all six corners plus the closure survive");

        // Guards: no reported scale on a page-units sheet; a sheet whose
        // outline is a different building; no outline page at all.
        assert!(resolve_outline(&spec(Some(12), None), &sources, &candidates, false).is_err());
        let tiny = vec![Candidate {
            ring: (0..5)
                .map(|i| json!([f64::from(i % 2) * 3.0, f64::from(i / 2 % 2) * 3.0]))
                .collect(),
            area: 9.0,
            corners: 4,
            centre: (1.5, 1.5),
            is_parcel: false,
        }];
        assert!(resolve_outline(&spec(Some(12), Some(100.0)), &sources, &tiny, false).is_err());
        assert!(
            resolve_outline(&spec(None, None), &sources, &candidates, false)
                .expect("no outline requested")
                .is_none()
        );
    }

    #[test]
    fn loose_polylines_stitch_into_the_outline_a_closed_path_never_drew() {
        // An exterior outline drawn as three loose polylines, the way CAD
        // exports emit walls — plus a stray mark that must not join.
        let collection = json!({"features": [
            {"geometry": {"type": "LineString",
                          "coordinates": [[0.0, 0.0], [10.0, 0.0], [10.0, 6.0]]}},
            {"geometry": {"type": "LineString",
                          "coordinates": [[4.0, 8.0], [4.0, 6.0], [10.0, 6.0]]}},
            {"geometry": {"type": "LineString",
                          "coordinates": [[0.0, 0.0], [0.0, 8.0], [4.0, 8.0]]}},
            {"geometry": {"type": "LineString",
                          "coordinates": [[50.0, 50.0], [51.0, 50.0]]}},
        ]});
        let ring = largest_closed_ring(&collection, 0.05).expect("a stitched ring");
        assert_eq!(ring.first(), ring.last(), "the ring closes");
        assert!(
            (ring_area(&ring, false) - 68.0).abs() < 1e-6,
            "the L-outline's area"
        );
    }

    #[test]
    fn a_junction_with_two_continuations_ends_the_chain() {
        // The tail at (10,0) could continue into either branch — a guess
        // would fabricate geometry, so the chain must stop and no ring form.
        let open = vec![
            vec![(0.0, 0.0), (10.0, 0.0)],
            vec![(10.0, 0.0), (10.0, 6.0)],
            vec![(10.0, 0.0), (14.0, 0.0)],
        ];
        assert!(stitch_polylines(open, 0.05).is_empty());
    }

    #[test]
    fn selection_reports_missing_render_labels_instead_of_guessing_pages() {
        // No answers at all: nothing is labelled photo_montage/3d_view, so
        // no page may be uploaded on a guess — the flag says so.
        let vector = DocumentVector {
            document: "t".into(),
            source: "t.pdf".into(),
            page_count: 9,
            pages: Vec::new(),
            warnings: Vec::new(),
        };
        assert!(
            select_pages(&vector, None).is_none(),
            "no drawing, no anchor"
        );
    }

    #[test]
    fn the_parcel_boundary_is_flagged_and_refused_as_a_building() {
        // The reference parcel with an extra collinear vertex, and the
        // same boundary drawn with four corners: one ring, two vertex
        // means metres apart, one area centroid.
        let mut parcel = rectangle();
        let mid = ((parcel[0].0 + parcel[1].0) / 2.0, parcel[0].1);
        parcel.insert(1, mid);
        let collection = json!({"features": [
            {"geometry": {"type": "Polygon", "coordinates": [ring(&parcel)]},
             "properties": {"kind": "reference_parcel"}},
        ]});
        let mut candidates = one_candidate();
        mark_parcel_candidates(&mut candidates, anchor_parcel(&collection).as_ref(), true);
        assert!(candidates[0].is_parcel, "the drawn boundary is the parcel");

        let house_on_parcel = json!({"footprint_candidate": 0, "height_m": 7.6});
        assert!(
            spec_from(&house_on_parcel, &candidates, 0).is_err(),
            "a building cannot stand on the parcel ring"
        );
        let fence = json!({"footprint_candidate": 0, "kind": "fence", "height_m": 2.0});
        assert!(
            spec_from(&fence, &candidates, 0).is_ok(),
            "a fence follows it"
        );
    }

    #[test]
    fn facts_keep_only_the_known_indicator_fields() {
        let facts = known_facts(&json!({
            "height_m": 7.6, "floors": 2, "buildings": 3,
            "roof_area_m2": 66.76, "notes": "three identical houses",
            "invented_field": "junk", "footprint_area_m2": null,
        }));
        assert_eq!(facts["height_m"], 7.6);
        assert_eq!(facts["buildings"], 3);
        assert!(facts.get("invented_field").is_none());
        assert!(facts.get("footprint_area_m2").is_none(), "null is absence");
    }
}
