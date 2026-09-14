//! The `model.json` schema: 2D polygons with z ranges, in metres, relative to
//! the grid origin. The one contract every consumer shares — the glTF
//! writer, the GeoJSON extrusion, and the three.js viewer are all clients of
//! this shape, so it does not change without all of them.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

/// A closed ring as `[x, y]` pairs, without the closing repeat.
pub type Ring = Vec<[f64; 2]>;

/// A polygon with optional holes. Outer rings are counter-clockwise, holes
/// clockwise.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Polygon2D {
    pub outer: Ring,
    #[serde(default)]
    pub holes: Vec<Ring>,
}

/// A room fill with the area it measures.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Room {
    /// Square metres, rounded to a hundredth.
    pub area: f64,
    #[serde(flatten)]
    pub polygon: Polygon2D,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Floor {
    pub name: String,
    /// One-based page of the plan sheet.
    pub page: usize,
    /// Finished floor level, metres.
    pub z: f64,
    /// Level of the storey above, metres.
    pub top: f64,
    /// The building box the sheet draws: the slab without its terraces,
    /// which is the outline a site plan draws for the building.
    #[serde(default)]
    pub envelope: Polygon2D,
    pub slab: Polygon2D,
    /// `[bottom, top]` of the slab, metres.
    pub slab_z: [f64; 2],
    pub walls: Vec<Polygon2D>,
    pub glazing: Vec<Polygon2D>,
    pub glazing_z: [f64; 2],
    pub door_gaps: Vec<Polygon2D>,
    pub rooms: Vec<Room>,
    /// Balcony railings as open polylines along the terrace edges.
    pub rails: Vec<Ring>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Roof {
    pub outline: Polygon2D,
    pub slab_z: [f64; 2],
    pub parapet: Vec<Polygon2D>,
    pub parapet_z: [f64; 2],
}

/// The whole building.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Model {
    pub units: String,
    pub up: String,
    pub source: String,
    /// The plan scale, as printed: `1:75`.
    pub scale: String,
    /// Named levels in metres: `ground`, one per storey, `roof`, `parapet`.
    pub levels: IndexMap<String, f64>,
    pub floors: Vec<Floor>,
    /// Absent when the set has no roof plan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roof: Option<Roof>,
    /// The parts of the ground-floor walls outside the envelope, cut at it and
    /// without the slivers a wall drawn past it leaves: the stone piers seen in
    /// the elevations, extruded from the plinth to the parapet.
    pub stone: Vec<Polygon2D>,
}

impl Polygon2D {
    pub fn vertex_count(&self) -> usize {
        self.outer.len() + self.holes.iter().map(Vec::len).sum::<usize>()
    }

    /// Square metres: the outer ring less its holes.
    pub fn area_m2(&self) -> f64 {
        let area = |ring: &Ring| {
            crate::geom::ring_area(&ring.iter().map(|p| (p[0], p[1])).collect::<Vec<_>>())
        };
        area(&self.outer) - self.holes.iter().map(area).sum::<f64>()
    }
}
