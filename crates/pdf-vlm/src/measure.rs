//! Deterministic site measurements over a reconstruction: every side
//! length and every clearance a reviewer would otherwise scale off the
//! drawing. Pure geometry over the parcel ring and the realized part
//! footprints — nothing here comes from a model.
//!
//! The output is a GeoJSON `FeatureCollection` in the anchor sheet's own
//! frame, so it draws on the same map as the extrusions: the parcel as a
//! polygon, each measured length as the two-point `LineString` it was
//! measured along, labelled in metres. Features are typed by `kind` so a
//! map can toggle them separately: `parcel`, `side` (parcel and footprint
//! edges), `setback` (a building to each parcel side), `clearance`
//! (building to building, building to fence).

use serde::Serialize;
use serde_json::{Value, json};

/// A closed ring of `(x, y)` in the anchor frame, first point repeated last.
pub type Ring = Vec<(f64, f64)>;

/// One structure whose footprint takes part in the measurements.
pub struct Body {
    pub name: String,
    /// `block` or `fence`, as in the part report.
    pub kind: &'static str,
    pub ring: Ring,
}

/// The parcel the measurements are taken against.
pub struct Parcel {
    /// `survey_parcel` (the placed sheet's turning points),
    /// `reference_parcel` (the caller's `--parcel` polygon the sheet was
    /// matched to), `fence` (the declared perimeter fence's ring) or
    /// `enclosing_ring` (the largest candidate that contains every building).
    pub source: &'static str,
    pub ring: Ring,
}

/// What the report says about the parcel, beside the collection.
#[derive(Debug, Serialize)]
pub struct ParcelReport {
    pub source: String,
    pub area_m2: f64,
    pub perimeter_m: f64,
    pub sides: usize,
}

/// Measures everything and returns the collection plus the parcel summary.
/// `geographic` says the frame is longitude/latitude; otherwise it is
/// metres already.
pub fn measure(parcel: &Parcel, bodies: &[Body], geographic: bool) -> (Value, ParcelReport) {
    let frame = Frame::about(&parcel.ring, geographic);
    let parcel_m = frame.to_metres(&parcel.ring);
    let (area_m2, perimeter_m) = (
        round2(ring_area(&parcel_m, false)),
        round2(perimeter(&parcel_m)),
    );
    let mut features = Vec::new();

    features.push(json!({
        "type": "Feature",
        "geometry": {"type": "Polygon", "coordinates": [ring_json(&parcel.ring)]},
        "properties": {
            "kind": "parcel",
            "source": parcel.source,
            "area_m2": area_m2,
            "perimeter_m": perimeter_m,
        },
    }));
    features.extend(sides(&frame, &parcel_m, "parcel"));

    let blocks: Vec<(&Body, Ring)> = bodies
        .iter()
        .filter(|b| b.kind == "block")
        .map(|b| (b, frame.to_metres(&b.ring)))
        .collect();
    for (body, ring_m) in &blocks {
        features.extend(sides(&frame, ring_m, &body.name));
        for (index, side) in parcel_m.windows(2).enumerate() {
            let (length, from, to) = ring_to_segment(ring_m, side[0], side[1]);
            features.push(line(
                &frame,
                from,
                to,
                length,
                json!({"kind": "setback", "from": body.name, "to": "parcel", "side": index}),
            ));
        }
    }
    for (i, (a, ring_a)) in blocks.iter().enumerate() {
        for (b, ring_b) in blocks.iter().skip(i + 1) {
            let (length, from, to) = ring_to_ring(ring_a, ring_b);
            features.push(line(
                &frame,
                from,
                to,
                length,
                json!({"kind": "clearance", "from": a.name, "to": b.name}),
            ));
        }
    }
    // A fence that *is* the parcel ring adds nothing the setbacks do not say.
    for fence in bodies
        .iter()
        .filter(|b| b.kind == "fence" && b.ring != parcel.ring)
    {
        let fence_m = frame.to_metres(&fence.ring);
        for (block, ring_m) in &blocks {
            let (length, from, to) = ring_to_ring(ring_m, &fence_m);
            features.push(line(
                &frame,
                from,
                to,
                length,
                json!({"kind": "clearance", "from": block.name, "to": fence.name}),
            ));
        }
    }

    let report = ParcelReport {
        source: parcel.source.to_string(),
        area_m2,
        perimeter_m,
        sides: parcel_m.len().saturating_sub(1),
    };
    let collection = json!({
        "type": "FeatureCollection",
        "features": features,
        "metadata": {
            "note": "deterministic geometry: parcel and footprint side lengths, each \
                     building's distance to every parcel side, and building-to-building \
                     / building-to-fence clearances; lengths in metres, measured along \
                     the drawn LineString",
        },
    });
    (collection, report)
}

/// Whether `point` lies inside the closed `ring` (ray casting).
pub fn contains(ring: &[(f64, f64)], point: (f64, f64)) -> bool {
    let mut inside = false;
    for pair in ring.windows(2) {
        let ((x1, y1), (x2, y2)) = (pair[0], pair[1]);
        if (y1 > point.1) != (y2 > point.1) {
            let x = x1 + (point.1 - y1) * (x2 - x1) / (y2 - y1);
            if point.0 < x {
                inside = !inside;
            }
        }
    }
    inside
}

/// A local metric frame about a ring's centroid: degrees become metres by
/// an equirectangular projection at that latitude, metres stay metres.
struct Frame {
    origin: (f64, f64),
    scale: (f64, f64),
}

impl Frame {
    fn about(ring: &[(f64, f64)], geographic: bool) -> Self {
        let origin = centroid(ring);
        Frame {
            origin,
            scale: scale_at(origin.1, geographic),
        }
    }

    fn to_metres(&self, ring: &[(f64, f64)]) -> Ring {
        ring.iter()
            .map(|&(x, y)| {
                (
                    (x - self.origin.0) * self.scale.0,
                    (y - self.origin.1) * self.scale.1,
                )
            })
            .collect()
    }

    fn to_frame(&self, (x, y): (f64, f64)) -> [f64; 2] {
        [
            round7(self.origin.0 + x / self.scale.0),
            round7(self.origin.1 + y / self.scale.1),
        ]
    }
}

/// Metres per degree of longitude and of latitude at `latitude` (degrees),
/// from the standard WGS 84 series — good to the centimetre over a parcel,
/// where a spherical constant is half a percent out.
pub fn metres_per_degree(latitude: f64) -> (f64, f64) {
    let phi = latitude.to_radians();
    let per_lat = 111_132.954 - 559.822 * (2.0 * phi).cos() + 1.175 * (4.0 * phi).cos();
    let per_lon = 111_412.84 * phi.cos() - 93.5 * (3.0 * phi).cos() + 0.118 * (5.0 * phi).cos();
    (per_lon, per_lat)
}

/// One `side` feature per edge of a metric ring.
fn sides(frame: &Frame, ring_m: &[(f64, f64)], of: &str) -> Vec<Value> {
    ring_m
        .windows(2)
        .enumerate()
        .map(|(index, pair)| {
            let length = distance(pair[0], pair[1]);
            line(
                frame,
                pair[0],
                pair[1],
                length,
                json!({"kind": "side", "of": of, "index": index}),
            )
        })
        .collect()
}

fn line(
    frame: &Frame,
    from: (f64, f64),
    to: (f64, f64),
    length: f64,
    mut properties: Value,
) -> Value {
    properties["length_m"] = round2(length).into();
    properties["label"] = format!("{:.2} m", length).into();
    json!({
        "type": "Feature",
        "geometry": {
            "type": "LineString",
            "coordinates": [frame.to_frame(from), frame.to_frame(to)],
        },
        "properties": properties,
    })
}

fn ring_json(ring: &[(f64, f64)]) -> Vec<[f64; 2]> {
    ring.iter().map(|&(x, y)| [round7(x), round7(y)]).collect()
}

fn distance(a: (f64, f64), b: (f64, f64)) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

/// The point of segment `ab` nearest `p`, and the distance to it.
fn nearest_on_segment(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> (f64, (f64, f64)) {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let length_sq = dx * dx + dy * dy;
    let t = if length_sq <= 1e-12 {
        0.0
    } else {
        (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / length_sq).clamp(0.0, 1.0)
    };
    let nearest = (a.0 + t * dx, a.1 + t * dy);
    (distance(p, nearest), nearest)
}

/// Shortest link between a ring and a segment: `(length, on ring, on
/// segment)`. Zero when they cross.
fn ring_to_segment(
    ring: &[(f64, f64)],
    a: (f64, f64),
    b: (f64, f64),
) -> (f64, (f64, f64), (f64, f64)) {
    let mut best = (f64::INFINITY, (0.0, 0.0), (0.0, 0.0));
    for &vertex in ring.iter().take(ring.len().saturating_sub(1)) {
        let (d, on_segment) = nearest_on_segment(vertex, a, b);
        if d < best.0 {
            best = (d, vertex, on_segment);
        }
    }
    for edge in ring.windows(2) {
        for endpoint in [a, b] {
            let (d, on_ring) = nearest_on_segment(endpoint, edge[0], edge[1]);
            if d < best.0 {
                best = (d, on_ring, endpoint);
            }
        }
        if segments_cross(edge[0], edge[1], a, b) {
            let crossing = nearest_on_segment(edge[0], a, b).1;
            return (0.0, crossing, crossing);
        }
    }
    best
}

/// Shortest link between two rings: `(length, on first, on second)`.
fn ring_to_ring(first: &[(f64, f64)], second: &[(f64, f64)]) -> (f64, (f64, f64), (f64, f64)) {
    let mut best = (f64::INFINITY, (0.0, 0.0), (0.0, 0.0));
    for edge in second.windows(2) {
        let (d, on_first, on_second) = ring_to_segment(first, edge[0], edge[1]);
        if d < best.0 {
            best = (d, on_first, on_second);
        }
    }
    best
}

fn segments_cross(p1: (f64, f64), p2: (f64, f64), q1: (f64, f64), q2: (f64, f64)) -> bool {
    let orient = |a: (f64, f64), b: (f64, f64), c: (f64, f64)| {
        (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0)
    };
    let (o1, o2) = (orient(p1, p2, q1), orient(p1, p2, q2));
    let (o3, o4) = (orient(q1, q2, p1), orient(q1, q2, p2));
    (o1 * o2 < 0.0) && (o3 * o4 < 0.0)
}

fn perimeter(ring: &[(f64, f64)]) -> f64 {
    ring.windows(2).map(|pair| distance(pair[0], pair[1])).sum()
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

pub(crate) fn ring_points(ring: &[Value]) -> Option<Vec<(f64, f64)>> {
    let points: Vec<(f64, f64)> = ring
        .iter()
        .filter_map(|p| Some((p[0].as_f64()?, p[1].as_f64()?)))
        .collect();
    (points.len() == ring.len()).then_some(points)
}

/// Shoelace area of one closed ring. Longitude/latitude rings are measured
/// in metres via an equirectangular projection about the ring's own
/// latitude — exact enough at building scale; a footprint is not a country.
pub(crate) fn ring_area(points: &[(f64, f64)], geographic: bool) -> f64 {
    let lat = points.iter().map(|p| p.1).sum::<f64>() / points.len().max(1) as f64;
    let (sx, sy) = scale_at(lat, geographic);
    let mut doubled = 0.0_f64;
    for pair in points.windows(2) {
        let (x1, y1) = (pair[0].0 * sx, pair[0].1 * sy);
        let (x2, y2) = (pair[1].0 * sx, pair[1].1 * sy);
        doubled += x1 * y2 - x2 * y1;
    }
    (doubled / 2.0).abs()
}

pub(crate) fn centroid(points: &[(f64, f64)]) -> (f64, f64) {
    let n = points.len().saturating_sub(1).max(1) as f64;
    let (sum_x, sum_y) = points
        .iter()
        .take(points.len().saturating_sub(1).max(1))
        .fold((0.0, 0.0), |(x, y), p| (x + p.0, y + p.1));
    (sum_x / n, sum_y / n)
}

pub(crate) fn round7(value: f64) -> f64 {
    (value * 1e7).round() / 1e7
}

/// Metres per unit of the frame: per degree at `latitude` when the frame is
/// geographic, one otherwise.
pub(crate) fn scale_at(latitude: f64, geographic: bool) -> (f64, f64) {
    if geographic {
        metres_per_degree(latitude)
    } else {
        (1.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rectangle(x0: f64, y0: f64, x1: f64, y1: f64) -> Ring {
        vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1), (x0, y0)]
    }

    fn features_of(collection: &Value, kind: &str) -> Vec<Value> {
        collection["features"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|f| f["properties"]["kind"] == kind)
            .cloned()
            .collect()
    }

    /// A 20 x 30 m parcel holding two 6 x 8 m houses side by side.
    fn site() -> (Parcel, Vec<Body>) {
        (
            Parcel {
                source: "survey_parcel",
                ring: rectangle(0.0, 0.0, 20.0, 30.0),
            },
            vec![
                Body {
                    name: "house A".into(),
                    kind: "block",
                    ring: rectangle(2.0, 5.0, 8.0, 13.0),
                },
                Body {
                    name: "house B".into(),
                    kind: "block",
                    ring: rectangle(12.0, 5.0, 18.0, 13.0),
                },
            ],
        )
    }

    #[test]
    fn sides_setbacks_and_clearances_come_out_in_metres() {
        let (parcel, bodies) = site();
        let (collection, report) = measure(&parcel, &bodies, false);

        assert_eq!(
            (report.area_m2, report.perimeter_m, report.sides),
            (600.0, 100.0, 4)
        );
        assert_eq!(report.source, "survey_parcel");

        let sides = features_of(&collection, "side");
        let parcel_sides: Vec<f64> = sides
            .iter()
            .filter(|s| s["properties"]["of"] == "parcel")
            .map(|s| s["properties"]["length_m"].as_f64().unwrap())
            .collect();
        assert_eq!(parcel_sides, [20.0, 30.0, 20.0, 30.0]);
        assert_eq!(sides.len(), 4 + 4 + 4, "parcel plus both footprints");

        // House A: 5 m to the south side (index 0), 12 m to the east
        // (index 1), 17 m to the north (index 2), 2 m to the west (index 3).
        let setbacks: Vec<f64> = features_of(&collection, "setback")
            .iter()
            .filter(|s| s["properties"]["from"] == "house A")
            .map(|s| s["properties"]["length_m"].as_f64().unwrap())
            .collect();
        assert_eq!(setbacks, [5.0, 12.0, 17.0, 2.0]);

        let clearances = features_of(&collection, "clearance");
        assert_eq!(clearances.len(), 1);
        assert_eq!(clearances[0]["properties"]["length_m"], 4.0);
        assert_eq!(clearances[0]["properties"]["label"], "4.00 m");
        let ends = clearances[0]["geometry"]["coordinates"].as_array().unwrap();
        assert_eq!(ends[0][0], 8.0, "measured from house A's east wall");
        assert_eq!(ends[1][0], 12.0, "to house B's west wall");

        assert_eq!(features_of(&collection, "parcel").len(), 1);
    }

    #[test]
    fn a_geographic_frame_measures_the_same_site_within_a_percent() {
        let (lon, lat) = (44.5_f64, 40.09_f64);
        let (per_lon, per_lat) = metres_per_degree(lat);
        let (mx, my) = (1.0 / per_lon, 1.0 / per_lat);
        let deg = |ring: Ring| -> Ring {
            ring.iter()
                .map(|&(x, y)| (lon + x * mx, lat + y * my))
                .collect()
        };
        let (parcel, bodies) = site();
        let parcel = Parcel {
            source: "survey_parcel",
            ring: deg(parcel.ring),
        };
        let bodies: Vec<Body> = bodies
            .into_iter()
            .map(|b| Body {
                ring: deg(b.ring),
                ..b
            })
            .collect();
        let (collection, report) = measure(&parcel, &bodies, true);
        assert!((report.area_m2 - 600.0).abs() < 0.05, "{}", report.area_m2);
        let clearance = &features_of(&collection, "clearance")[0];
        let length = clearance["properties"]["length_m"].as_f64().unwrap();
        assert!((length - 4.0).abs() < 0.005, "{length}");
        let ends = clearance["geometry"]["coordinates"].as_array().unwrap();
        assert!(
            (ends[0][0].as_f64().unwrap() - (lon + 8.0 * mx)).abs() < 1e-6,
            "endpoints are written back in degrees"
        );
    }

    #[test]
    fn a_fence_on_its_own_ring_gets_clearances_and_a_boundary_fence_does_not() {
        let (parcel, mut bodies) = site();
        bodies.push(Body {
            name: "yard wall".into(),
            kind: "fence",
            ring: parcel.ring.clone(),
        });
        let (collection, _) = measure(&parcel, &bodies, false);
        assert_eq!(
            features_of(&collection, "clearance").len(),
            1,
            "boundary fence adds nothing"
        );

        bodies.last_mut().unwrap().ring = rectangle(1.0, 1.0, 19.0, 29.0);
        let (collection, _) = measure(&parcel, &bodies, false);
        let to_fence: Vec<f64> = features_of(&collection, "clearance")
            .iter()
            .filter(|c| c["properties"]["to"] == "yard wall")
            .map(|c| c["properties"]["length_m"].as_f64().unwrap())
            .collect();
        assert_eq!(to_fence, [1.0, 1.0]);
    }

    #[test]
    fn crossing_rings_measure_zero_and_containment_is_decided() {
        let a = rectangle(0.0, 0.0, 10.0, 10.0);
        let b = rectangle(5.0, 5.0, 15.0, 15.0);
        assert_eq!(ring_to_ring(&a, &b).0, 0.0);
        assert!(contains(&a, (1.0, 1.0)));
        assert!(!contains(&a, (11.0, 1.0)));
    }
}
