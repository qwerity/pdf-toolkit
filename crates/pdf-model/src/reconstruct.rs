//! The reconstruction rules: from a storey's fills to walls, openings, slab
//! and railings, and from the roof sheet to the roof outline and parapet.
//!
//! Every rule is a set operation on exact fills. Openings are what is left
//! of the envelope after the rooms and the walls are subtracted; a piece on
//! the envelope's edge is exterior glazing, an interior piece a door gap.
//! Ground-floor wall pieces outside the envelope are the stone piers the
//! elevations show. Nothing here is estimated from a rendering.

use anyhow::{Result, anyhow};
use geo::{Area, BoundingRect, Buffer, Polygon};
use indexmap::IndexMap;

use crate::geom;
use crate::model::{Floor, Model, Polygon2D, Roof, Room};
use crate::params::{FloorParams, Params};
use crate::sheet::Sheet;

/// Slivers thinner than twice this (m) are dropped from the openings.
const OPENING_SLIVER_M: f64 = 0.01;
/// An opening smaller than this (m²) is noise between fills.
const MIN_OPENING_M2: f64 = 0.02;
/// An opening this close to the envelope edge is exterior glazing.
const EDGE_TOUCH_M: f64 = 0.02;
/// A room fill must be at least this big (m²); smaller white fills are
/// furniture and fixtures.
const MIN_ROOM_M2: f64 = 1.0;
/// Terrace edge pieces shorter than this (m) are corner debris, not rails.
const MIN_RAIL_M: f64 = 0.3;
/// Gaps narrower than twice this (m) between the roof's parts are closed.
const ROOF_CLOSE_M: f64 = 0.03;
/// Roof-edge strips and corner pieces: their area range (m²), minimum
/// width (m) and how far from the envelope they may sit (m).
const ROOF_EDGE_AREA_M2: std::ops::Range<f64> = 0.3..8.0;
const ROOF_EDGE_MIN_WIDTH_M: f64 = 0.15;
const ROOF_EDGE_REACH_M: f64 = 1.0;

/// A storey's parameters with its sheet read in.
pub struct FloorInput<'a> {
    pub params: &'a FloorParams,
    pub sheet: &'a Sheet,
}

/// What one storey's rules produced, before serialisation.
struct FloorBuild {
    floor: Floor,
    /// Wall pieces outside the envelope, kept only for the ground floor.
    outside: Vec<Polygon<f64>>,
}

/// Builds the model from the read sheets. `source` names the document.
pub fn build(
    floors: &[FloorInput<'_>],
    roof: Option<&Sheet>,
    params: &Params,
    source: &str,
    warnings: &mut Vec<String>,
) -> Result<Model> {
    let mut model = Model {
        units: "m".into(),
        up: "z".into(),
        source: source.to_string(),
        scale: format!("1:{}", params.scale_denominator),
        levels: levels(params),
        floors: Vec::new(),
        roof: None,
        stone: Vec::new(),
    };
    for (index, input) in floors.iter().enumerate() {
        // A storey whose sheet carries none of the fills the rules read is
        // left out with a warning; the model is the storeys that could be built.
        let built = match build_floor(input, index == 0, params, warnings) {
            Ok(built) => built,
            Err(error) => {
                warnings.push(format!(
                    "page {}: storey skipped: {error:#}",
                    input.sheet.page
                ));
                continue;
            }
        };
        if index == 0 {
            model.stone = built.outside.iter().map(geom::to_polygon2d).collect();
        }
        model.floors.push(built.floor);
    }
    if model.floors.is_empty() {
        return Err(anyhow!("no storey could be built: {}", warnings.join("; ")));
    }
    if let Some(sheet) = roof {
        // A roof sheet that draws no outline the rules can read leaves the
        // model without a roof rather than without a model.
        match build_roof(sheet, params) {
            Ok(roof) => model.roof = Some(roof),
            Err(error) => warnings.push(format!("roof omitted: {error:#}")),
        }
    }
    Ok(model)
}

fn levels(params: &Params) -> IndexMap<String, f64> {
    let mut levels = IndexMap::new();
    for (index, floor) in params.floors.iter().enumerate() {
        let name = match index {
            0 => "ground".to_string(),
            1 => "first".to_string(),
            2 => "second".to_string(),
            3 => "third".to_string(),
            n => format!("floor_{n}"),
        };
        levels.insert(name, floor.level_m);
    }
    levels.insert("roof".into(), params.roof_level_m);
    levels.insert("parapet".into(), params.parapet_level_m);
    levels
}

fn build_floor(
    input: &FloorInput<'_>,
    is_ground: bool,
    params: &Params,
    warnings: &mut Vec<String>,
) -> Result<FloorBuild> {
    let sheet = input.sheet;
    let derived = if sheet.envelope().is_none() {
        let outline = sheet.derived_envelope().ok_or_else(|| {
            anyhow!(
                "page {}: no envelope-coloured fill and no walls or rooms to outline",
                sheet.page
            )
        })?;
        warnings.push(format!(
            "page {}: no envelope-coloured fill; the building box is the outline of the walls and rooms",
            sheet.page
        ));
        Some(outline)
    } else {
        None
    };
    let envelope = derived
        .as_ref()
        .or_else(|| sheet.envelope())
        .expect("an envelope, drawn or derived");
    let terraces = sheet.side_terraces(envelope);
    let walls_u = geom::union_all(&sheet.walls);
    let footprint = geom::union_all(std::iter::once(envelope).chain(terraces.iter()));

    let rooms: Vec<&Polygon<f64>> = sheet
        .rooms
        .iter()
        .filter(|p| p.unsigned_area() > MIN_ROOM_M2)
        .filter(|p| geom::intersection_area(p, &footprint) > 0.5 * p.unsigned_area())
        .collect();
    let rooms_u = geom::union_all(rooms.iter().copied());

    // Openings: the envelope minus rooms minus walls, slivers dropped.
    let leftover = geom::open(
        &geo::BooleanOps::difference(&geo::BooleanOps::difference(envelope, &rooms_u), &walls_u),
        OPENING_SLIVER_M,
    );
    let mut glazing = Vec::new();
    let mut door_gaps = Vec::new();
    for piece in geom::parts(leftover) {
        if piece.unsigned_area() < MIN_OPENING_M2 {
            continue;
        }
        if geom::ring_distance(piece.exterior(), envelope.exterior()) < EDGE_TOUCH_M {
            glazing.push(piece);
        } else {
            door_gaps.push(piece);
        }
    }

    // Walls inside the box are the storey's; ground-floor pieces outside it
    // are the stone piers. Upper-floor pieces outside are still walls.
    let mut inside = Vec::new();
    let mut outside = Vec::new();
    for piece in geom::parts(walls_u) {
        if geom::intersection_area(&piece, envelope) > 0.5 * piece.unsigned_area() {
            inside.push(piece);
        } else {
            outside.push(piece);
        }
    }
    if !is_ground {
        inside.append(&mut outside);
    }

    let slab = geom::largest(footprint)
        .map(|p| geom::exterior_only(&p))
        .ok_or_else(|| anyhow!("page {}: empty footprint", sheet.page))?;

    // Railings run along the terrace edges the box does not share.
    let mut rails = Vec::new();
    if !is_ground {
        let box_grown =
            geom::largest(envelope.buffer(EDGE_TOUCH_M)).unwrap_or_else(|| envelope.clone());
        for terrace in &terraces {
            for edge in geom::line_outside(terrace.exterior(), &box_grown) {
                if geom::length(&edge) > MIN_RAIL_M {
                    rails.push(geom::line_out(&edge));
                }
            }
        }
    }

    let z = input.params.level_m;
    let slab_bottom = z - if is_ground {
        params.plinth_m
    } else {
        params.slab_thickness_m
    };
    Ok(FloorBuild {
        floor: Floor {
            name: input.params.name.clone(),
            page: sheet.page,
            z,
            top: input.params.top_m,
            envelope: geom::to_polygon2d(envelope),
            slab: geom::to_polygon2d(&slab),
            slab_z: [slab_bottom, z],
            walls: inside.iter().map(geom::to_polygon2d).collect(),
            glazing: glazing.iter().map(geom::to_polygon2d).collect(),
            glazing_z: [z, input.params.top_m - params.slab_thickness_m],
            door_gaps: door_gaps.iter().map(geom::to_polygon2d).collect(),
            rooms: rooms
                .iter()
                .map(|p| Room {
                    area: (p.unsigned_area() * 100.0).round() / 100.0,
                    polygon: geom::to_polygon2d(p),
                })
                .collect(),
            rails,
        },
        outside,
    })
}

/// The roof outline is the envelope, its curved sides and the white edge
/// strips and quarter-circle corners the roof sheet draws around it, closed
/// into one ring; the parapet is a ring of that outline.
fn build_roof(sheet: &Sheet, params: &Params) -> Result<Roof> {
    let derived = sheet
        .envelope()
        .is_none()
        .then(|| sheet.derived_envelope())
        .flatten();
    let envelope = derived
        .as_ref()
        .or_else(|| sheet.envelope())
        .ok_or_else(|| anyhow!("page {}: no envelope-coloured fill", sheet.page))?;
    let mut parts: Vec<Polygon<f64>> = vec![envelope.clone()];
    parts.extend(sheet.side_terraces(envelope));
    for fill in &sheet.rooms {
        let Some(bounds) = fill.bounding_rect() else {
            continue;
        };
        let width = bounds.width().min(bounds.height());
        if ROOF_EDGE_AREA_M2.contains(&fill.unsigned_area())
            && width > ROOF_EDGE_MIN_WIDTH_M
            && geom::polygon_distance(fill, envelope) < ROOF_EDGE_REACH_M
        {
            parts.push(fill.clone());
        }
    }
    let joined = geom::close(&geom::union_all(&parts), ROOF_CLOSE_M);
    let outline = geom::largest(joined)
        .map(|p| geom::exterior_only(&p))
        .ok_or_else(|| anyhow!("page {}: empty roof", sheet.page))?;
    let parapet: Vec<Polygon2D> = geom::parts(geo::BooleanOps::difference(
        &outline,
        &outline.buffer(-params.parapet_width_m),
    ))
    .iter()
    .map(geom::to_polygon2d)
    .collect();
    Ok(Roof {
        outline: geom::to_polygon2d(&outline),
        slab_z: [
            params.roof_level_m - params.slab_thickness_m,
            params.roof_level_m,
        ],
        parapet,
        parapet_z: [params.roof_level_m, params.parapet_level_m],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::FloorParams;
    use crate::sheet::OriginSource;
    use pdf_geometry::model::Point;

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Polygon<f64> {
        geom::polygon_from_ring(&[(x0, y0), (x1, y0), (x1, y1), (x0, y1)])
            .pop()
            .unwrap()
    }

    /// A 10 x 6 box with a 0.4 m wall ring, one 0.1 m partition, two rooms
    /// filling the interior except a 1.2 m window gap in the north wall
    /// and a 0.9 m door gap in the partition.
    fn house() -> Sheet {
        let envelope = rect(0.0, 0.0, 10.0, 6.0);
        let walls = vec![
            rect(0.0, 0.0, 10.0, 0.4),
            rect(0.0, 5.6, 1.0, 6.0),
            rect(2.2, 5.6, 10.0, 6.0),
            rect(0.0, 0.0, 0.4, 6.0),
            rect(9.6, 0.0, 10.0, 6.0),
            rect(4.95, 0.4, 5.05, 3.0),
            rect(4.95, 3.9, 5.05, 5.6),
            // A stone pier outside the box.
            rect(12.0, 0.0, 12.5, 0.5),
        ];
        let rooms = vec![rect(0.4, 0.4, 4.95, 5.6), rect(5.05, 0.4, 9.6, 5.6)];
        Sheet {
            page: 7,
            origin: Point::new(0.0, 0.0),
            origin_source: OriginSource::AxisBubbles,
            walls,
            envelopes: vec![envelope],
            rooms,
        }
    }

    fn params() -> Params {
        let mut params = Params::archicad_template();
        params.floors.push(FloorParams {
            page: 7,
            name: "Ground".into(),
            level_m: 0.0,
            top_m: 3.6,
        });
        params
    }

    #[test]
    fn openings_split_into_edge_glazing_and_interior_door_gaps() {
        let params = params();
        let sheet = house();
        let built = build_floor(
            &FloorInput {
                params: &params.floors[0],
                sheet: &sheet,
            },
            true,
            &params,
            &mut Vec::new(),
        )
        .unwrap();
        let floor = built.floor;
        assert_eq!(
            floor.glazing.len(),
            1,
            "the window gap: {:?}",
            floor.glazing
        );
        assert_eq!(
            floor.door_gaps.len(),
            1,
            "the door gap: {:?}",
            floor.door_gaps
        );
        assert_eq!(floor.rooms.len(), 2);
        assert!((floor.rooms[0].area - 23.66).abs() < 0.01);
        assert_eq!(floor.walls.len(), 1, "the wall ring and partition touch");
        assert_eq!(built.outside.len(), 1, "the pier is outside the box");
        assert_eq!(floor.slab_z, [-0.35, 0.0]);
        assert_eq!(floor.glazing_z, [0.0, 3.1]);
        assert!(floor.rails.is_empty(), "no rails on the ground floor");
    }

    #[test]
    fn an_upper_floor_keeps_outside_walls_and_rails_its_terrace() {
        let mut params = params();
        params.floors[0].level_m = 3.6;
        params.floors[0].top_m = 7.2;
        let mut sheet = house();
        // A 3 x 3 terrace hugging the east side.
        sheet.envelopes.push(rect(10.0, 1.5, 13.0, 4.5));
        let built = build_floor(
            &FloorInput {
                params: &params.floors[0],
                sheet: &sheet,
            },
            false,
            &params,
            &mut Vec::new(),
        )
        .unwrap();
        assert!(built.outside.is_empty());
        assert_eq!(built.floor.walls.len(), 2, "pier counted as a wall");
        let rail_length: f64 = built
            .floor
            .rails
            .iter()
            .map(|r| geom::length(&geom::ring_in(r)))
            .sum();
        assert!(
            (rail_length - 9.0).abs() < 0.1,
            "three free sides of 3 m: {rail_length}"
        );
        assert_eq!(built.floor.slab_z, [3.1, 3.6]);
        let slab = geom::from_polygon2d(&built.floor.slab);
        assert!((slab.unsigned_area() - 69.0).abs() < 1e-6);
    }

    #[test]
    fn the_roof_closes_its_parts_and_rings_a_parapet() {
        let params = params();
        let mut sheet = house();
        // A 0.3 m edge strip along the north side, 2 cm away.
        sheet.rooms = vec![rect(0.0, 6.02, 10.0, 6.32)];
        let roof = build_roof(&sheet, &params).unwrap();
        let outline = geom::from_polygon2d(&roof.outline);
        assert!(
            (outline.unsigned_area() - 63.2).abs() < 0.05,
            "box plus strip, gap closed: {}",
            outline.unsigned_area()
        );
        let parapet: f64 = roof.parapet.iter().map(Polygon2D::area_m2).sum();
        assert!(parapet > 4.0 && parapet < 6.0, "a 0.15 m ring: {parapet}");
        assert_eq!(roof.slab_z, [6.7, 7.2]);
        assert_eq!(roof.parapet_z, [7.2, 7.78]);
    }
}
