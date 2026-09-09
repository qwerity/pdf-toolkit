//! Reassembly of fragmented CAD line work into polylines.
//!
//! Cadastral exports frequently emit every boundary as an isolated two-point
//! segment, so a parcel outline exists in the file only as an unordered soup of
//! edges. Shape-based georeferencing needs the outline back, which means
//! rebuilding the segment graph and walking it.

use std::collections::HashMap;

use pdf_geometry::model::{Point, VectorPath};

/// A polyline recovered from the segment graph.
#[derive(Debug, Clone)]
pub struct Chain {
    pub points: Vec<Point>,
    pub closed: bool,
}

/// Tolerances for rebuilding polylines.
#[derive(Debug, Clone, Copy)]
pub struct ChainOptions {
    /// Endpoints closer than this, in page units, are the same node.
    pub weld_tolerance: f64,
    /// At a junction — a node where more than two segments meet — the largest
    /// turn, in degrees, that still counts as continuing the same line. A
    /// boundary split by a neighbouring parcel meeting it mid-span continues
    /// almost straight, so a small limit picks the right branch. Nodes where
    /// exactly two segments meet are always followed, since that is what a
    /// parcel corner looks like.
    pub max_continuation_turn_deg: f64,
    /// Douglas–Peucker tolerance applied to each finished chain, which removes
    /// the intermediate nodes left behind by those junctions.
    pub simplify_tolerance: f64,
    /// Bound on the number of edges considered, so a hatched drawing cannot
    /// make assembly unbounded.
    pub max_edges: usize,
}

impl Default for ChainOptions {
    fn default() -> Self {
        Self {
            weld_tolerance: 0.05,
            max_continuation_turn_deg: 60.0,
            simplify_tolerance: 0.3,
            max_edges: 400_000,
        }
    }
}

/// Rebuilds polylines from every segment on the page.
pub fn assemble_chains(paths: &[VectorPath], options: ChainOptions) -> Vec<Chain> {
    let mut graph = Graph::new(options.weld_tolerance);
    for path in paths {
        for sub in &path.subpaths {
            for pair in sub.points.windows(2) {
                if graph.edges.len() >= options.max_edges {
                    break;
                }
                graph.add_edge(pair[0], pair[1]);
            }
            if sub.closed && sub.points.len() > 2 {
                graph.add_edge(sub.points[sub.points.len() - 1], sub.points[0]);
            }
        }
    }
    graph.walk(options)
}

struct Edge {
    a: usize,
    b: usize,
}

struct Graph {
    tolerance: f64,
    nodes: Vec<Point>,
    lookup: HashMap<(i64, i64), usize>,
    edges: Vec<Edge>,
    seen: std::collections::HashSet<(usize, usize)>,
    incident: Vec<Vec<usize>>,
}

impl Graph {
    fn new(tolerance: f64) -> Self {
        Self {
            tolerance,
            nodes: Vec::new(),
            lookup: HashMap::new(),
            edges: Vec::new(),
            seen: std::collections::HashSet::new(),
            incident: Vec::new(),
        }
    }

    fn node(&mut self, p: Point) -> usize {
        let key = (
            (p.x / self.tolerance).round() as i64,
            (p.y / self.tolerance).round() as i64,
        );
        if let Some(&id) = self.lookup.get(&key) {
            return id;
        }
        let id = self.nodes.len();
        self.nodes.push(p);
        self.incident.push(Vec::new());
        self.lookup.insert(key, id);
        id
    }

    fn add_edge(&mut self, from: Point, to: Point) {
        if from.distance(&to) <= self.tolerance {
            return;
        }
        let (a, b) = (self.node(from), self.node(to));
        if a == b {
            return;
        }
        let key = (a.min(b), a.max(b));
        if !self.seen.insert(key) {
            return;
        }
        let id = self.edges.len();
        self.edges.push(Edge { a, b });
        self.incident[a].push(id);
        self.incident[b].push(id);
    }

    fn other(&self, edge: usize, node: usize) -> usize {
        let e = &self.edges[edge];
        if e.a == node { e.b } else { e.a }
    }

    /// Greedily grows each unused edge into the longest polyline that continues
    /// without an ambiguous turn.
    fn walk(&self, options: ChainOptions) -> Vec<Chain> {
        let max_turn = options.max_continuation_turn_deg.to_radians();
        let mut used = vec![false; self.edges.len()];
        let mut chains = Vec::new();

        for seed in 0..self.edges.len() {
            if used[seed] {
                continue;
            }
            used[seed] = true;
            let Edge { a, b } = self.edges[seed];
            let mut forward = self.extend(b, a, &mut used, max_turn);
            let backward = self.extend(a, b, &mut used, max_turn);

            let mut points: Vec<Point> = backward.iter().rev().map(|&n| self.nodes[n]).collect();
            points.push(self.nodes[a]);
            points.push(self.nodes[b]);
            points.extend(forward.drain(..).map(|n| self.nodes[n]));

            let closed =
                points.len() > 3 && points[0].distance(&points[points.len() - 1]) <= self.tolerance;
            if closed {
                points.pop();
            }
            let points = simplify(&points, options.simplify_tolerance);
            if points.len() >= 2 {
                chains.push(Chain { points, closed });
            }
        }
        chains
    }

    /// Walks outward from `from` (arrived at from `previous`), consuming edges.
    fn extend(&self, from: usize, previous: usize, used: &mut [bool], max_turn: f64) -> Vec<usize> {
        let mut out = Vec::new();
        let (mut current, mut prior) = (from, previous);
        loop {
            let incoming = direction(self.nodes[prior], self.nodes[current]);
            let unambiguous = self.incident[current].len() <= 2;
            let mut best: Option<(f64, usize, usize)> = None;
            for &edge in &self.incident[current] {
                if used[edge] {
                    continue;
                }
                let next = self.other(edge, current);
                let turn =
                    angle_between(incoming, direction(self.nodes[current], self.nodes[next]));
                if !unambiguous && turn > max_turn {
                    continue;
                }
                if best.as_ref().is_none_or(|(t, _, _)| turn < *t) {
                    best = Some((turn, edge, next));
                }
            }
            let Some((_, edge, next)) = best else {
                return out;
            };
            used[edge] = true;
            out.push(next);
            if next == from || out.len() > 4096 {
                return out;
            }
            prior = current;
            current = next;
        }
    }
}

fn direction(a: Point, b: Point) -> f64 {
    (b.y - a.y).atan2(b.x - a.x)
}

fn angle_between(a: f64, b: f64) -> f64 {
    let mut d = (b - a).abs() % std::f64::consts::TAU;
    if d > std::f64::consts::PI {
        d = std::f64::consts::TAU - d;
    }
    d
}

/// Douglas–Peucker, used here to drop the collinear nodes introduced wherever a
/// neighbouring boundary meets a line mid-span.
fn simplify(points: &[Point], tolerance: f64) -> Vec<Point> {
    if points.len() <= 2 || tolerance <= 0.0 {
        return points.to_vec();
    }
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;
    let mut stack = vec![(0usize, points.len() - 1)];
    while let Some((start, end)) = stack.pop() {
        if end <= start + 1 {
            continue;
        }
        let (a, b) = (points[start], points[end]);
        let (mut best, mut best_d) = (start, 0.0);
        for (i, p) in points.iter().enumerate().take(end).skip(start + 1) {
            let d = perpendicular_distance(*p, a, b);
            if d > best_d {
                best = i;
                best_d = d;
            }
        }
        if best_d > tolerance {
            keep[best] = true;
            stack.push((start, best));
            stack.push((best, end));
        }
    }
    points
        .iter()
        .zip(keep)
        .filter(|(_, k)| *k)
        .map(|(p, _)| *p)
        .collect()
}

fn perpendicular_distance(p: Point, a: Point, b: Point) -> f64 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let denom = (dx * dx + dy * dy).sqrt();
    if denom < 1e-12 {
        return p.distance(&a);
    }
    ((p.x - a.x) * dy - (p.y - a.y) * dx).abs() / denom
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdf_geometry::model::{PaintMode, Rgb, SubPath};

    fn segments(pairs: &[(f64, f64, f64, f64)]) -> Vec<VectorPath> {
        pairs
            .iter()
            .map(|&(x1, y1, x2, y2)| VectorPath {
                subpaths: vec![SubPath {
                    points: vec![Point::new(x1, y1), Point::new(x2, y2)],
                    closed: false,
                }],
                mode: PaintMode::Stroke,
                stroke: Rgb::BLACK,
                fill: Rgb::BLACK,
                width: 0.5,
                layer: None,
            })
            .collect()
    }

    #[test]
    fn rebuilds_a_square_from_loose_segments() {
        let paths = segments(&[
            (0.0, 0.0, 10.0, 0.0),
            (10.0, 10.0, 0.0, 10.0),
            (10.0, 0.0, 10.0, 10.0),
            (0.0, 10.0, 0.0, 0.0),
        ]);
        let chains = assemble_chains(&paths, ChainOptions::default());
        assert_eq!(chains.len(), 1);
        assert!(chains[0].closed);
        assert_eq!(chains[0].points.len(), 4);
    }

    #[test]
    fn merges_collinear_pieces_and_keeps_corners() {
        // A side split in two by a neighbour meeting it mid-span.
        let paths = segments(&[
            (0.0, 0.0, 5.0, 0.0),
            (5.0, 0.0, 10.0, 0.0),
            (10.0, 0.0, 10.0, 10.0),
            (5.0, 0.0, 5.0, -8.0),
        ]);
        let chains = assemble_chains(&paths, ChainOptions::default());
        let main = chains
            .iter()
            .max_by_key(|c| c.points.len())
            .expect("a chain");
        assert_eq!(main.points.len(), 3, "collinear midpoint should be dropped");
        assert_eq!(main.points[0], Point::new(0.0, 0.0));
        assert_eq!(main.points[2], Point::new(10.0, 10.0));
    }

    #[test]
    fn follows_a_corner_where_only_two_segments_meet() {
        let paths = segments(&[(0.0, 0.0, 10.0, 0.0), (10.0, 0.0, 10.0, 10.0)]);
        let chains = assemble_chains(&paths, ChainOptions::default());
        assert_eq!(chains.len(), 1);
        assert_eq!(chains[0].points.len(), 3);
        assert!(!chains[0].closed);
    }
}
