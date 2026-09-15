//! The extrusion table: every element the model draws, with its z range
//! and colour, decided once — the glTF writer and the GeoJSON writer both
//! walk this list, so "what is extruded between which levels, in which
//! colour" has one definition and the two outputs cannot drift apart.

use std::borrow::Cow;

use indexmap::IndexMap;

use crate::geom;
use crate::model::{ChamferFace, Material, MaterialCall, Model, Part, Polygon2D, Ring, SlabEdge};
use crate::slab_edge;

/// Half the thickness a railing is drawn with, metres.
const RAIL_HALF_WIDTH_M: f64 = 0.03;

/// What an element is; its name is the GeoJSON `role`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Slab,
    Wall,
    Glazing,
    Rail,
    Stone,
    Roof,
    Parapet,
}

impl Role {
    pub fn name(self) -> &'static str {
        match self {
            Role::Slab => "slab",
            Role::Wall => "wall",
            Role::Glazing => "glazing",
            Role::Rail => "rail",
            Role::Stone => "stone",
            Role::Roof => "roof",
            Role::Parapet => "parapet",
        }
    }

    /// RGBA in 0..=255, the viewer's palette.
    pub fn colour(self) -> [u8; 4] {
        match self {
            Role::Wall | Role::Parapet => [235, 232, 225, 255],
            Role::Slab => [200, 200, 200, 255],
            Role::Glazing => [120, 170, 220, 120],
            Role::Stone => [95, 90, 85, 255],
            Role::Rail => [40, 40, 40, 255],
            Role::Roof => [180, 180, 180, 255],
        }
    }

    /// The same colour as `#rrggbb`, for a map's `color` property.
    pub fn hex(self) -> String {
        hex(self.colour())
    }
}

/// A material's colour, RGBA in 0..=255.
pub fn material_colour(material: Material) -> [u8; 4] {
    match material {
        Material::Travertine => [219, 206, 182, 255],
        Material::Basalt => [72, 72, 75, 255],
        Material::Felsite => [196, 172, 152, 255],
        Material::Tuff => [196, 142, 112, 255],
        Material::RoughStone => [122, 110, 98, 255],
        Material::Stone => [206, 199, 186, 255],
        Material::Plaster => [236, 233, 226, 255],
        Material::Concrete => [186, 186, 181, 255],
        Material::Metal => [58, 61, 66, 255],
        Material::MetalSheet => [150, 156, 162, 255],
        Material::Glass => [120, 170, 220, 120],
    }
}

/// One extruded element.
pub struct Element<'a> {
    pub polygon: Cow<'a, Polygon2D>,
    pub role: Role,
    /// The storey's index and name; absent for the stone piers and the roof.
    pub floor: Option<(usize, &'a str)>,
    /// `[base, top]`, metres.
    pub z: [f64; 2],
    /// RGBA in 0..=255: the material the drawings call out for the element's
    /// part, else the role's own colour.
    pub colour: [u8; 4],
}

impl Element<'_> {
    /// The glTF node this element belongs to: `floor0_walls`, `stone`,
    /// `roof_slab`, `parapet`.
    pub fn layer(&self) -> String {
        match (self.floor, self.role) {
            (Some((i, _)), Role::Slab) => format!("floor{i}_slab"),
            (Some((i, _)), Role::Wall) => format!("floor{i}_walls"),
            (Some((i, _)), Role::Glazing) => format!("floor{i}_glazing"),
            (Some((i, _)), Role::Rail) => format!("floor{i}_rails"),
            (_, Role::Roof) => "roof_slab".into(),
            (_, role) => role.name().into(),
        }
    }

    /// The element's colour as `#rrggbb`, for a map's `color` property.
    pub fn hex(&self) -> String {
        hex(self.colour)
    }
}

/// Everything the model extrudes, in drawing order: each storey's slab and
/// the band its edge ends in, walls, glazing and stroked rails; the stone
/// piers from the plinth to their top, or to the parapet without one; the
/// roof slab, its band and its parapet. `plinth_m` and `rail_height_m` are
/// parameters rather than model facts.
pub fn elements(model: &Model, plinth_m: f64, rail_height_m: f64) -> Vec<Element<'_>> {
    let materials = &model.materials;
    let mut out: Vec<Element<'_>> = Vec::new();
    for (index, floor) in model.floors.iter().enumerate() {
        let storey = Some((index, floor.name.as_str()));
        let element = |polygon, role, z| Element {
            polygon,
            role,
            floor: storey,
            z,
            colour: colour(role, Some(index), materials),
        };
        out.push(element(
            Cow::Borrowed(&floor.slab),
            Role::Slab,
            floor.slab_z,
        ));
        out.extend(
            floor
                .edge
                .iter()
                .flat_map(band_steps)
                .map(|(polygon, z)| element(Cow::Owned(polygon), Role::Slab, z)),
        );
        out.extend(
            floor
                .walls
                .iter()
                .map(|wall| element(Cow::Borrowed(wall), Role::Wall, [floor.z, floor.top])),
        );
        out.extend(
            floor
                .glazing
                .iter()
                .map(|glazing| element(Cow::Borrowed(glazing), Role::Glazing, floor.glazing_z)),
        );
        out.extend(floor.rails.iter().flat_map(|rail| {
            geom::stroke(&geom::ring_in(rail), RAIL_HALF_WIDTH_M)
                .0
                .into_iter()
                .map(move |polygon| {
                    element(
                        Cow::Owned(geom::to_polygon2d(&polygon)),
                        Role::Rail,
                        [floor.z, floor.z + rail_height_m],
                    )
                })
        }));
    }
    let ground = model.levels.get("ground").copied().unwrap_or(0.0);
    let parapet_top = model
        .levels
        .get("parapet")
        .copied()
        .unwrap_or_else(|| model.roof.as_ref().map_or(ground, |r| r.parapet_z[1]));
    for pier in &model.stone {
        out.push(Element {
            polygon: Cow::Borrowed(&pier.polygon),
            role: Role::Stone,
            floor: None,
            z: [ground - plinth_m, pier.top.unwrap_or(parapet_top)],
            colour: colour(Role::Stone, None, materials),
        });
    }
    if let Some(roof) = &model.roof {
        let element = |polygon, role, z| Element {
            polygon,
            role,
            floor: None,
            z,
            colour: colour(role, None, materials),
        };
        out.push(element(
            Cow::Borrowed(&roof.outline),
            Role::Roof,
            roof.slab_z,
        ));
        out.extend(
            roof.edge
                .iter()
                .flat_map(band_steps)
                .map(|(polygon, z)| element(Cow::Owned(polygon), Role::Roof, z)),
        );
        out.extend(
            roof.parapet
                .iter()
                .map(|parapet| element(Cow::Borrowed(parapet), Role::Parapet, roof.parapet_z)),
        );
    }
    out
}

/// The colour an element is drawn in: the material the drawings call out for
/// its part — the ground storey's slab shows as the plinth, when one is called
/// out — or its role's own.
fn colour(role: Role, floor: Option<usize>, materials: &IndexMap<Part, MaterialCall>) -> [u8; 4] {
    let parts: &[Part] = match (role, floor) {
        (Role::Slab, Some(0)) => &[Part::Plinth, Part::Slab],
        (Role::Slab, _) => &[Part::Slab],
        (Role::Wall, _) => &[Part::Wall],
        (Role::Glazing, _) => &[Part::Glazing],
        (Role::Rail, _) => &[Part::Rail],
        (Role::Stone, _) => &[Part::Stone],
        (Role::Roof, _) => &[Part::Roof],
        (Role::Parapet, _) => &[Part::Parapet],
    };
    parts
        .iter()
        .find_map(|part| materials.get(part))
        .map_or(role.colour(), |call| material_colour(call.material))
}

fn hex([r, g, b, _]: [u8; 4]) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// A slab's edge band in steps, for a consumer that extrudes but cannot
/// slope: across the chamfer two strips, each as high as the slope stands
/// halfway along it, then the rest of the band at full height.
fn band_steps(edge: &SlabEdge) -> Vec<(Polygon2D, [f64; 2])> {
    let [bottom, top] = edge.z;
    let rise = top - bottom - edge.tip_m;
    let span = |height: f64| match edge.face {
        ChamferFace::Top => [bottom, bottom + height],
        ChamferFace::Bottom => [top - height, top],
    };
    let share = if edge.depth_m > 0.0 {
        (edge.chamfer_m / edge.depth_m).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let mut steps = Vec::new();
    for run in &edge.runs {
        let middle = across(&run.outer, &run.inner, share / 2.0);
        let full = across(&run.outer, &run.inner, share);
        for (from, to, height) in [
            (&run.outer, &middle, edge.tip_m + rise / 4.0),
            (&middle, &full, edge.tip_m + rise * 3.0 / 4.0),
            (&full, &run.inner, top - bottom),
        ] {
            steps.extend(
                slab_edge::between(from, to)
                    .iter()
                    .map(|polygon| (geom::to_polygon2d(polygon), span(height))),
            );
        }
    }
    steps
}

/// The line `share` of the way from `from` to `to`, vertex by vertex.
fn across(from: &Ring, to: &Ring, share: f64) -> Ring {
    from.iter()
        .zip(to)
        .map(|(p, q)| [p[0] + (q[0] - p[0]) * share, p[1] + (q[1] - p[1]) * share])
        .collect()
}
