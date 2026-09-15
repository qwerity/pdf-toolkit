//! Putting the model on the ground.
//!
//! The model is built in the building's own metres about the grid origin;
//! nothing on a floor plan says where on Earth that is. Three things can:
//!
//! 1. **A placed sibling sheet.** The dossier's site plan draws the same
//!    building box, and when that sheet was georeferenced (from its
//!    coordinate table, a sibling's, or the caller's parcel polygon) the
//!    box it draws is a set of points on the ground. The ground floor's
//!    envelope is matched to it by shape — rigidly, rotation and
//!    translation only, because both are already in metres — and accepted
//!    when the corners agree to a fraction of the building's size. A box is
//!    symmetric, so the sheet's other line work (the terraces, the entrance
//!    steps) settles which way round the building stands: of the rotations
//!    that fit the box equally, the one whose slab outline lies closest to
//!    that line work wins.
//! 2. **References** the caller supplies (`--georeference`): two points in
//!    the model's frame and where they are, the same mechanism `vector`
//!    offers an unplaced page.
//! 3. **Nothing.** The model stays in `local_metres`, right in shape and
//!    unknown in position, for a map to let someone put it down by hand.
//!
//! The choice is reported; a placement is never guessed.

use geo::BoundingRect;
use pdf_geo::LonLat;
use pdf_geo::fit::Similarity;
use pdf_geo::local::{Placement, Reference, metres_per_degree, place};
use pdf_geometry::model::Point;
use pdf_vector::DocumentVector;
use serde::Serialize;
use serde_json::Value;

use crate::geom::{self, point_segment_distance, ring_area, rotate, round};
use crate::model::{Model, Polygon2D};

/// How the model was placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelPlacementSource {
    /// Matched by shape to the building box drawn on a placed sheet.
    SheetOutline,
    /// From caller-supplied references.
    References,
}

/// One drawn segment, as two points.
type Segment = ((f64, f64), (f64, f64));

/// A placement plus its provenance.
#[derive(Debug, Clone)]
pub struct ModelPlacement {
    pub placement: Placement,
    pub source: ModelPlacementSource,
    /// The placed sheet the box was matched on.
    pub page: Option<usize>,
    /// Mean distance between the sheet's box corners and the placed
    /// envelope, metres. The number that says the match is right.
    pub residual_m: f64,
    /// Mean distance from the placed slab's corners to the sheet's nearest
    /// line work, metres — what settled the orientation. Absent when the
    /// sheet draws nothing else near the building.
    pub linework_residual_m: Option<f64>,
}

/// Box candidates on a placed sheet within this ratio of the envelope's
/// area are tried: a site plan draws the footprint simplified, not scaled.
const AREA_RATIO: std::ops::RangeInclusive<f64> = 0.7..=1.4;
/// Accepted when the corners agree to this fraction of the building's
/// size, but never looser than half a metre.
const RESIDUAL_FRACTION: f64 = 0.04;
const RESIDUAL_FLOOR_M: f64 = 0.5;
/// Rotations whose box fit is within this of the best are the symmetric
/// alternatives the line work decides between (metres).
const ALTERNATIVE_MARGIN_M: f64 = 0.05;
/// Line work farther than this from the box centre (metres) is not the
/// building's.
const LINEWORK_REACH_M: f64 = 40.0;
/// Loose polylines whose ends meet within this (degrees, ≈5 cm) are stitched.
const STITCH_TOLERANCE_DEG: f64 = 5e-7;

/// Places the model from caller-supplied references.
pub fn from_references(references: &[Reference]) -> Option<ModelPlacement> {
    let placement = place(references)?;
    Some(ModelPlacement {
        residual_m: placement.rmse_m,
        placement,
        source: ModelPlacementSource::References,
        page: None,
        linework_residual_m: None,
    })
}

/// Places the model by matching its ground-floor envelope to a building
/// box drawn on a placed sheet of the same dossier. The best-fitting box
/// across every placed sheet wins; none within tolerance, no placement.
pub fn from_document(model: &Model, vector: &DocumentVector) -> Option<ModelPlacement> {
    let floor = model.floors.first()?;
    let box_polygon = if floor.envelope.outer.len() >= 3 {
        &floor.envelope
    } else {
        &floor.slab
    };
    let target = ring_of(box_polygon);
    let target_area = ring_area(&target);
    let (target_centred, target_centre) = centre(&target);
    let slab_centred: Vec<(f64, f64)> = ring_of(&floor.slab)
        .iter()
        .map(|p| (p.0 - target_centre.0, p.1 - target_centre.1))
        .collect();
    let allowance = (RESIDUAL_FRACTION * target_area.sqrt()).max(RESIDUAL_FLOOR_M);

    // Ranked by the box fit to the centimetre, then by the line work.
    let mut best: Option<(ModelPlacement, (i64, f64))> = None;
    for page in vector
        .pages
        .iter()
        .filter(|p| p.crs == "EPSG:4326" && p.georeference.as_ref().is_some_and(|g| g.placed))
    {
        let Ok(collection) = serde_json::to_value(&page.geojson) else {
            continue;
        };
        let lines = drawn_lines(&collection);
        let segments = segments_of(&lines);
        for ring in closed_rings(lines) {
            let centroid = ring_centroid(&ring);
            let (per_lon, per_lat) = metres_per_degree(centroid.1);
            let to_metres =
                |p: (f64, f64)| ((p.0 - centroid.0) * per_lon, (p.1 - centroid.1) * per_lat);
            let ring_m: Vec<(f64, f64)> = ring.iter().map(|&p| to_metres(p)).collect();
            let area = ring_area(&ring_m);
            if !AREA_RATIO.contains(&(area / target_area.max(1e-9))) {
                continue;
            }
            let fits = align(&target_centred, &ring_m);
            let Some(&(_, best_residual)) = fits.first() else {
                continue;
            };
            if best_residual > allowance {
                continue;
            }
            let nearby: Vec<Segment> = segments
                .iter()
                .map(|&(a, b)| (to_metres(a), to_metres(b)))
                .filter(|&(a, b)| {
                    a.0.hypot(a.1) <= LINEWORK_REACH_M && b.0.hypot(b.1) <= LINEWORK_REACH_M
                })
                .collect();
            for &(angle, residual) in fits
                .iter()
                .take_while(|(_, r)| *r <= best_residual + ALTERNATIVE_MARGIN_M)
            {
                let linework = (!nearby.is_empty())
                    .then(|| mean_segment_distance(&rotate(&slab_centred, angle), &nearby));
                let rank = (
                    (residual * 100.0).round() as i64,
                    linework.unwrap_or(f64::INFINITY),
                );
                if best.as_ref().is_some_and(|(_, r)| *r <= rank) {
                    continue;
                }
                // Model metres → tangent-plane metres about the ring's
                // centroid: rotate about the box's centre, and the centres
                // coincide.
                let (sin, cos) = angle.sin_cos();
                let (cx, cy) = target_centre;
                let transform = Similarity {
                    a: cos,
                    b: sin,
                    tx: -(cos * cx - sin * cy),
                    ty: -(sin * cx + cos * cy),
                };
                best = Some((
                    ModelPlacement {
                        placement: Placement {
                            transform,
                            origin: LonLat {
                                lon: centroid.0,
                                lat: centroid.1,
                            },
                            rmse_m: residual,
                            max_residual_m: residual,
                            references: ring.len(),
                            scale: 1.0,
                            rotation_degrees: angle.to_degrees(),
                        },
                        source: ModelPlacementSource::SheetOutline,
                        page: Some(page.page_number),
                        residual_m: residual,
                        linework_residual_m: linework,
                    },
                    rank,
                ));
            }
        }
    }
    best.map(|(placement, _)| placement)
}

/// A placement's report row.
pub fn report(placement: &ModelPlacement) -> Value {
    let p = &placement.placement;
    let mut row = serde_json::json!({
        "placed": true,
        "source": placement.source,
        "residual_m": round(placement.residual_m, 4),
        "rotation_degrees": round(p.rotation_degrees.rem_euclid(360.0), 4),
        "origin": [round(p.origin.lon, 7), round(p.origin.lat, 7)],
        // The full local → tangent-plane transform, so a consumer can bring
        // other ground geometry (the parcel) into the model's frame.
        "transform": {
            "a": p.transform.a,
            "b": p.transform.b,
            "tx": p.transform.tx,
            "ty": p.transform.ty,
        },
    });
    if let Some(page) = placement.page {
        row["page"] = page.into();
    }
    if let Some(linework) = placement.linework_residual_m {
        row["linework_residual_m"] = round(linework, 4).into();
    }
    row
}

fn ring_of(polygon: &Polygon2D) -> Vec<(f64, f64)> {
    polygon.outer.iter().map(|p| (p[0], p[1])).collect()
}

/// The untagged line work of a placed sheet: the survey parcel, the
/// reference parcel and the lettering carry a `kind`; the drawing does not.
fn drawn_lines(collection: &Value) -> Vec<Vec<(f64, f64)>> {
    let mut lines = Vec::new();
    for feature in collection["features"]
        .as_array()
        .map_or(&[][..], Vec::as_slice)
    {
        if feature["properties"]["kind"].is_string() {
            continue;
        }
        let geometry = &feature["geometry"];
        let line = match geometry["type"].as_str() {
            Some("LineString") => geometry["coordinates"].as_array(),
            Some("Polygon") => geometry["coordinates"][0].as_array(),
            _ => None,
        };
        if let Some(points) = line.and_then(|values| points_of(values))
            && points.len() >= 2
        {
            lines.push(points);
        }
    }
    lines
}

/// Every closed ring among a sheet's drawn lines: polygons, closed line
/// strings, and loose polylines whose ends meet.
fn closed_rings(lines: Vec<Vec<(f64, f64)>>) -> Vec<Vec<(f64, f64)>> {
    let mut rings = Vec::new();
    let mut open = Vec::new();
    for points in lines {
        if points.len() >= 4 && points.first() == points.last() {
            rings.push(points);
        } else {
            open.push(points);
        }
    }
    rings.extend(stitch(open, STITCH_TOLERANCE_DEG));
    rings
}

/// Every drawn segment, in degrees.
fn segments_of(lines: &[Vec<(f64, f64)>]) -> Vec<Segment> {
    lines
        .iter()
        .flat_map(|line| line.windows(2).map(|w| (w[0], w[1])).collect::<Vec<_>>())
        .collect()
}

fn points_of(values: &[Value]) -> Option<Vec<(f64, f64)>> {
    values
        .iter()
        .map(|v| Some((v[0].as_f64()?, v[1].as_f64()?)))
        .collect()
}

/// Chains loose polylines end to end into closed rings; a junction with
/// more than one continuation ends the chain rather than guessing.
fn stitch(mut open: Vec<Vec<(f64, f64)>>, tolerance: f64) -> Vec<Vec<(f64, f64)>> {
    let near = |a: (f64, f64), b: (f64, f64)| (a.0 - b.0).hypot(a.1 - b.1) <= tolerance;
    let mut rings = Vec::new();
    while let Some(mut chain) = open.pop() {
        loop {
            let tail = *chain.last().expect("non-empty");
            let matches: Vec<usize> = open
                .iter()
                .enumerate()
                .filter(|(_, p)| near(tail, p[0]) || near(tail, *p.last().expect("non-empty")))
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
        if chain.len() >= 4 && near(chain[0], *chain.last().expect("non-empty")) {
            let head = chain[0];
            *chain.last_mut().expect("non-empty") = head;
            rings.push(chain);
        }
    }
    rings
}

/// Area centroid of a ring, computed about its first vertex: the shoelace
/// on absolute longitudes cancels catastrophically — a building's area in
/// square degrees is 1e-8 against cross products of 1e3.
fn ring_centroid(points: &[(f64, f64)]) -> (f64, f64) {
    let Some(&(ox, oy)) = points.first() else {
        return (0.0, 0.0);
    };
    let mut ring: Vec<(f64, f64)> = points.iter().map(|p| (p.0 - ox, p.1 - oy)).collect();
    if ring.len() > 1 && ring.first() == ring.last() {
        ring.pop();
    }
    let n = ring.len();
    let mut twice_area = 0.0;
    let (mut cx, mut cy) = (0.0, 0.0);
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        let cross = a.0 * b.1 - b.0 * a.1;
        twice_area += cross;
        cx += (a.0 + b.0) * cross;
        cy += (a.1 + b.1) * cross;
    }
    if twice_area.abs() < 1e-18 {
        let n = n.max(1) as f64;
        return (
            ox + ring.iter().map(|p| p.0).sum::<f64>() / n,
            oy + ring.iter().map(|p| p.1).sum::<f64>() / n,
        );
    }
    (ox + cx / (3.0 * twice_area), oy + cy / (3.0 * twice_area))
}

/// The points about their area centroid, and that centroid.
fn centre(points: &[(f64, f64)]) -> (Vec<(f64, f64)>, (f64, f64)) {
    let c = ring_centroid(points);
    (points.iter().map(|p| (p.0 - c.0, p.1 - c.1)).collect(), c)
}

/// Mean distance from each of `from`'s vertices to the nearest segment of
/// the closed outline `onto`.
fn mean_distance(from: &[(f64, f64)], onto: &[(f64, f64)]) -> f64 {
    if from.is_empty() || onto.len() < 2 {
        return f64::INFINITY;
    }
    let n = onto.len();
    let total: f64 = from
        .iter()
        .map(|&p| {
            (0..n)
                .map(|i| point_segment_distance(p, onto[i], onto[(i + 1) % n]))
                .fold(f64::INFINITY, f64::min)
        })
        .sum();
    total / from.len() as f64
}

/// Mean distance from each vertex to the nearest of the loose segments.
fn mean_segment_distance(from: &[(f64, f64)], segments: &[Segment]) -> f64 {
    if from.is_empty() || segments.is_empty() {
        return f64::INFINITY;
    }
    let total: f64 = from
        .iter()
        .map(|&p| {
            segments
                .iter()
                .map(|&(a, b)| point_segment_distance(p, a, b))
                .fold(f64::INFINITY, f64::min)
        })
        .sum();
    total / from.len() as f64
}

/// The rotations that lay the centred `outline` onto the centred `target`
/// ring, best first, each with how far the two then sit apart (metres): the
/// mean distance from each ring's corners to the other's edges, whichever
/// is worse, so a small outline inside a big ring cannot score well. The
/// whole circle is scanned and every local minimum refined — a rectangle
/// fits its box two ways, a square four, and the caller decides between
/// them with other evidence.
fn align(outline: &[(f64, f64)], target: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let (target_centred, _) = centre(target);
    let score_at = |angle: f64| {
        let rotated = rotate(outline, angle);
        mean_distance(&target_centred, &rotated).max(mean_distance(&rotated, &target_centred))
    };
    const STEPS: usize = 72;
    let step_angle = |k: usize| k as f64 * 5.0_f64.to_radians();
    let coarse: Vec<f64> = (0..STEPS).map(|k| score_at(step_angle(k))).collect();
    let mut fits: Vec<(f64, f64)> = Vec::new();
    for k in 0..STEPS {
        let (prev, next) = (coarse[(k + STEPS - 1) % STEPS], coarse[(k + 1) % STEPS]);
        if coarse[k] > prev || coarse[k] > next {
            continue;
        }
        let mut best = (step_angle(k), coarse[k]);
        for step in [1.0_f64, 0.2, 0.05, 0.01] {
            let centre = best.0;
            for j in -6..=6 {
                let angle = centre + f64::from(j) * step.to_radians();
                let score = score_at(angle);
                if score < best.1 {
                    best = (angle, score);
                }
            }
        }
        let angle = best.0.rem_euclid(std::f64::consts::TAU);
        if !fits
            .iter()
            .any(|(a, _): &(f64, f64)| (a - angle).abs() < 0.5_f64.to_radians())
        {
            fits.push((angle, best.1));
        }
    }
    fits.sort_by(|a, b| a.1.total_cmp(&b.1));
    fits
}

/// The bounding box of the placed slab, `[[west, south], [east, north]]`.
pub fn placed_bounds(model: &Model, placement: &Placement) -> Option<[[f64; 2]; 2]> {
    let slab = &model.floors.first()?.slab;
    let rect = geom::from_polygon2d(slab).bounding_rect()?;
    let corners = [
        Point::new(rect.min().x, rect.min().y),
        Point::new(rect.max().x, rect.min().y),
        Point::new(rect.max().x, rect.max().y),
        Point::new(rect.min().x, rect.max().y),
    ];
    let mut west = f64::INFINITY;
    let mut south = f64::INFINITY;
    let mut east = f64::NEG_INFINITY;
    let mut north = f64::NEG_INFINITY;
    for corner in corners {
        let p = placement.to_wgs84(corner);
        west = west.min(p.lon);
        east = east.max(p.lon);
        south = south.min(p.lat);
        north = north.max(p.lat);
    }
    Some([[west, south], [east, north]])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Floor;
    use indexmap::IndexMap;
    use pdf_classify::{Confidence, PageClass};
    use pdf_geo::geojson::ExportSummary;
    use pdf_vector::report::{Classification, Features};
    use pdf_vector::{GeoreferenceReport, PageVector};

    fn polygon(points: &[(f64, f64)]) -> Polygon2D {
        Polygon2D {
            outer: points.iter().map(|p| [p.0, p.1]).collect(),
            holes: Vec::new(),
        }
    }

    /// An L-shaped 10 x 6 slab with a 4 x 2 notch.
    fn l_slab() -> Polygon2D {
        polygon(&[
            (0.0, 0.0),
            (10.0, 0.0),
            (10.0, 6.0),
            (4.0, 6.0),
            (4.0, 8.0),
            (0.0, 8.0),
        ])
    }

    /// A 10 x 6 box.
    fn box_envelope() -> Polygon2D {
        polygon(&[(0.0, 0.0), (10.0, 0.0), (10.0, 6.0), (0.0, 6.0)])
    }

    /// The box with a 3 x 3 terrace on its east side.
    fn box_with_terrace() -> Polygon2D {
        polygon(&[
            (0.0, 0.0),
            (10.0, 0.0),
            (10.0, 1.5),
            (13.0, 1.5),
            (13.0, 4.5),
            (10.0, 4.5),
            (10.0, 6.0),
            (0.0, 6.0),
        ])
    }

    fn model(envelope: Polygon2D, slab: Polygon2D) -> Model {
        Model {
            units: "m".into(),
            up: "z".into(),
            source: "t".into(),
            scale: "1:75".into(),
            levels: IndexMap::new(),
            floors: vec![Floor {
                name: "g".into(),
                page: 7,
                z: 0.0,
                top: 3.6,
                envelope,
                slab,
                slab_z: [-0.35, 0.0],
                walls: Vec::new(),
                glazing: Vec::new(),
                glazing_z: [0.0, 3.1],
                door_gaps: Vec::new(),
                rooms: Vec::new(),
                rails: Vec::new(),
                edge: None,
            }],
            roof: None,
            stone: Vec::new(),
        }
    }

    /// `ring` (metres, about its centroid) rotated by `angle` and drawn on
    /// the ground about (44.5, 40.09), with `extras` as loose lines.
    fn placed_page(
        ring: &Polygon2D,
        angle: f64,
        kind: Option<&str>,
        extras: &[Vec<(f64, f64)>],
    ) -> PageVector {
        let (per_lon, per_lat) = metres_per_degree(40.09);
        let outline = ring_of(ring);
        let (_, c) = centre(&outline);
        let ground = |points: &[(f64, f64)]| -> Vec<Value> {
            rotate(
                &points
                    .iter()
                    .map(|p| (p.0 - c.0, p.1 - c.1))
                    .collect::<Vec<_>>(),
                angle,
            )
            .iter()
            .map(|&(x, y)| serde_json::json!([44.5 + x / per_lon, 40.09 + y / per_lat]))
            .collect()
        };
        let mut ring_json = ground(&outline);
        ring_json.push(ring_json[0].clone());
        let mut properties = serde_json::Map::new();
        if let Some(kind) = kind {
            properties.insert("kind".into(), kind.into());
        }
        let mut features = vec![serde_json::json!({
            "type": "Feature",
            "geometry": {"type": "Polygon", "coordinates": [ring_json]},
            "properties": properties,
        })];
        for extra in extras {
            features.push(serde_json::json!({
                "type": "Feature",
                "geometry": {"type": "LineString", "coordinates": ground(extra)},
                "properties": {},
            }));
        }
        let collection: geojson::FeatureCollection = serde_json::from_value(serde_json::json!({
            "type": "FeatureCollection",
            "features": features,
        }))
        .unwrap();
        PageVector {
            page_number: 12,
            classification: Classification {
                class: PageClass::SitePlan,
                confidence: Confidence::Certain,
                reason: "t".into(),
                is_drawing: true,
                is_mappable: true,
                features: Features {
                    path_count: 1,
                    text_count: 0,
                    image_count: 0,
                    image_coverage: 0.0,
                    vector_coverage: 0.0,
                    line_density: 0.0,
                    distinct_colors: 1,
                    layer_count: 0,
                    cadastral_codes: 0,
                    declared_scales: Vec::new(),
                    has_control_table: false,
                    control_rows: 0,
                    sheet_title: None,
                    sheet_role: None,
                    sheet_level_m: None,
                    level_marks: 0,
                    filled_paths: 0,
                },
            },
            georeference: Some(GeoreferenceReport {
                placed: true,
                source: None,
                failure: None,
                crs: None,
                rmse_m: Some(0.01),
                max_residual_m: None,
                control_points_used: None,
                fitted_scale_denominator: None,
                declared_scale_denominator: None,
                rotation_degrees: None,
                center: None,
            }),
            crs: "EPSG:4326".into(),
            summary: ExportSummary {
                features: 1,
                polygons: 1,
                linestrings: 0,
                dropped_out_of_roi: 0,
                dropped_too_small: 0,
                dropped_furniture: 0,
                truncated: false,
            },
            geojson: collection,
        }
    }

    fn vector(pages: Vec<PageVector>) -> DocumentVector {
        DocumentVector {
            document: "t".into(),
            source: "t.pdf".into(),
            page_count: 20,
            pages,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn the_envelope_is_matched_to_a_rotated_box_on_a_placed_sheet() {
        let model = model(l_slab(), l_slab());
        let placed = from_document(
            &model,
            &vector(vec![placed_page(&l_slab(), 0.6, None, &[])]),
        )
        .expect("the outline matches");
        assert_eq!(placed.source, ModelPlacementSource::SheetOutline);
        assert_eq!(placed.page, Some(12));
        assert!(
            placed.residual_m < 0.02,
            "an exact copy: {}",
            placed.residual_m
        );
        assert!(
            (placed.placement.rotation_degrees - 0.6_f64.to_degrees()).abs() < 0.2,
            "{}",
            placed.placement.rotation_degrees
        );
        // The envelope's centroid lands on the drawn box's.
        let c = ring_centroid(&ring_of(&l_slab()));
        let on_ground = placed.placement.to_wgs84(Point::new(c.0, c.1));
        assert!(
            (on_ground.lon - 44.5).abs() < 1e-7 && (on_ground.lat - 40.09).abs() < 1e-7,
            "{on_ground:?}"
        );
    }

    #[test]
    fn a_symmetric_box_is_turned_the_way_the_line_work_says() {
        // The sheet draws the box and, as a loose line, the terrace's far
        // edge; only the rotation that puts the slab's terrace on that line
        // agrees with it.
        let model = model(box_envelope(), box_with_terrace());
        let terrace_edge = vec![(13.0, 1.5), (13.0, 4.5)];
        let angle = 0.6;
        let placed = from_document(
            &model,
            &vector(vec![placed_page(
                &box_envelope(),
                angle,
                None,
                &[terrace_edge],
            )]),
        )
        .expect("the box matches");
        assert!(placed.residual_m < 0.02);
        let found = placed.placement.rotation_degrees.rem_euclid(360.0);
        assert!(
            (found - angle.to_degrees()).abs() < 0.5,
            "the terrace side, not its mirror: {found}"
        );
        assert!(
            placed.linework_residual_m.is_some_and(|d| d < 0.5),
            "{:?}",
            placed.linework_residual_m
        );
    }

    #[test]
    fn the_parcel_ring_is_not_a_building_outline() {
        let model = model(l_slab(), l_slab());
        assert!(
            from_document(
                &model,
                &vector(vec![placed_page(
                    &l_slab(),
                    0.0,
                    Some("survey_parcel"),
                    &[]
                )])
            )
            .is_none()
        );
    }

    #[test]
    fn nothing_placed_means_no_placement() {
        assert!(from_document(&model(l_slab(), l_slab()), &vector(Vec::new())).is_none());
        assert!(from_references(&[]).is_none());
    }
}
