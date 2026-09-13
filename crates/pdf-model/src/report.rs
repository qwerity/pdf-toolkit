//! What the `model` command prints: the parameters it ran on and where
//! they came from, how each sheet was read, per-storey counts, the
//! placement (or its absence), the files written, and the model and its
//! extruded GeoJSON inline — the same inline policy as `vector` and
//! `building`, so a caller needs no second file to draw it.

use geojson::FeatureCollection;
use serde::Serialize;
use serde_json::Value;

use crate::SheetReport;
use crate::geom::round;
use crate::model::Model;
use crate::params::Params;

/// One storey's counts, for a caller that wants the summary without the
/// polygons.
#[derive(Debug, Clone, Serialize)]
pub struct FloorSummary {
    pub name: String,
    pub page: usize,
    pub level_m: f64,
    pub top_m: f64,
    pub walls: usize,
    pub glazing: usize,
    pub door_gaps: usize,
    pub rooms: usize,
    /// Sum of the room fills' areas, m².
    pub rooms_area_m2: f64,
    pub slab_area_m2: f64,
    pub rails: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct RoofSummary {
    pub page: usize,
    pub area_m2: f64,
    pub parapet_pieces: usize,
}

/// The stdout contract of `model`.
#[derive(Debug, Serialize)]
pub struct ModelReport {
    pub document: String,
    pub source: String,
    /// The parameters the model was built from; `source` says whether they
    /// were detected, loaded, or proposed by a model.
    pub params: Params,
    /// How each parameter was settled, one line each. Absent for a
    /// parameters file, which needs no explaining.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub params_notes: Vec<String>,
    /// The frame `geojson` and `model` coordinates are in: `EPSG:4326`
    /// once placed, `local_metres` otherwise. `model` is always in local
    /// metres.
    pub crs: String,
    pub placed: bool,
    /// How the model was placed. Absent when it was not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub georeference: Option<Value>,
    /// The placed ground-floor slab's extent, `[[west, south], [east,
    /// north]]`. Absent when not placed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bounds: Option<[[f64; 2]; 2]>,
    pub sheets: Vec<SheetReport>,
    pub floors: Vec<FloorSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roof: Option<RoofSummary>,
    pub stone_piers: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// Written under `--output` as `model/<document>.model.json`,
    /// `model/<document>.glb`, `model/<document>.geojson` and
    /// `model/<document>.params.json`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub glb_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub geojson_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params_file: Option<String>,
    /// Every hosted call `--llm-params` made, with tokens and estimated
    /// cost — the shared `llm_usage[]` shape. Absent when no model was
    /// called.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_usage: Option<Value>,
    /// The model itself (`model.json`), inline.
    pub model: Model,
    /// The extruded FeatureCollection, inline.
    pub geojson: FeatureCollection,
}

impl FloorSummary {
    pub fn of(floor: &crate::model::Floor) -> Self {
        Self {
            name: floor.name.clone(),
            page: floor.page,
            level_m: floor.z,
            top_m: floor.top,
            walls: floor.walls.len(),
            glazing: floor.glazing.len(),
            door_gaps: floor.door_gaps.len(),
            rooms: floor.rooms.len(),
            rooms_area_m2: round(floor.rooms.iter().map(|r| r.area).sum(), 2),
            slab_area_m2: round(floor.slab.area_m2(), 2),
            rails: floor.rails.len(),
        }
    }
}

impl RoofSummary {
    pub fn of(roof: &crate::model::Roof, page: usize) -> Self {
        Self {
            page,
            area_m2: round(roof.outline.area_m2(), 2),
            parapet_pieces: roof.parapet.len(),
        }
    }
}
