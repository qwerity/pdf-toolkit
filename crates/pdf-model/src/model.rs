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
    /// The band the slab's edge ends in, when a section cuts one and a plan
    /// draws where it runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge: Option<SlabEdge>,
    /// The storey drew nothing the rules could read — no rooms and no walls,
    /// or a slab too small to be a storey. It is carried as it came off the
    /// sheet, but nothing here is a reconstruction of a floor.
    #[serde(default, skip_serializing_if = "is_false")]
    pub empty: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Roof {
    /// The envelope, the terraces and the edge strips the roof plan draws,
    /// less the edge band when one is read.
    pub outline: Polygon2D,
    pub slab_z: [f64; 2],
    /// The parapet the roof plan draws, on the roof; a ring of
    /// `parapet_width_m` along the outline when it draws none.
    pub parapet: Vec<Polygon2D>,
    pub parapet_z: [f64; 2],
    /// The band the roof's edge ends in, as for a storey's slab.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge: Option<SlabEdge>,
    /// The outline is too small to be a roof, as a storey's `empty` is.
    #[serde(default, skip_serializing_if = "is_false")]
    pub empty: bool,
}

/// `skip_serializing_if` for a flag that means something only when true.
pub(crate) fn is_false(flag: &bool) -> bool {
    !*flag
}

/// Which face of a slab's edge band slopes from its tip to full height.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChamferFace {
    /// The top slopes: the tip stands on the band's underside.
    Top,
    /// The underside slopes: the tip hangs from the band's top.
    Bottom,
}

/// One stretch of an edge band: `inner` along the slab's edge, `outer` along
/// the band's tip, each vertex of one facing the same vertex of the other.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EdgeRun {
    pub inner: Ring,
    pub outer: Ring,
}

/// The band a slab's edge ends in: where it runs, and the profile a section
/// cuts it with. A consumer that can slope lofts each run — the tip along
/// `outer`, full height `chamfer_m` in from it — and one that cannot extrudes
/// it in steps, as the glTF and the GeoJSON do.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlabEdge {
    pub runs: Vec<EdgeRun>,
    /// `[bottom, top]` of the band at full height, metres.
    pub z: [f64; 2],
    /// From the tip to the slab's edge, metres.
    pub depth_m: f64,
    /// The band's height at its tip, metres.
    pub tip_m: f64,
    /// How far in from the tip the sloping face reaches full height, metres.
    pub chamfer_m: f64,
    pub face: ChamferFace,
    /// The section the profile was read from.
    pub section_page: usize,
    /// The plan whose strips placed the runs.
    pub plan_page: usize,
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
    /// the elevations, extruded from the plinth to their `top` — or to the
    /// parapet when no elevation draws one.
    pub stone: Vec<Stone>,
    /// What the callouts on the elevations, sections and roof plan say each
    /// part of the building is made of; absent when they name nothing the
    /// rules know.
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub materials: IndexMap<Part, MaterialCall>,
}

/// A part of the building a material callout names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Part {
    Wall,
    Glazing,
    Rail,
    Stone,
    Roof,
    Parapet,
    Slab,
    /// The ground storey's slab, where it shows above the ground.
    Plinth,
}

/// A material a callout names, from the set the rules know.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Material {
    Travertine,
    Basalt,
    Felsite,
    Tuff,
    RoughStone,
    Stone,
    Plaster,
    Concrete,
    Metal,
    MetalSheet,
    Glass,
}

/// A part's material as the callouts give it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaterialCall {
    pub material: Material,
    /// The first callout that names it, as printed.
    pub text: String,
    /// The sheets that call it out.
    pub pages: Vec<usize>,
}

/// A stone pier: its footprint, and the level its face stands to when the
/// elevations draw it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Stone {
    #[serde(flatten)]
    pub polygon: Polygon2D,
    /// Top of the pier's face on the elevations that draw it, metres; absent
    /// when none does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top: Option<f64>,
    /// The elevation pages `top` was read from; absent with it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub top_pages: Vec<usize>,
}

impl From<Polygon2D> for Stone {
    fn from(polygon: Polygon2D) -> Self {
        Self {
            polygon,
            top: None,
            top_pages: Vec::new(),
        }
    }
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
