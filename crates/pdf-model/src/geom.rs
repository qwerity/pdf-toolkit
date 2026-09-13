//! Polygon helpers over `geo`: the boolean set operations, buffering and
//! measurements the reconstruction rules are written in, with the repair and
//! filtering steps the reference implementation relied on `shapely` for.

use geo::algorithm::bool_ops::FillRule;
use geo::{
    Area, BooleanOps, Buffer, Coord, Distance, Euclidean, Length, LineString, MultiLineString,
    MultiPolygon, OpType, Polygon, unary_union,
};

use crate::model::{Polygon2D, Ring};

/// Parts smaller than this (m²) are numerical debris, not geometry.
const DEBRIS_AREA: f64 = 1e-4;

/// A polygon from a raw ring as the PDF painted it: normalised through the
/// overlay engine under the non-zero winding rule (`f`, the operator CAD
/// exporters use), so a self-touching or self-crossing outline comes back
/// as the valid parts the page actually shows filled.
pub fn polygon_from_ring(points: &[(f64, f64)]) -> Vec<Polygon<f64>> {
    if points.len() < 3 {
        return Vec::new();
    }
    let ring = LineString::from(
        points
            .iter()
            .map(|&(x, y)| Coord { x, y })
            .collect::<Vec<_>>(),
    );
    let raw = Polygon::new(ring, Vec::new());
    parts(raw.boolean_op_with_fill_rule(&raw, OpType::Union, FillRule::NonZero))
}

/// The polygons of a multipolygon, debris dropped.
pub fn parts(multi: MultiPolygon<f64>) -> Vec<Polygon<f64>> {
    multi
        .0
        .into_iter()
        .filter(|p| p.unsigned_area() > DEBRIS_AREA)
        .collect()
}

pub fn union_all<'a>(polys: impl IntoIterator<Item = &'a Polygon<f64>>) -> MultiPolygon<f64> {
    let polys: Vec<&Polygon<f64>> = polys.into_iter().collect();
    if polys.is_empty() {
        return MultiPolygon::new(Vec::new());
    }
    unary_union(polys)
}

pub fn largest(multi: MultiPolygon<f64>) -> Option<Polygon<f64>> {
    parts(multi)
        .into_iter()
        .max_by(|a, b| a.unsigned_area().total_cmp(&b.unsigned_area()))
}

/// The polygon's exterior alone, holes discarded.
pub fn exterior_only(polygon: &Polygon<f64>) -> Polygon<f64> {
    Polygon::new(polygon.exterior().clone(), Vec::new())
}

/// Erodes then dilates by `distance`: drops slivers thinner than twice it.
pub fn open(multi: &MultiPolygon<f64>, distance: f64) -> MultiPolygon<f64> {
    multi.buffer(-distance).buffer(distance)
}

/// Dilates then erodes: closes gaps narrower than twice `distance`.
pub fn close(multi: &MultiPolygon<f64>, distance: f64) -> MultiPolygon<f64> {
    multi.buffer(distance).buffer(-distance)
}

pub fn intersection_area(a: &Polygon<f64>, b: &impl BooleanOps<Scalar = f64>) -> f64 {
    a.intersection(b).unsigned_area()
}

pub fn polygon_distance(a: &Polygon<f64>, b: &Polygon<f64>) -> f64 {
    Euclidean.distance(a, b)
}

pub fn ring_distance(a: &LineString<f64>, b: &LineString<f64>) -> f64 {
    Euclidean.distance(a, b)
}

/// The parts of `line` outside `polygon`.
pub fn line_outside(line: &LineString<f64>, polygon: &Polygon<f64>) -> Vec<LineString<f64>> {
    polygon
        .clip(&MultiLineString::new(vec![line.clone()]), true)
        .0
}

pub fn length(line: &LineString<f64>) -> f64 {
    Euclidean.length(line)
}

/// Area of a ring given as points, closed or not (the shoelace, unsigned).
pub fn ring_area(points: &[(f64, f64)]) -> f64 {
    let n = points.len();
    (0..n)
        .map(|i| {
            let (a, b) = (points[i], points[(i + 1) % n]);
            a.0 * b.1 - b.0 * a.1
        })
        .sum::<f64>()
        .abs()
        / 2.0
}

/// `v` rounded to `decimals` places.
pub fn round(v: f64, decimals: i32) -> f64 {
    let factor = 10f64.powi(decimals);
    (v * factor).round() / factor
}

pub fn rotate(points: &[(f64, f64)], angle: f64) -> Vec<(f64, f64)> {
    let (sin, cos) = angle.sin_cos();
    points
        .iter()
        .map(|&(x, y)| (x * cos - y * sin, x * sin + y * cos))
        .collect()
}

/// Distance from `p` to the segment `a`–`b`.
pub fn point_segment_distance(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let length_sq = dx * dx + dy * dy;
    let s = if length_sq <= 1e-12 {
        0.0
    } else {
        (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / length_sq).clamp(0.0, 1.0)
    };
    ((p.0 - a.0 - s * dx).powi(2) + (p.1 - a.1 - s * dy).powi(2)).sqrt()
}

/// A thin polygon along a polyline, square-capped: how a railing is drawn.
pub fn stroke(line: &LineString<f64>, half_width: f64) -> MultiPolygon<f64> {
    use geo::algorithm::buffer::{BufferStyle, LineCap, LineJoin};
    line.buffer_with_style(
        BufferStyle::new(half_width)
            .line_cap(LineCap::Square)
            .line_join(LineJoin::Miter(2.0)),
    )
}

/// A ring as output coordinates: the closing repeat dropped, values rounded
/// to a tenth of a millimetre, orientation forced (`ccw` for outers).
fn ring_out(line: &LineString<f64>, ccw: bool) -> Ring {
    let mut coords: Vec<Coord<f64>> = line.0.clone();
    if coords.len() > 1 && coords.first() == coords.last() {
        coords.pop();
    }
    let signed: f64 = coords
        .iter()
        .zip(coords.iter().cycle().skip(1))
        .map(|(a, b)| a.x * b.y - b.x * a.y)
        .sum();
    if (signed > 0.0) != ccw {
        coords.reverse();
    }
    coords
        .into_iter()
        .map(|c| [round(c.x, 4), round(c.y, 4)])
        .collect()
}

pub fn to_polygon2d(polygon: &Polygon<f64>) -> Polygon2D {
    Polygon2D {
        outer: ring_out(polygon.exterior(), true),
        holes: polygon
            .interiors()
            .iter()
            .map(|hole| ring_out(hole, false))
            .collect(),
    }
}

pub fn line_out(line: &LineString<f64>) -> Ring {
    line.0
        .iter()
        .map(|c| [round(c.x, 4), round(c.y, 4)])
        .collect()
}

/// Back from the output schema into `geo`, for the mesh and GeoJSON writers.
pub fn from_polygon2d(polygon: &Polygon2D) -> Polygon<f64> {
    Polygon::new(
        ring_in(&polygon.outer),
        polygon.holes.iter().map(ring_in).collect(),
    )
}

pub fn ring_in(ring: &Ring) -> LineString<f64> {
    LineString::from(
        ring.iter()
            .map(|p| Coord { x: p[0], y: p[1] })
            .collect::<Vec<_>>(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x: f64, y: f64, side: f64) -> Polygon<f64> {
        polygon_from_ring(&[(x, y), (x + side, y), (x + side, y + side), (x, y + side)])
            .pop()
            .unwrap()
    }

    #[test]
    fn a_bow_tie_ring_is_repaired_into_parts() {
        let parts = polygon_from_ring(&[(0.0, 0.0), (2.0, 2.0), (2.0, 0.0), (0.0, 2.0)]);
        assert_eq!(parts.len(), 2, "two triangles: {parts:?}");
        let total: f64 = parts.iter().map(|p| p.unsigned_area()).sum();
        assert!((total - 2.0).abs() < 1e-9);
    }

    #[test]
    fn open_drops_a_sliver_and_keeps_the_body() {
        // A 5 mm wide strip sticking half a metre out of the body.
        let body = square(0.0, 0.0, 1.0);
        let sliver = polygon_from_ring(&[(1.0, 0.4), (1.5, 0.4), (1.5, 0.405), (1.0, 0.405)])
            .pop()
            .unwrap();
        let joined = union_all([&body, &sliver]);
        let opened = open(&joined, 0.01);
        let area = opened.unsigned_area();
        assert!((area - 1.0).abs() < 1e-3, "sliver gone, body kept: {area}");
    }

    #[test]
    fn output_rings_are_ccw_without_the_closing_point() {
        let cw = polygon_from_ring(&[(0.0, 0.0), (0.0, 1.0), (1.0, 1.0), (1.0, 0.0)])
            .pop()
            .unwrap();
        let out = to_polygon2d(&cw);
        assert_eq!(out.outer.len(), 4);
        let signed: f64 = out
            .outer
            .iter()
            .zip(out.outer.iter().cycle().skip(1))
            .map(|(a, b)| a[0] * b[1] - b[0] * a[1])
            .sum();
        assert!(signed > 0.0);
        assert_eq!(from_polygon2d(&out).unsigned_area(), 1.0);
    }

    #[test]
    fn line_outside_keeps_what_the_polygon_does_not_cover() {
        let poly = square(0.0, 0.0, 1.0);
        let line = LineString::from(vec![(-1.0, 0.5), (2.0, 0.5)]);
        let outside = line_outside(&line, &poly);
        let total: f64 = outside.iter().map(length).sum();
        assert!(
            (total - 2.0).abs() < 1e-9,
            "one unit inside, two outside: {total}"
        );
    }
}
