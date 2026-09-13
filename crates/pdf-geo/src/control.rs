//! Discovery of survey control points ("շրջադարձային կետեր" / turning points)
//! and their correspondence with vertices in the drawing.
//!
//! Two independent stages:
//!
//! 1. **Table recovery** — the projected coordinates. The preferred source is
//!    the page's own text layer; some CAD exports convert the table to
//!    outlines, in which case a vision model reads it instead (see
//!    a vision model, where one is wired in).
//! 2. **Anchoring** — pairing those coordinates with positions on the page.
//!    Two routes, both of which feed the same consensus step:
//!    [`shape_correspondences`] finds the drawn parcel by geometry alone, which
//!    is what works when the vertex numbers were converted to outlines, and
//!    [`candidate_correspondences`] matches the printed numbers when they
//!    survived as text.

use std::collections::HashMap;

use regex::Regex;
use serde::{Deserialize, Serialize};

use super::crs::{LonLat, Projected, ProjectedCrs};
use super::fit::{CandidateSource, Correspondence};
use super::topology::{Chain, ChainOptions, assemble_chains};
use pdf_geometry::model::{BBox, PageContent, Point};

/// One row of a control-point table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ControlRow {
    /// Row label as printed, normally the point number.
    pub label: String,
    pub projected: Projected,
}

/// A recovered control-point table plus provenance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlTable {
    pub rows: Vec<ControlRow>,
    pub crs: ProjectedCrs,
    pub source: TableSource,
    /// Page-space extent of the table, when it was found in the text layer.
    pub bbox: Option<BBox>,
    /// Side length printed for the segment leaving each row, in metres, where
    /// the sheet prints one. Parallel to `rows`; the last entry is normally
    /// `None` because there is no segment after the final point.
    ///
    /// Checked against the coordinates by [`super::dimensions`], which is what
    /// catches a digit read wrongly — the georeferencing residual cannot, being
    /// fitted to the same coordinates it scores.
    #[serde(default)]
    pub printed_distances: Vec<Option<f64>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TableSource {
    /// Read directly from the PDF text layer.
    TextLayer,
    /// Read by a vision model from the rendered page.
    Vlm { model: String },
    /// Supplied by the caller — a parcel polygon from a cadastre, say — as a
    /// stand-in for the table a sheet did not print. `origin` names where
    /// it came from.
    External { origin: String },
}

impl ControlTable {
    pub fn from_rows(rows: Vec<ControlRow>, source: TableSource, bbox: Option<BBox>) -> Self {
        let projected: Vec<Projected> = rows.iter().map(|r| r.projected).collect();
        let crs = ProjectedCrs::detect(&projected);
        Self {
            printed_distances: vec![None; rows.len()],
            rows,
            crs,
            source,
            bbox,
        }
    }

    /// Attaches the side lengths printed beside the coordinates, padding or
    /// truncating to one entry per row.
    pub fn with_printed_distances(mut self, mut distances: Vec<Option<f64>>) -> Self {
        distances.resize(self.rows.len(), None);
        self.printed_distances = distances;
        self
    }

    pub fn is_usable(&self) -> bool {
        self.rows.len() >= 3 && self.crs != ProjectedCrs::Unknown
    }

    /// Whether the table came from the caller rather than from a sheet.
    pub fn is_external(&self) -> bool {
        matches!(self.source, TableSource::External { .. })
    }

    /// A control table from a WGS 84 ring — an external statement of the
    /// parcel (the cadastre's polygon) projected into the sheets' own
    /// frame, so it can be matched to a drawn outline exactly like a lent
    /// sibling table. A closing repeat of the first vertex is dropped.
    /// `None` for fewer than three distinct vertices.
    pub fn from_wgs84_ring(ring: &[LonLat], origin: &str) -> Option<Self> {
        let mut ring = ring.to_vec();
        drop_closing_vertex(&mut ring, |a, b| {
            (a.lon - b.lon).abs() < 1e-9 && (a.lat - b.lat).abs() < 1e-9
        });
        if ring.len() < 3 {
            return None;
        }
        let mean_lon = ring.iter().map(|p| p.lon).sum::<f64>() / ring.len() as f64;
        let zone = ProjectedCrs::zone_for_longitude(mean_lon);
        let rows: Vec<ControlRow> = ring
            .iter()
            .enumerate()
            .map(|(i, p)| ControlRow {
                label: (i + 1).to_string(),
                projected: ProjectedCrs::gauss_kruger_forward(zone, *p),
            })
            .collect();
        Some(Self::from_rows(
            rows,
            TableSource::External {
                origin: origin.to_string(),
            },
            None,
        ))
    }
}

/// Survey tables conventionally repeat the first vertex as the last; the
/// ring's own vertices are what a polygon is made of.
fn drop_closing_vertex<T>(ring: &mut Vec<T>, same: impl Fn(&T, &T) -> bool) {
    while ring.len() > 3 && same(&ring[0], &ring[ring.len() - 1]) {
        ring.pop();
    }
}

/// Matches `123456.78`, `1234567,8901` — a coordinate cell, with either
/// decimal separator and no thousands grouping.
fn coordinate_regex() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\d{6,8}[.,]\d{1,5}$").expect("static regex"))
}

/// Matches a side length printed in metres: a short decimal. Requiring the
/// decimal separator is what keeps point numbers and parcel codes out.
fn distance_regex() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\d{1,3}[.,]\d{1,2}$").expect("static regex"))
}

/// Matches a bare point number used as a drawing marker.
fn marker_regex() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\d{1,3}$").expect("static regex"))
}

/// Recovers a control-point table from the page's text layer, returning every
/// plausible column assignment.
///
/// Numeric cells are grouped into rows by projecting their origins onto the
/// axis perpendicular to the text baseline, which keeps rotated tables (common
/// on landscape CAD sheets) working without a special case.
///
/// Which column holds the northing and which the easting cannot be decided
/// from the numbers alone — both orderings can yield a syntactically valid
/// Gauss-Krüger zone. Rather than guess, both are returned, best-first; the
/// georeferencing fit then discards the one that fails to agree with the
/// drawing. Callers wanting a single answer can use
/// [`table_from_text_layer`].
pub fn table_candidates_from_text_layer(page: &PageContent) -> Vec<ControlTable> {
    let cells: Vec<(&str, Point, f64, f64)> = page
        .texts
        .iter()
        .flat_map(|t| t.tokens())
        .filter(|t| coordinate_regex().is_match(t.text))
        .map(|t| (t.text, t.origin, t.angle, t.size))
        .collect();
    if cells.len() < 6 {
        return Vec::new();
    }

    let angle = median(&cells.iter().map(|c| c.2).collect::<Vec<_>>());
    let (cos, sin) = (angle.cos(), angle.sin());
    let along = |p: Point| p.x * cos + p.y * sin;
    let across = |p: Point| -p.x * sin + p.y * cos;

    let row_tolerance = median(&cells.iter().map(|c| c.3).collect::<Vec<_>>()).max(1.0) * 0.7;
    // Successive lines of text advance in −v, so descending `across` is the
    // sheet's own reading order and keeps row numbering aligned with the table.
    let mut sorted: Vec<usize> = (0..cells.len()).collect();
    sorted.sort_by(|&a, &b| {
        across(cells[b].1)
            .partial_cmp(&across(cells[a].1))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut groups: Vec<Vec<usize>> = Vec::new();
    for idx in sorted {
        match groups.last_mut() {
            Some(g)
                if (across(cells[idx].1) - across(cells[*g.last().expect("non-empty")].1))
                    .abs()
                    <= row_tolerance =>
            {
                g.push(idx)
            }
            _ => groups.push(vec![idx]),
        }
    }

    let mut first_column = Vec::new();
    let mut second_column = Vec::new();
    let mut labels: Vec<Option<String>> = Vec::new();
    // Where each row sits across the table, and how far along its last
    // coordinate cell reaches — together these locate the distance column.
    let mut row_across: Vec<f64> = Vec::new();
    let mut last_cell_along = f64::NEG_INFINITY;
    let mut bbox = BBox::EMPTY;
    for group in &groups {
        if group.len() < 2 {
            continue;
        }
        let mut ordered = group.clone();
        ordered.sort_by(|&a, &b| {
            along(cells[a].1)
                .partial_cmp(&along(cells[b].1))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let (Some(a), Some(b)) = (
            parse_decimal(cells[ordered[0]].0),
            parse_decimal(cells[ordered[1]].0),
        ) else {
            continue;
        };
        first_column.push(a);
        second_column.push(b);
        labels.push(row_label(
            page,
            &across,
            &along,
            across(cells[ordered[0]].1),
            along(cells[ordered[0]].1),
            row_tolerance,
        ));
        row_across.push(across(cells[ordered[0]].1));
        last_cell_along = last_cell_along.max(along(cells[ordered[1]].1));
        for &i in &ordered {
            bbox.extend(cells[i].1);
        }
    }
    if first_column.len() < 3 {
        return Vec::new();
    }

    // Cells are the text origins, so pad the box outwards before it is used to
    // exclude in-table text. The pad also has to swallow the row-number column,
    // which sits outside the coordinate cells but is still table furniture.
    let pad = row_tolerance * 7.0;
    let bbox = BBox {
        min_x: bbox.min_x - pad,
        min_y: bbox.min_y - pad,
        max_x: bbox.max_x + pad,
        max_y: bbox.max_y + pad,
    };

    let distances = printed_distances(
        page,
        &across,
        &along,
        &row_across,
        last_cell_along,
        row_tolerance,
    );

    let build = |northings: &[f64], eastings: &[f64]| -> Option<ControlTable> {
        let rows: Vec<ControlRow> = northings
            .iter()
            .zip(eastings.iter())
            .enumerate()
            .map(|(i, (&northing, &easting))| ControlRow {
                label: labels[i].clone().unwrap_or_else(|| (i + 1).to_string()),
                projected: Projected { northing, easting },
            })
            .collect();
        let table = ControlTable::from_rows(rows, TableSource::TextLayer, Some(bbox))
            .with_printed_distances(distances.clone());
        table.is_usable().then_some(table)
    };

    let mut candidates: Vec<ControlTable> = [
        build(&first_column, &second_column),
        build(&second_column, &first_column),
    ]
    .into_iter()
    .flatten()
    .collect();
    candidates.sort_by(|a, b| {
        plausibility(b)
            .partial_cmp(&plausibility(a))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    candidates
}

/// Convenience wrapper returning only the most plausible column assignment.
pub fn table_from_text_layer(page: &PageContent) -> Option<ControlTable> {
    table_candidates_from_text_layer(page).into_iter().next()
}

/// Reads the side-length column many survey tables print to the right of the
/// coordinates.
///
/// Each entry belongs to the segment leaving a row, and is printed *between*
/// that row's baseline and the next one — so a value is assigned to row `i`
/// when it falls in the gap below it. Only cells beyond the last coordinate
/// column are considered, which keeps the coordinates themselves out.
fn printed_distances(
    page: &PageContent,
    across: &impl Fn(Point) -> f64,
    along: &impl Fn(Point) -> f64,
    row_across: &[f64],
    last_cell_along: f64,
    row_tolerance: f64,
) -> Vec<Option<f64>> {
    let mut out = vec![None; row_across.len()];
    if row_across.len() < 2 {
        return out;
    }
    for token in page.texts.iter().flat_map(|t| t.tokens()) {
        if !distance_regex().is_match(token.text) {
            continue;
        }
        if along(token.origin) <= last_cell_along + row_tolerance {
            continue;
        }
        let position = across(token.origin);
        // Reading order runs from high `across` to low, so the gap below row i
        // lies between row i and row i+1.
        let Some(index) = (0..row_across.len() - 1).find(|&i| {
            position <= row_across[i] + row_tolerance
                && position >= row_across[i + 1] - row_tolerance
        }) else {
            continue;
        };
        if out[index].is_none() {
            out[index] = parse_decimal(token.text);
        }
    }
    out
}

/// Weak prior used only to order otherwise-equivalent column assignments:
/// Gauss-Krüger cadastral work clusters in the mid-latitudes, so a table that
/// projects to 40°N is ranked above the same numbers read the other way round
/// projecting to 76°N.
///
/// A prior, not a decision. Where a drawing exists the fit settles the ordering
/// on evidence; this only decides which to try first, and is all there is to go
/// on for a scanned page with no line work to fit.
pub fn plausibility(table: &ControlTable) -> f64 {
    let Some(first) = table.rows.first() else {
        return f64::NEG_INFINITY;
    };
    let Some(ll) = table.crs.to_wgs84(first.projected) else {
        return f64::NEG_INFINITY;
    };
    if !(25.0..=78.0).contains(&ll.lat) {
        return f64::NEG_INFINITY;
    }
    -(ll.lat - 45.0).abs()
}

/// Finds the point-number cell printed to the left of a table row, which is
/// what the drawing's markers refer to. Falls back to `None` when the table has
/// no number column.
fn row_label(
    page: &PageContent,
    across: &impl Fn(Point) -> f64,
    along: &impl Fn(Point) -> f64,
    row_across: f64,
    first_cell_along: f64,
    tolerance: f64,
) -> Option<String> {
    page.texts
        .iter()
        .flat_map(|t| t.tokens())
        .filter(|t| marker_regex().is_match(t.text))
        .filter(|t| (across(t.origin) - row_across).abs() <= tolerance)
        .filter(|t| along(t.origin) < first_cell_along)
        .max_by(|a, b| {
            along(a.origin)
                .partial_cmp(&along(b.origin))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|t| t.text.to_string())
}

fn parse_decimal(s: &str) -> Option<f64> {
    s.replace(',', ".").parse().ok()
}

fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v[v.len() / 2]
}

/// A spatial hash over every vertex on the page, so a marker can be snapped to
/// the drawing corner it annotates without scanning all geometry.
struct VertexIndex {
    cell: f64,
    buckets: HashMap<(i64, i64), Vec<Point>>,
}

impl VertexIndex {
    fn build(page: &PageContent, cell: f64) -> Self {
        let mut buckets: HashMap<(i64, i64), Vec<Point>> = HashMap::new();
        for path in &page.paths {
            for sub in &path.subpaths {
                for p in &sub.points {
                    buckets
                        .entry(((p.x / cell) as i64, (p.y / cell) as i64))
                        .or_default()
                        .push(*p);
                }
            }
        }
        Self { cell, buckets }
    }

    /// The `limit` nearest vertices within `radius` of `p`, closest first.
    fn nearest(&self, p: Point, radius: f64, limit: usize) -> Vec<Point> {
        let reach = (radius / self.cell).ceil() as i64;
        let (cx, cy) = ((p.x / self.cell) as i64, (p.y / self.cell) as i64);
        let mut found: Vec<(f64, Point)> = Vec::new();
        for dx in -reach..=reach {
            for dy in -reach..=reach {
                let Some(bucket) = self.buckets.get(&(cx + dx, cy + dy)) else {
                    continue;
                };
                for q in bucket {
                    let d = p.distance(q);
                    if d <= radius {
                        found.push((d, *q));
                    }
                }
            }
        }
        found.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        // Vertices are shared between adjacent paths, so collapse duplicates.
        let mut out: Vec<Point> = Vec::new();
        for (_, q) in found {
            if out.iter().any(|e| e.distance(&q) < 0.05) {
                continue;
            }
            out.push(q);
            if out.len() >= limit {
                break;
            }
        }
        out
    }
}

/// How far a numeric marker may sit from the vertex it labels, in PDF units.
const MARKER_SNAP_RADIUS: f64 = 14.0;
/// Number of nearby vertices offered per marker; RANSAC picks the consistent one.
const MARKER_SNAP_CANDIDATES: usize = 4;

/// Builds candidate page↔world correspondences by matching numeric markers in
/// the drawing to control-table rows.
///
/// Each marker contributes its own origin plus its nearest vertices as
/// alternatives, because the printed number is offset from the corner it
/// annotates by a leader line of unknown length.
pub fn candidate_correspondences(page: &PageContent, table: &ControlTable) -> Vec<Correspondence> {
    let index = VertexIndex::build(page, MARKER_SNAP_RADIUS);
    let by_label: HashMap<&str, usize> = table
        .rows
        .iter()
        .enumerate()
        .map(|(i, r)| (r.label.as_str(), i))
        .collect();

    let mut out = Vec::new();
    for token in page.texts.iter().flat_map(|t| t.tokens()) {
        if !marker_regex().is_match(token.text) {
            continue;
        }
        let Some(&row) = by_label.get(token.text) else {
            continue;
        };
        // Markers printed inside the table itself are not drawing positions.
        if table.bbox.is_some_and(|b| b.contains(token.origin)) {
            continue;
        }
        let world = Point::new(
            table.rows[row].projected.easting,
            table.rows[row].projected.northing,
        );
        out.push(Correspondence {
            page: token.origin,
            world,
            row,
            source: CandidateSource::TextMarker,
        });
        for vertex in index.nearest(token.origin, MARKER_SNAP_RADIUS, MARKER_SNAP_CANDIDATES) {
            out.push(Correspondence {
                page: vertex,
                world,
                row,
                source: CandidateSource::TextMarker,
            });
        }
    }
    out
}

/// Grows a table's text-derived bounding box to cover its drawn frame.
///
/// The box recovered from the coordinate cells spans only the numbers; the
/// rule lines that box them in reach further, and a survey sheet's table sits
/// inside the drawing frame, so those rules would otherwise be projected onto
/// the map as a grid of streets. Ruled lines are absorbed by repeatedly
/// unioning in any path that substantially overlaps the box without dwarfing
/// it.
pub fn table_exclusion_bbox(page: &PageContent, table: &ControlTable) -> Option<BBox> {
    /// A path must lie almost entirely inside the region to count as part of
    /// the table. Anything less would let a boundary line that merely crosses
    /// the table's corner drag the region out over the drawing.
    const CONTAINMENT: f64 = 0.9;
    /// Hard ceiling on how far the region may grow, relative to the box the
    /// coordinate cells define. A table's rules and frame are a small margin
    /// around its text; nothing legitimate is several times larger.
    const MAX_GROWTH: f64 = 3.0;
    /// Two passes are enough for a frame to pull in its rules.
    const PASSES: usize = 3;

    let seed = table.bbox?;
    let ceiling = seed.area() * MAX_GROWTH;
    let mut region = seed;
    for _ in 0..PASSES {
        let before = region;
        for path in &page.paths {
            let b = path.bbox();
            if b.is_empty() || b.overlap_ratio(&before) < CONTAINMENT {
                continue;
            }
            let mut candidate = region;
            candidate.union(&b);
            if candidate.area() <= ceiling {
                region = candidate;
            }
        }
        if region == before {
            break;
        }
    }
    Some(region)
}

/// The control points as a closed world-space ring, with the repeated closing
/// vertex that survey tables conventionally include removed.
pub fn world_polygon(table: &ControlTable) -> Vec<Point> {
    let mut pts: Vec<Point> = table
        .rows
        .iter()
        .map(|r| Point::new(r.projected.easting, r.projected.northing))
        .collect();
    drop_closing_vertex(&mut pts, |a, b| a.distance(b) < 1e-6);
    pts
}

/// Inlier distance for vertex matching, in metres. Drawings are plotted from
/// the same survey coordinates the table lists, so agreement is a matter of
/// centimetres; half a metre is generous while still discriminating.
const SHAPE_MATCH_TOLERANCE_M: f64 = 0.5;
/// A drawn outline routinely carries vertices the table omits — shared corners
/// with neighbouring parcels, or a boundary that steps around a building — so
/// the search allows a generous surplus rather than requiring equal counts.
const MAX_EXTRA_VERTICES: usize = 14;
/// Upper bound on seed hypotheses evaluated per page, so that a drawing made of
/// hundreds of thousands of strokes cannot make the search unbounded.
const SEED_BUDGET: usize = 20_000_000;
/// Fewest drawn vertices that can identify a parcel. A table often covers
/// several adjacent parcels, so a ring matching only part of it is expected.
const MIN_MATCHED_VERTICES: usize = 4;
/// Candidate outlines must span at least this fraction of the page diagonal,
/// which excludes hatch stipple and symbol glyphs from the search.
const MIN_EXTENT_RATIO: f64 = 0.02;
/// Guards the hypothesis search on pages made of hundreds of thousands of
/// short strokes, where no candidate could plausibly be a parcel outline.
const MAX_RINGS_CONSIDERED: usize = 20_000;

/// Locates the drawn parcel outline by geometry alone, without needing the
/// vertex numbers printed on the drawing to be readable text.
///
/// The turning-point table *is* the subject parcel's boundary, so the polyline
/// in the drawing that maps onto it under a similarity transform identifies the
/// correspondence directly. This matters because CAD exports routinely convert
/// the vertex numbers into outlines, leaving nothing to match on.
///
/// Each candidate polyline is tested against every hypothesis that pairs one of
/// its vertices — and its successor — with one control row and *its* successor.
/// Two pairs determine a similarity, so each hypothesis can be scored by how
/// many further vertices it explains. Working from adjacent pairs rather than
/// requiring equal vertex counts tolerates the drawing carrying an extra
/// vertex, or collapsing two control points that lie a decimetre apart.
pub fn shape_correspondences(page: &PageContent, table: &ControlTable) -> Vec<Correspondence> {
    let world = world_polygon(table);
    let n = world.len();
    if n < 3 {
        return Vec::new();
    }
    let perimeter = ring_perimeter(&world);
    if perimeter <= 0.0 {
        return Vec::new();
    }
    let tolerance = SHAPE_MATCH_TOLERANCE_M.min(perimeter * 0.05);
    let high = n + MAX_EXTRA_VERTICES;
    let min_extent = (page.media_box.width().hypot(page.media_box.height())) * MIN_EXTENT_RATIO;

    // Outlines are searched both as the file draws them and as reassembled
    // polylines, because cadastral exports routinely emit a boundary as loose
    // two-point segments that no single subpath contains.
    let drawn: Vec<Chain> = page
        .paths
        .iter()
        .flat_map(|p| p.subpaths.iter())
        .map(|s| Chain {
            points: s.points.clone(),
            closed: s.closed,
        })
        .collect();
    let assembled = assemble_chains(&page.paths, ChainOptions::default());

    let mut considered = 0usize;
    let mut budget = SEED_BUDGET;
    let mut best: Option<(usize, f64, Vec<Correspondence>)> = None;
    for chain in drawn.iter().chain(assembled.iter()) {
        {
            if !(MIN_MATCHED_VERTICES..=high).contains(&chain.points.len()) {
                continue;
            }
            let mut bbox = pdf_geometry::model::BBox::EMPTY;
            for p in &chain.points {
                bbox.extend(*p);
            }
            if bbox.width().hypot(bbox.height()) < min_extent {
                continue;
            }
            let ring = dedupe_ring(&chain.points);
            if !(MIN_MATCHED_VERTICES..=high).contains(&ring.len()) {
                continue;
            }
            considered += 1;
            let cost = 2 * ring.len() * n;
            if considered > MAX_RINGS_CONSIDERED || cost > budget {
                return best.map(|(_, _, pairs)| pairs).unwrap_or_default();
            }
            budget -= cost;
            let Some(found) = match_ring(&ring, &world, tolerance) else {
                continue;
            };
            let better = match &best {
                None => true,
                Some((count, rmse, _)) => (found.0, -found.1) > (*count, -*rmse),
            };
            if better {
                best = Some(found);
            }
        }
    }
    best.map(|(_, _, pairs)| pairs).unwrap_or_default()
}

/// Scores every two-point hypothesis pairing consecutive drawn vertices with
/// consecutive control rows, returning the best consensus as
/// `(inlier count, RMSE, correspondences)`.
fn match_ring(
    ring: &[Point],
    world: &[Point],
    tolerance: f64,
) -> Option<(usize, f64, Vec<Correspondence>)> {
    let (m, n) = (ring.len(), world.len());
    if m < 3 || n < 3 {
        return None;
    }
    let mut best: Option<(usize, f64, Vec<Correspondence>)> = None;

    for reversed in [false, true] {
        let step = |a: usize| {
            if reversed {
                (a + m - 1) % m
            } else {
                (a + 1) % m
            }
        };
        for a in 0..m {
            for i in 0..n {
                let seed = [
                    Correspondence {
                        page: ring[a],
                        world: world[i],
                        row: i,
                        source: CandidateSource::ShapeMatch,
                    },
                    Correspondence {
                        page: ring[step(a)],
                        world: world[(i + 1) % n],
                        row: (i + 1) % n,
                        source: CandidateSource::ShapeMatch,
                    },
                ];
                let Some(model) = super::fit::fit_similarity(&seed) else {
                    continue;
                };
                let denominator = model.drawing_scale_denominator();
                if !(20.0..=25000.0).contains(&denominator) {
                    continue;
                }

                // Each drawn vertex claims the nearest control row, and each row
                // is claimed at most once, so a degenerate transform collapsing
                // the ring onto one point cannot score.
                let mut pairs: Vec<Correspondence> = Vec::new();
                let mut used = vec![false; n];
                let mut sum_sq = 0.0;
                for (v, vertex) in ring.iter().enumerate() {
                    let projected = model.apply(*vertex);
                    let candidate = (0..n)
                        .filter(|r| !used[*r])
                        .map(|r| (r, projected.distance(&world[r])))
                        .min_by(|x, y| x.1.partial_cmp(&y.1).unwrap_or(std::cmp::Ordering::Equal));
                    let Some((row, distance)) = candidate else {
                        break;
                    };
                    if distance > tolerance {
                        continue;
                    }
                    used[row] = true;
                    sum_sq += distance * distance;
                    pairs.push(Correspondence {
                        page: ring[v],
                        world: world[row],
                        row,
                        source: CandidateSource::ShapeMatch,
                    });
                }
                if pairs.len() < MIN_MATCHED_VERTICES {
                    continue;
                }
                let rmse = (sum_sq / pairs.len() as f64).sqrt();
                let better = match &best {
                    None => true,
                    Some((count, best_rmse, _)) => (pairs.len(), -rmse) > (*count, -*best_rmse),
                };
                if better {
                    best = Some((pairs.len(), rmse, pairs));
                }
            }
        }
    }
    best
}

/// Drops consecutive duplicate vertices and any repeated closing vertex.
fn dedupe_ring(points: &[Point]) -> Vec<Point> {
    let mut out: Vec<Point> = Vec::with_capacity(points.len());
    for p in points {
        if out.last().is_none_or(|q: &Point| q.distance(p) > 1e-9) {
            out.push(*p);
        }
    }
    while out.len() > 3 && out[0].distance(&out[out.len() - 1]) < 1e-9 {
        out.pop();
    }
    out
}

fn ring_perimeter(points: &[Point]) -> f64 {
    if points.len() < 2 {
        return 0.0;
    }
    points.windows(2).map(|w| w[0].distance(&w[1])).sum::<f64>()
        + points[points.len() - 1].distance(&points[0])
}

/// Extracts a declared drawing scale such as `Մ 1:500` from the page text, used
/// as an independent check on the fitted transform.
pub fn declared_scale_denominators(page: &PageContent) -> Vec<f64> {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(
        || // Sheets write the scale as `Մ 1:500`, `M 1: 500` or `Մ : 1-500`.
        Regex::new(r"1\s*[:：\-\u{2013}]\s*(\d{2,6})").expect("static regex"),
    );
    let text = page.concatenated_text();
    re.captures_iter(&text)
        .filter_map(|c| c.get(1)?.as_str().parse::<f64>().ok())
        .filter(|d| (20.0..=25000.0).contains(d))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdf_geometry::model::{Rgb, TextItem};

    fn page_with_texts(texts: Vec<TextItem>) -> PageContent {
        PageContent {
            page_number: 1,
            media_box: BBox {
                min_x: 0.0,
                min_y: 0.0,
                max_x: 842.0,
                max_y: 595.0,
            },
            paths: Vec::new(),
            texts,
            image_count: 0,
            image_coverage: 0.0,
        }
    }

    fn text(s: &str, x: f64, y: f64) -> TextItem {
        TextItem {
            text: s.to_string(),
            origin: Point::new(x, y),
            size: 6.0,
            angle: 0.0,
            color: Rgb::BLACK,
        }
    }

    #[test]
    fn reads_a_two_column_table() {
        let mut items = Vec::new();
        for (i, (n, e)) in [
            (4419484.1041, 8471288.7976),
            (4419494.8211, 8471308.8616),
            (4419492.3898, 8471310.0397),
            (4419491.9276, 8471309.0626),
        ]
        .iter()
        .enumerate()
        {
            let y = 500.0 - i as f64 * 12.0;
            items.push(text(&format!("{n:.4}"), 600.0, y));
            items.push(text(&format!("{e:.4}"), 680.0, y));
        }
        let table = table_from_text_layer(&page_with_texts(items)).expect("table");
        assert_eq!(table.rows.len(), 4);
        assert_eq!(table.crs, ProjectedCrs::PulkovoGaussKruger { zone: 8 });
        assert!((table.rows[0].projected.northing - 4419484.1041).abs() < 1e-6);
        assert!((table.rows[0].projected.easting - 8471288.7976).abs() < 1e-6);
    }

    #[test]
    fn swapped_columns_are_detected() {
        let mut items = Vec::new();
        for (i, (n, e)) in [
            (4419484.1041, 8471288.7976),
            (4419494.8211, 8471308.8616),
            (4419492.3898, 8471310.0397),
        ]
        .iter()
        .enumerate()
        {
            let y = 500.0 - i as f64 * 12.0;
            // Easting printed first this time.
            items.push(text(&format!("{e:.4}"), 600.0, y));
            items.push(text(&format!("{n:.4}"), 680.0, y));
        }
        let table = table_from_text_layer(&page_with_texts(items)).expect("table");
        assert!((table.rows[0].projected.northing - 4419484.1041).abs() < 1e-6);
    }

    #[test]
    fn markers_inside_the_table_are_ignored() {
        let table = ControlTable::from_rows(
            vec![ControlRow {
                label: "1".into(),
                projected: Projected {
                    northing: 4419484.1,
                    easting: 8471288.8,
                },
            }],
            TableSource::TextLayer,
            Some(BBox {
                min_x: 590.0,
                min_y: 400.0,
                max_x: 760.0,
                max_y: 520.0,
            }),
        );
        let page = page_with_texts(vec![text("1", 600.0, 500.0), text("1", 120.0, 300.0)]);
        let candidates = candidate_correspondences(&page, &table);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].page, Point::new(120.0, 300.0));
    }

    #[test]
    fn parses_declared_scale() {
        let page = page_with_texts(vec![text("Հատակագիծ Մ 1:2000", 10.0, 10.0)]);
        assert_eq!(declared_scale_denominators(&page), vec![2000.0]);
    }
}
