//! CAD refinement: a hosted model proposes fixes to recovered vector
//! geometry, and the fixes are applied deterministically.
//!
//! The model never draws. It sees the rendered sheet next to an inventory of
//! the exact extracted features and proposes *operations* — assign features
//! to a layer, drop plotting noise, close a nearly-closed ring — through
//! parallel tool calls, so one request carries the whole cleanup. Geometry
//! stays the geometry the interpreter recovered; a proposal referencing a
//! feature that does not exist is a warning, not a change.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use pdfium_render::prelude::Pdfium;
use serde::Serialize;
use serde_json::Value;

use llm_providers::{Client, Effort, Request, Tool, ToolCall};
use pdf_core::page::open_document;
use pdf_vector::{VectorOptions, extract_document};

use crate::DRAWING_RENDER_WIDTH_PX;

/// How many features the inventory names individually. A drawing sheet can
/// carry thousands of hatching strokes; past this the rest are summarised
/// and left untouched rather than ballooning the prompt.
const INVENTORY_LIMIT: usize = 400;

/// What the refinement did, plus the refined collection itself.
#[derive(Debug, Serialize)]
pub struct CadFixReport {
    pub document: String,
    pub source: String,
    pub page: usize,
    pub model: String,
    /// The frame the geometry is written in, verbatim from `vector`.
    pub crs: String,
    pub placed: bool,
    pub features_before: usize,
    pub features_after: usize,
    /// Features assigned a `layer` property, per layer.
    pub layers: BTreeMap<String, usize>,
    pub closed: usize,
    pub dropped: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// Written under `--output` as `<document>_pNN.fixed.geojson`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub geojson_file: Option<String>,
    /// The refined FeatureCollection, inline — same policy as `vector`.
    pub geojson: Value,
}

/// Renders one page, extracts its vector geometry, asks the model for fixes,
/// and applies them. Progress goes to stderr.
pub fn fix_page(
    client: &Client,
    pdfium: &Pdfium,
    pdf_path: &Path,
    page_number: usize,
    output_dir: Option<&Path>,
) -> Result<CadFixReport> {
    let vector = extract_document(pdf_path, &[page_number], &VectorOptions::default())?;
    let page = vector
        .pages
        .first()
        .ok_or_else(|| anyhow!("page {page_number} has no readable content"))?;
    let mut collection = serde_json::to_value(&page.geojson)?;
    let features_before = features_of_ref(&collection).len();
    if features_before == 0 {
        anyhow::bail!("page {page_number} carries no vector features to refine");
    }

    eprintln!(
        "cadfix: page {page_number} ({features_before} features) -> {}",
        client.model()
    );
    let document = open_document(pdfium, pdf_path).map_err(anyhow::Error::from)?;
    let png = crate::render_page_png(document.pages(), page_number, DRAWING_RENDER_WIDTH_PX)?;

    let mut request = Request::new(prompt(&collection));
    request.images_png.push(png);
    request.max_output_tokens = 16_000;
    request.effort = Some(Effort::Medium);
    request.tools = tools();
    let reply = client
        .ask(&request)
        .with_context(|| format!("CAD refinement request to '{}' failed", client.model()))?;

    let mut outcome = apply_calls(&mut collection, &reply.tool_calls);
    outcome.warnings.extend(vector.warnings.iter().cloned());
    let placed = page.georeference.as_ref().is_some_and(|g| g.placed);
    let geojson_file = output_dir
        .map(|dir| write_fixed(dir, &vector.document, page_number, &collection))
        .transpose()?;

    Ok(CadFixReport {
        document: vector.document.clone(),
        source: vector.source.clone(),
        page: page_number,
        model: client.model().to_string(),
        crs: page.crs.clone(),
        placed,
        features_before,
        features_after: features_of_ref(&collection).len(),
        layers: outcome.layers,
        closed: outcome.closed,
        dropped: outcome.dropped,
        warnings: outcome.warnings,
        geojson_file,
        geojson: collection,
    })
}

/// The operations the model may propose.
fn tools() -> Vec<Tool> {
    let ids = serde_json::json!({"type": "array", "items": {"type": "integer"}});
    vec![
        Tool {
            name: "set_layer".into(),
            description: "Assign the listed feature ids to one layer. Layers: \
                          parcel_boundary, building, walls, openings, roof, annotation, \
                          dimensions, grid, terrain, utilities, furniture, other."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"layer": {"type": "string"}, "ids": ids},
                "required": ["layer", "ids"],
            }),
        },
        Tool {
            name: "drop_features".into(),
            description: "Remove the listed feature ids: plotting noise, stray marks, \
                          duplicated strokes — anything that is not drawing content."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"ids": ids, "reason": {"type": "string"}},
                "required": ["ids"],
            }),
        },
        Tool {
            name: "close_rings".into(),
            description: "Close the listed features: outlines that are visibly closed \
                          shapes on the sheet but whose extracted path does not quite \
                          return to its start."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"ids": ids},
                "required": ["ids"],
            }),
        },
    ]
}

fn prompt(collection: &Value) -> String {
    format!(
        "You are refining CAD geometry recovered from one sheet of an Armenian \
         building-permit drawing set. The image is the rendered sheet; below is an \
         inventory of the extracted vector features, each with an id.\n\
         Propose fixes ONLY through the provided tools, calling them in parallel:\n\
         - set_layer: group features into semantic layers\n\
         - drop_features: remove noise that is not drawing content\n\
         - close_rings: close outlines that clearly should be closed shapes\n\
         Do not invent geometry. If a feature is ambiguous, leave it alone.\n\n{}",
        inventory(collection)
    )
}

/// One line per feature: what the model needs to reason about a stroke,
/// nothing more.
fn inventory(collection: &Value) -> String {
    let features = features_of_ref(collection);
    let mut lines = Vec::with_capacity(features.len().min(INVENTORY_LIMIT) + 1);
    for (id, feature) in features.iter().take(INVENTORY_LIMIT).enumerate() {
        let geometry = &feature["geometry"];
        let kind = geometry["type"].as_str().unwrap_or("?");
        // A Polygon's coordinates are its RINGS; counting them as points
        // would describe every closed boundary as "1 points, closed=false"
        // — the exact profile the prompt says to drop as noise.
        let ring = match kind {
            "Polygon" => geometry["coordinates"][0].as_array(),
            _ => geometry["coordinates"].as_array(),
        };
        let points = ring.map_or(0, Vec::len);
        let closed = kind == "Polygon"
            || ring
                .and_then(|ring| Some((ring.first()?, ring.last()?)))
                .is_some_and(|(first, last)| first == last && points > 3);
        let length = feature["properties"]["length_m"]
            .as_f64()
            .map(|m| format!(", length {m} m"))
            .unwrap_or_default();
        let stroke = feature["properties"]["stroke"].as_str().unwrap_or("?");
        lines.push(format!(
            "id {id}: {kind}, {points} points, closed={closed}{length}, stroke {stroke}"
        ));
    }
    if features.len() > INVENTORY_LIMIT {
        lines.push(format!(
            "... and {} further features not inventoried; leave them alone.",
            features.len() - INVENTORY_LIMIT
        ));
    }
    lines.join("\n")
}

struct FixOutcome {
    layers: BTreeMap<String, usize>,
    closed: usize,
    dropped: usize,
    warnings: Vec<String>,
}

/// Applies the model's proposals to the collection. A call naming an unknown
/// tool, malformed arguments, or an out-of-range id degrades to a warning —
/// one bad proposal must not cost the rest of the refinement.
fn apply_calls(collection: &mut Value, calls: &[ToolCall]) -> FixOutcome {
    let mut layer_by_id: HashMap<usize, String> = HashMap::new();
    let mut to_close: BTreeSet<usize> = BTreeSet::new();
    let mut to_drop: BTreeSet<usize> = BTreeSet::new();
    let mut warnings = Vec::new();
    // Only what the inventory actually described is actionable: features
    // past the cap were promised to be left alone, so an id in that range
    // is a hallucination, not an instruction.
    let count = features_of_ref(collection).len().min(INVENTORY_LIMIT);

    for call in calls {
        let ids: Vec<usize> = call.arguments["ids"]
            .as_array()
            .map(|ids| {
                ids.iter()
                    .filter_map(Value::as_u64)
                    .map(|id| id as usize)
                    .collect()
            })
            .unwrap_or_default();
        let (known, unknown): (Vec<usize>, Vec<usize>) = ids.iter().partition(|&&id| id < count);
        if !unknown.is_empty() {
            warnings.push(format!("{}: ignored unknown ids {unknown:?}", call.name));
        }
        match call.name.as_str() {
            "set_layer" => match call.arguments["layer"].as_str() {
                Some(layer) => {
                    for id in known {
                        layer_by_id.insert(id, layer.to_string());
                    }
                }
                None => warnings.push("set_layer: call without a layer ignored".into()),
            },
            "drop_features" => to_drop.extend(known),
            "close_rings" => to_close.extend(known),
            other => warnings.push(format!("ignored unknown tool call '{other}'")),
        }
    }

    let mut layers: BTreeMap<String, usize> = BTreeMap::new();
    let mut closed = 0;
    if let Some(features) = features_of(collection) {
        for (id, feature) in features.iter_mut().enumerate() {
            if let Some(layer) = layer_by_id.get(&id) {
                feature["properties"]["layer"] = Value::String(layer.clone());
                *layers.entry(layer.clone()).or_default() += 1;
            }
            if to_close.contains(&id) && close_ring(feature) {
                closed += 1;
            }
        }
        let before = features.len();
        let mut id = 0;
        features.retain(|_| {
            let keep = !to_drop.contains(&id);
            id += 1;
            keep
        });
        let dropped = before - features.len();
        return FixOutcome {
            layers,
            closed,
            dropped,
            warnings,
        };
    }
    FixOutcome {
        layers,
        closed,
        dropped: 0,
        warnings,
    }
}

/// Closes an open LineString by returning to its first point. Already-closed
/// rings and degenerate paths are left as they are.
fn close_ring(feature: &mut Value) -> bool {
    let geometry = &mut feature["geometry"];
    if geometry["type"].as_str() != Some("LineString") {
        return false;
    }
    let Some(ring) = geometry["coordinates"].as_array_mut() else {
        return false;
    };
    if ring.len() < 3 || ring.first() == ring.last() {
        return false;
    }
    let first = ring[0].clone();
    ring.push(first);
    true
}

fn features_of(collection: &mut Value) -> Option<&mut Vec<Value>> {
    collection["features"].as_array_mut()
}

fn features_of_ref(collection: &Value) -> &[Value] {
    collection["features"].as_array().map_or(&[], Vec::as_slice)
}

fn write_fixed(
    dir: &Path,
    document: &str,
    page_number: usize,
    collection: &Value,
) -> Result<String> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("cannot create output directory {}", dir.display()))?;
    let name = format!("{document}_p{page_number:02}.fixed.geojson");
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
    use serde_json::json;

    fn collection() -> Value {
        json!({"type": "FeatureCollection", "features": [
            {"type": "Feature",
             "geometry": {"type": "LineString",
                          "coordinates": [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]]},
             "properties": {"stroke": "#000000", "length_m": 3.0}},
            {"type": "Feature",
             "geometry": {"type": "LineString",
                          "coordinates": [[5.0, 5.0], [5.1, 5.1]]},
             "properties": {"stroke": "#ff0000"}},
        ]})
    }

    fn call(name: &str, arguments: Value) -> ToolCall {
        ToolCall {
            name: name.into(),
            arguments,
        }
    }

    #[test]
    fn proposals_apply_and_the_geometry_is_only_ever_edited_deterministically() {
        let mut c = collection();
        let outcome = apply_calls(
            &mut c,
            &[
                call("set_layer", json!({"layer": "walls", "ids": [0]})),
                call("close_rings", json!({"ids": [0]})),
                call("drop_features", json!({"ids": [1], "reason": "stray mark"})),
            ],
        );
        assert_eq!(outcome.layers["walls"], 1);
        assert_eq!((outcome.closed, outcome.dropped), (1, 1));
        assert!(outcome.warnings.is_empty());

        let features = c["features"].as_array().unwrap();
        assert_eq!(features.len(), 1);
        assert_eq!(features[0]["properties"]["layer"], "walls");
        let ring = features[0]["geometry"]["coordinates"].as_array().unwrap();
        assert_eq!(ring.first(), ring.last(), "the ring returned to its start");
        assert_eq!(ring.len(), 4);
    }

    #[test]
    fn bad_proposals_degrade_to_warnings_not_changes() {
        let mut c = collection();
        let outcome = apply_calls(
            &mut c,
            &[
                call("set_layer", json!({"ids": [0]})),
                call("drop_features", json!({"ids": [7]})),
                call("bulldoze", json!({})),
                call("close_rings", json!({"ids": [1]})),
            ],
        );
        assert_eq!(outcome.warnings.len(), 3);
        assert_eq!(
            (outcome.closed, outcome.dropped),
            (0, 0),
            "a 2-point path cannot close"
        );
        assert_eq!(c["features"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn the_inventory_names_each_feature_once_and_caps_itself() {
        let text = inventory(&collection());
        assert!(
            text.contains("id 0: LineString, 3 points, closed=false, length 3 m, stroke #000000")
        );
        assert!(text.contains("id 1:"));
        assert!(!text.contains("further features"));
    }

    #[test]
    fn a_polygon_is_inventoried_by_its_outer_ring_and_reads_closed() {
        let c = json!({"type": "FeatureCollection", "features": [
            {"type": "Feature",
             "geometry": {"type": "Polygon",
                          "coordinates": [[[0.0, 0.0], [4.0, 0.0], [4.0, 3.0], [0.0, 0.0]]]},
             "properties": {"stroke": "#000000"}},
        ]});
        let text = inventory(&c);
        assert!(
            text.contains("id 0: Polygon, 4 points, closed=true"),
            "a parcel boundary must not read as '1 points, closed=false': {text}"
        );
    }

    #[test]
    fn ids_past_the_inventory_cap_are_hallucinations_not_instructions() {
        let features: Vec<Value> = (0..INVENTORY_LIMIT + 50)
            .map(|_| {
                json!({"type": "Feature",
                       "geometry": {"type": "LineString",
                                    "coordinates": [[0.0, 0.0], [1.0, 1.0]]},
                       "properties": {}})
            })
            .collect();
        let mut c = json!({"type": "FeatureCollection", "features": features});
        let outcome = apply_calls(
            &mut c,
            &[call(
                "drop_features",
                json!({"ids": [INVENTORY_LIMIT + 10]}),
            )],
        );
        assert_eq!(outcome.dropped, 0, "uninventoried geometry stays untouched");
        assert_eq!(outcome.warnings.len(), 1);
        assert_eq!(
            c["features"].as_array().unwrap().len(),
            INVENTORY_LIMIT + 50
        );
    }

    #[test]
    fn already_closed_rings_are_left_alone() {
        let mut feature = json!({"geometry": {"type": "LineString",
            "coordinates": [[0, 0], [1, 0], [1, 1], [0, 0]]}});
        assert!(!close_ring(&mut feature));
    }
}
