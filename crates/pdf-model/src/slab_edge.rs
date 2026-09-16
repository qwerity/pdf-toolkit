//! A slab's edge band: its profile off the section, its runs off the plans.
//!
//! Where a section cuts a slab through, each end of the cut shows the slab's
//! edge in profile. A canopy-like edge is drawn there as a band of its own: a
//! filled polygon standing out past the slab, taller than it, only a tip's
//! height at its outer side and sloping to full height on one face.
//! [`read_profile`] takes the outermost fill at a slab's height on each side of
//! the sheet, reads its outline against the sheet's level marks — how far the
//! band reaches, how tall its tip is, which face slopes and over what run —
//! and keeps the reading only when the two ends agree. Nothing is guessed: a
//! slab cut off square, a wall stub at its height, or two ends drawn
//! differently read as `None`.
//!
//! A section shows the band only where it cuts it. Where else the band runs,
//! a plan says: it fills the band as strips as wide as the section's band,
//! beside the slab's edge. [`read_edges`] runs each upper slab's band — and
//! the roof's — along the edges such strips lie beside, on the storey's own
//! plan or else on the roof plan, and takes the roof's band out of the roof's
//! outline, which the roof plan's strips had joined.

use geo::{Area, BooleanOps, BoundingRect, Buffer, Contains, MultiPolygon, Polygon};
use pdf_classify::sheet::SheetRole;
use pdf_geometry::PdfDocument;
use pdf_geometry::model::{BBox, PageContent, PaintMode, Point, SubPath};

pub use crate::model::ChamferFace;

use crate::elevation::{self, Elevation, LevelScale, PlanAxes};
use crate::geom;
use crate::model::{EdgeRun, Model, Ring, Roof, SlabEdge};
use crate::params::Params;
use crate::sheet::Sheet;

/// A band reaches this far (m) from its tip to its inner face; a deeper fill
/// at the slab's height is the slab itself.
const BAND_DEPTH_M: std::ops::RangeInclusive<f64> = 0.1..=1.5;
/// A band covers at least this share of its slab's thickness...
const MIN_THICKNESS_SHARE: f64 = 0.8;
/// ...and reaches no further than this (m) above or below the slab, which a
/// wall cut through the storey does.
const BAND_REACH_M: f64 = 0.3;
/// Two vertices this close (page units) are one.
const VERTEX_EPSILON: f64 = 0.1;
/// A vertex this close (page units) to a band's outer side lies on its tip.
const TIP_EPSILON: f64 = 0.3;
/// Two levels this close (m) are the same.
const LEVEL_EPSILON_M: f64 = 0.02;
/// The two ends agree when each of their readings is within this (m).
const ENDS_AGREE_M: f64 = 0.03;
/// A plan's strip is as wide as the section's band within this (m)...
const STRIP_WIDTH_M: f64 = 0.05;
/// ...and lies outside the slab, no more than this share of it inside.
const STRIP_INSIDE_SHARE: f64 = 0.1;
/// A slab's edge carries the band when this share of the points along it,
/// moved half a band out, fall in a strip.
const BANDED_SHARE: f64 = 0.8;
/// Points along a slab's edge are tested this far apart (m).
const EDGE_STEP_M: f64 = 0.1;
/// A run shorter than this (m) is a strip drawn at a corner, not a band.
const MIN_RUN_M: f64 = 1.0;
/// A corner's mitre reaches no further than this many band depths.
const MITRE_LIMIT: f64 = 2.0;

/// A slab's edge band as a section cuts it; levels and lengths in metres.
#[derive(Debug, Clone, PartialEq)]
pub struct EdgeProfile {
    /// The section page it was read from.
    pub page: usize,
    /// The band's underside at full height.
    pub bottom: f64,
    /// The band's top at full height.
    pub top: f64,
    /// How far the band reaches, from its tip to its inner face.
    pub depth_m: f64,
    /// How tall it stands at its tip.
    pub tip_m: f64,
    /// How far in from the tip the sloping face reaches full height.
    pub chamfer_m: f64,
    pub face: ChamferFace,
    /// Where the two tips lie along the sheet's axis, model metres, lower first.
    pub tips: [f64; 2],
}

/// One end of a slab's cut, read on its own.
#[derive(Debug, Clone, Copy, PartialEq)]
struct End {
    bottom: f64,
    top: f64,
    depth_m: f64,
    tip_m: f64,
    chamfer_m: f64,
    face: ChamferFace,
    /// Page x of the tip.
    tip_x: f64,
}

/// Reads the edge bands of the storeys above the ground and of the roof into
/// `model`: the profile off the section pages `params` names, the runs off
/// the plans. `sheets` are the storeys' plans, bottom first, and `roof` the
/// roof plan. A slab the sections cut square keeps its edge; one they cut
/// with a band no plan draws keeps it too, with a warning.
pub fn read_edges(
    document: &PdfDocument,
    params: &Params,
    sheets: &[&Sheet],
    roof: Option<&Sheet>,
    model: &mut Model,
    warnings: &mut Vec<String>,
) {
    let Some(ground) = sheets.first() else {
        return;
    };
    if params.section_pages.is_empty() {
        return;
    }
    let Ok(plan) = document.page_content(ground.page) else {
        return;
    };
    let axes = PlanAxes::read(&plan, ground.origin, params.mm_per_point() / 1000.0);
    let mut sections = Vec::new();
    let mut unreadable = Vec::new();
    for &page in &params.section_pages {
        let read = document.page_content(page).ok().and_then(|content| {
            elevation::read(
                &content,
                SheetRole::Section,
                params.scale_denominator,
                &axes,
            )
            .map(|sheet| (content, sheet))
        });
        match read {
            Some(read) => sections.push(read),
            None => unreadable.push(page.to_string()),
        }
    }
    if !unreadable.is_empty() {
        warnings.push(format!(
            "page(s) {}: the section's level marks or grid axes do not read; no slab edge is taken from them",
            unreadable.join(", ")
        ));
    }
    let profile = |slab: [f64; 2]| {
        sections
            .iter()
            .find_map(|(content, sheet)| read_profile(content, sheet, slab))
    };
    let unplaced = |profile: &EdgeProfile, level: f64| {
        format!(
            "page {}: the section cuts the slab at {level:+.2} with an edge band no plan draws; it keeps a square edge",
            profile.page
        )
    };
    for floor in model
        .floors
        .iter_mut()
        .filter(|floor| floor.page != ground.page)
    {
        let Some(profile) = profile(floor.slab_z) else {
            continue;
        };
        let plans: Vec<&Sheet> = sheets
            .iter()
            .copied()
            .filter(|sheet| sheet.page == floor.page)
            .chain(roof)
            .collect();
        match band(&geom::from_polygon2d(&floor.slab), &plans, profile.depth_m) {
            Some((runs, plan_page)) => floor.edge = Some(edge(runs, &profile, plan_page)),
            None => warnings.push(unplaced(&profile, floor.z)),
        }
    }
    let Some((model_roof, sheet)) = model.roof.as_mut().zip(roof) else {
        return;
    };
    let Some(profile) = profile(model_roof.slab_z) else {
        return;
    };
    // The roof plan joins its strips to the roof's outline: its band runs
    // along the envelope and terraces they were joined to.
    let field = sheet
        .envelope()
        .map(|envelope| {
            let parts: Vec<Polygon<f64>> = std::iter::once(envelope.clone())
                .chain(sheet.side_terraces(envelope))
                .collect();
            geom::union_all(&parts)
        })
        .and_then(geom::largest)
        .map(|field| geom::exterior_only(&field));
    match field.and_then(|field| band(&field, &[sheet], profile.depth_m)) {
        Some((runs, plan_page)) => {
            let edge = edge(runs, &profile, plan_page);
            trim(model_roof, &edge);
            model_roof.edge = Some(edge);
        }
        None => warnings.push(unplaced(&profile, model_roof.slab_z[1])),
    }
}

/// The edge band the section draws for the slab spanning the levels `slab`
/// (`[bottom, top]`): read at both ends of the slab's cut, `None` unless both
/// ends draw a sloping band and agree on it.
pub fn read_profile(
    content: &PageContent,
    sheet: &Elevation,
    slab: [f64; 2],
) -> Option<EdgeProfile> {
    let levels = &sheet.levels;
    let bands: Vec<(BBox, &SubPath)> = content
        .paths
        .iter()
        .filter(|path| matches!(path.mode, PaintMode::Fill | PaintMode::FillStroke))
        .flat_map(|path| &path.subpaths)
        .map(|subpath| (subpath.bbox(), subpath))
        .filter(|(bbox, _)| !bbox.is_empty() && at_the_slab(bbox, levels, slab))
        .collect();
    let &(left_box, left) = bands
        .iter()
        .min_by(|a, b| a.0.min_x.total_cmp(&b.0.min_x))?;
    let &(right_box, right) = bands
        .iter()
        .max_by(|a, b| a.0.max_x.total_cmp(&b.0.max_x))?;
    // One band, or two copies of it, is not both ends.
    if right_box.min_x < left_box.max_x {
        return None;
    }
    let left = end(left, &left_box, left_box.min_x, levels)?;
    let right = end(right, &right_box, right_box.max_x, levels)?;
    let close = |a: f64, b: f64| (a - b).abs() <= ENDS_AGREE_M;
    let agree = left.face == right.face
        && close(left.bottom, right.bottom)
        && close(left.top, right.top)
        && close(left.depth_m, right.depth_m)
        && close(left.tip_m, right.tip_m)
        && close(left.chamfer_m, right.chamfer_m);
    if !agree {
        return None;
    }
    let mean = |a: f64, b: f64| millimetres((a + b) / 2.0);
    let (a, b) = (
        sheet.across.coordinate(left.tip_x),
        sheet.across.coordinate(right.tip_x),
    );
    Some(EdgeProfile {
        page: sheet.page,
        bottom: mean(left.bottom, right.bottom),
        top: mean(left.top, right.top),
        depth_m: mean(left.depth_m, right.depth_m),
        tip_m: mean(left.tip_m, right.tip_m),
        chamfer_m: mean(left.chamfer_m, right.chamfer_m),
        face: left.face,
        tips: [millimetres(a.min(b)), millimetres(a.max(b))],
    })
}

/// Whether a fill stands at a slab's height the way its edge band would:
/// covering most of the slab's thickness, reaching little past it, and no
/// deeper than a band.
fn at_the_slab(bbox: &BBox, levels: &LevelScale, slab: [f64; 2]) -> bool {
    let (low, high) = (levels.level_at(bbox.min_y), levels.level_at(bbox.max_y));
    let covered = high.min(slab[1]) - low.max(slab[0]);
    covered >= MIN_THICKNESS_SHARE * (slab[1] - slab[0])
        && low >= slab[0] - BAND_REACH_M
        && high <= slab[1] + BAND_REACH_M
        && BAND_DEPTH_M.contains(&(bbox.width() / levels.units_per_m))
}

/// Reads the band at one end of the cut, whose tip is the side of its outline
/// at page x `outer`. A tip standing on the band's underside has its top
/// sloping, one hanging from its top its underside: from the tip's other
/// corner the outline runs in along the slope to the band's full height.
fn end(subpath: &SubPath, bbox: &BBox, outer: f64, levels: &LevelScale) -> Option<End> {
    let ring = ring(&subpath.points);
    let n = ring.len();
    if n < 3 {
        return None;
    }
    let on_tip = |p: &Point| (p.x - outer).abs() <= TIP_EPSILON;
    let (low, high) = ring
        .iter()
        .filter(|p| on_tip(p))
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(low, high), p| {
            (low.min(p.y), high.max(p.y))
        });
    if high - low <= VERTEX_EPSILON {
        return None;
    }
    let (tip_bottom, tip_top) = (levels.level_at(low), levels.level_at(high));
    let stands = (tip_bottom - levels.level_at(bbox.min_y)).abs() <= LEVEL_EPSILON_M;
    let hangs = (levels.level_at(bbox.max_y) - tip_top).abs() <= LEVEL_EPSILON_M;
    // A tip as tall as the band is a square end; one touching neither face is
    // not a band this reads.
    let (face, corner) = match (stands, hangs) {
        (true, false) => (ChamferFace::Top, high),
        (false, true) => (ChamferFace::Bottom, low),
        _ => return None,
    };
    let index = ring
        .iter()
        .position(|p| on_tip(p) && (p.y - corner).abs() <= VERTEX_EPSILON)?;
    let (before, after) = (ring[(index + n - 1) % n], ring[(index + 1) % n]);
    // One neighbour runs along the tip, the other in along the slope.
    let far = match (on_tip(&before), on_tip(&after)) {
        (true, false) => after,
        (false, true) => before,
        _ => return None,
    };
    let full = levels.level_at(far.y);
    let (bottom, top) = match face {
        ChamferFace::Top if full > tip_top + LEVEL_EPSILON_M => (tip_bottom, full),
        ChamferFace::Bottom if full < tip_bottom - LEVEL_EPSILON_M => (full, tip_top),
        _ => return None,
    };
    let chamfer_m = (far.x - outer).abs() / levels.units_per_m;
    let depth_m = bbox.width() / levels.units_per_m;
    (chamfer_m <= depth_m).then_some(End {
        bottom,
        top,
        depth_m,
        tip_m: tip_top - tip_bottom,
        chamfer_m,
        face,
        tip_x: outer,
    })
}

/// A subpath's vertices as a ring: repeats and the closing vertex dropped.
fn ring(points: &[Point]) -> Vec<Point> {
    let mut ring: Vec<Point> = Vec::with_capacity(points.len());
    for &point in points {
        if ring
            .last()
            .is_none_or(|last| last.distance(&point) > VERTEX_EPSILON)
        {
            ring.push(point);
        }
    }
    while ring.len() > 1 && ring[0].distance(&ring[ring.len() - 1]) <= VERTEX_EPSILON {
        ring.pop();
    }
    ring
}

/// Where a slab's band runs: along the edges of `outline` that strips as wide
/// as a band `depth` deep lie beside, on the first of `plans` that draws any —
/// with the page that did.
fn band(outline: &Polygon<f64>, plans: &[&Sheet], depth: f64) -> Option<(Vec<EdgeRun>, usize)> {
    plans.iter().find_map(|plan| {
        let runs = runs(outline, &strips(plan, outline, depth), depth);
        (!runs.is_empty()).then_some((runs, plan.page))
    })
}

/// A plan's fills as wide as a band `depth` deep, lying outside the slab.
fn strips(plan: &Sheet, outline: &Polygon<f64>, depth: f64) -> MultiPolygon<f64> {
    let wider_than =
        |fill: &Polygon<f64>, width: f64| !geom::parts(fill.buffer(-width / 2.0)).is_empty();
    let strips: Vec<&Polygon<f64>> = plan
        .rooms
        .iter()
        .chain(&plan.envelopes)
        .filter(|fill| {
            fill.bounding_rect()
                .is_some_and(|b| b.width().min(b.height()) >= depth - 2.0 * STRIP_WIDTH_M)
        })
        .filter(|fill| {
            wider_than(fill, depth - 2.0 * STRIP_WIDTH_M)
                && !wider_than(fill, depth + 2.0 * STRIP_WIDTH_M)
        })
        .filter(|fill| {
            geom::intersection_area(fill, outline) <= STRIP_INSIDE_SHARE * fill.unsigned_area()
        })
        .collect();
    geom::union_all(strips)
}

/// The runs of `outline`'s edges that `strips` lie beside. An edge carries the
/// band when most of the points along it, moved half a band out, fall in a
/// strip; consecutive such edges make one run, whose outer line is its inner
/// one moved a band out. A run shorter than [`MIN_RUN_M`] is dropped.
fn runs(outline: &Polygon<f64>, strips: &MultiPolygon<f64>, depth: f64) -> Vec<EdgeRun> {
    let ring = counter_clockwise(outline);
    let n = ring.len();
    if n < 3 || strips.0.is_empty() {
        return Vec::new();
    }
    let banded: Vec<bool> = (0..n)
        .map(|i| {
            let (a, b) = (ring[i], ring[(i + 1) % n]);
            let Some(normal) = outward(a, b) else {
                return false;
            };
            let samples = ((distance(a, b) / EDGE_STEP_M).ceil() as usize).max(1);
            let beside = (0..samples)
                .filter(|&k| {
                    let t = (k as f64 + 0.5) / samples as f64;
                    let x = a.0 + (b.0 - a.0) * t + normal.0 * depth / 2.0;
                    let y = a.1 + (b.1 - a.1) * t + normal.1 * depth / 2.0;
                    strips.contains(&geo::Point::new(x, y))
                })
                .count();
            beside as f64 >= BANDED_SHARE * samples as f64
        })
        .collect();
    let Some(start) = banded.iter().position(|banded| !banded) else {
        // A band all round: one run, closed on itself.
        let mut inner = ring.clone();
        inner.push(ring[0]);
        return vec![run(&inner, depth, true)];
    };
    let mut lines: Vec<Vec<(f64, f64)>> = Vec::new();
    let mut line: Vec<(f64, f64)> = Vec::new();
    for step in 1..=n {
        let i = (start + step) % n;
        if banded[i] {
            if line.is_empty() {
                line.push(ring[i]);
            }
            line.push(ring[(i + 1) % n]);
        } else if !line.is_empty() {
            lines.push(std::mem::take(&mut line));
        }
    }
    lines
        .iter()
        .filter(|line| line.windows(2).map(|w| distance(w[0], w[1])).sum::<f64>() >= MIN_RUN_M)
        .map(|line| run(line, depth, false))
        .collect()
}

/// A run from its inner line: each vertex moved `depth` out along the mitre
/// of the edges either side of it. A closed line's first and last vertex are
/// the same corner.
fn run(inner: &[(f64, f64)], depth: f64, closed: bool) -> EdgeRun {
    let last = inner.len() - 1;
    let outer: Vec<(f64, f64)> = (0..=last)
        .map(|j| {
            let before = match j {
                0 if closed => outward(inner[last - 1], inner[last]),
                0 => None,
                _ => outward(inner[j - 1], inner[j]),
            };
            let after = if j < last {
                outward(inner[j], inner[j + 1])
            } else if closed {
                outward(inner[0], inner[1])
            } else {
                None
            };
            let (dx, dy) = mitre(before, after, depth);
            (inner[j].0 + dx, inner[j].1 + dy)
        })
        .collect();
    let out = |line: &[(f64, f64)]| -> Ring {
        line.iter()
            .map(|&(x, y)| [geom::round(x, 4), geom::round(y, 4)])
            .collect()
    };
    EdgeRun {
        inner: out(inner),
        outer: out(&outer),
    }
}

/// How far, and which way, a vertex moves to lie `depth` out from both edges
/// meeting at it — at a sharp corner no further than [`MITRE_LIMIT`] depths.
fn mitre(before: Option<(f64, f64)>, after: Option<(f64, f64)>, depth: f64) -> (f64, f64) {
    match (before, after) {
        (Some(a), Some(b)) => {
            let (x, y) = (a.0 + b.0, a.1 + b.1);
            let length = x.hypot(y);
            if length < 1e-9 {
                return (a.0 * depth, a.1 * depth);
            }
            let (x, y) = (x / length, y / length);
            let reach = depth / (x * a.0 + y * a.1).max(1.0 / MITRE_LIMIT);
            (x * reach, y * reach)
        }
        (Some(normal), None) | (None, Some(normal)) => (normal.0 * depth, normal.1 * depth),
        (None, None) => (0.0, 0.0),
    }
}

/// The unit normal out of a counter-clockwise ring along its edge `a`–`b`.
fn outward(a: (f64, f64), b: (f64, f64)) -> Option<(f64, f64)> {
    let length = distance(a, b);
    (length > 1e-9).then(|| ((b.1 - a.1) / length, (a.0 - b.0) / length))
}

fn distance(a: (f64, f64), b: (f64, f64)) -> f64 {
    (b.0 - a.0).hypot(b.1 - a.1)
}

/// A polygon's exterior as a counter-clockwise ring, repeats dropped.
fn counter_clockwise(polygon: &Polygon<f64>) -> Vec<(f64, f64)> {
    let mut ring: Vec<(f64, f64)> = Vec::new();
    for c in polygon.exterior().coords() {
        if ring
            .last()
            .is_none_or(|&last| distance(last, (c.x, c.y)) > 1e-6)
        {
            ring.push((c.x, c.y));
        }
    }
    while ring.len() > 1 && distance(ring[0], ring[ring.len() - 1]) <= 1e-6 {
        ring.pop();
    }
    let signed: f64 = (0..ring.len())
        .map(|i| {
            let (a, b) = (ring[i], ring[(i + 1) % ring.len()]);
            a.0 * b.1 - b.0 * a.1
        })
        .sum();
    if signed < 0.0 {
        ring.reverse();
    }
    ring
}

fn edge(runs: Vec<EdgeRun>, profile: &EdgeProfile, plan_page: usize) -> SlabEdge {
    SlabEdge {
        runs,
        z: [profile.bottom, profile.top],
        depth_m: profile.depth_m,
        tip_m: profile.tip_m,
        chamfer_m: profile.chamfer_m,
        face: profile.face,
        section_page: profile.page,
        plan_page,
    }
}

/// The polygon between two facing lines of a run — its whole band between
/// tip and slab, or a strip of it.
pub fn between(a: &Ring, b: &Ring) -> Vec<Polygon<f64>> {
    let ring: Vec<(f64, f64)> = a
        .iter()
        .chain(b.iter().rev())
        .map(|p| (p[0], p[1]))
        .collect();
    geom::polygon_from_ring(&ring)
}

/// Takes a roof's band out of its outline, which the roof plan's strips had
/// joined, and out of its parapet where the two overlap.
fn trim(roof: &mut Roof, edge: &SlabEdge) {
    let band: Vec<Polygon<f64>> = edge
        .runs
        .iter()
        .flat_map(|run| between(&run.outer, &run.inner))
        .collect();
    let band = geom::union_all(&band);
    let Some(kept) = geom::largest(geom::from_polygon2d(&roof.outline).difference(&band))
        .map(|kept| geom::exterior_only(&kept))
    else {
        return;
    };
    roof.parapet = roof
        .parapet
        .iter()
        .filter_map(|parapet| geom::largest(geom::from_polygon2d(parapet).intersection(&kept)))
        .map(|parapet| geom::to_polygon2d(&parapet))
        .collect();
    roof.outline = geom::to_polygon2d(&kept);
}

fn millimetres(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::elevation::{Across, ModelAxis};
    use crate::sheet::OriginSource;
    use pdf_geometry::model::{Rgb, VectorPath};

    /// Page units per metre at 1:75.
    const UNITS_PER_M: f64 = 72.0 / (75.0 * 0.0254);

    /// The reference set's section 1-1: ±0.000 at page y 268.07, axis 1 at
    /// page x 410.77, the columns running rightwards.
    fn section() -> Elevation {
        Elevation {
            page: 20,
            role: SheetRole::Section,
            levels: LevelScale {
                datum_y: 268.07,
                units_per_m: UNITS_PER_M,
                marks: 8,
            },
            across: Across {
                axis: ModelAxis::X,
                at_zero: 410.77,
                per_m: UNITS_PER_M,
            },
        }
    }

    /// A filled polygon through `(coordinate, level)` points on the section.
    fn fill(points: &[(f64, f64)]) -> VectorPath {
        let sheet = section();
        VectorPath {
            subpaths: vec![SubPath {
                points: points
                    .iter()
                    .map(|&(x, level)| Point::new(sheet.across.page_x(x), sheet.levels.y_of(level)))
                    .collect(),
                closed: true,
            }],
            mode: PaintMode::Fill,
            stroke: Rgb::BLACK,
            fill: Rgb {
                r: 0.9,
                g: 0.91,
                b: 0.9,
            },
            width: 0.0,
            layer: None,
        }
    }

    fn page(paths: Vec<VectorPath>) -> PageContent {
        PageContent {
            page_number: 20,
            media_box: BBox {
                min_x: 0.0,
                min_y: 0.0,
                max_x: 1190.0,
                max_y: 842.0,
            },
            paths,
            texts: Vec::new(),
            image_count: 0,
            image_coverage: 0.0,
        }
    }

    /// The +3,600 slab's cut as the reference section draws it: a band either
    /// end 0.40 m deep, its top sloping from a 0.20 m tip over 0.33 m — the
    /// right one over `right_chamfer` — with the slab and a wall below between
    /// them.
    fn floor_cut(right_chamfer: f64) -> Vec<VectorPath> {
        vec![
            fill(&[
                (-2.9, 3.1),
                (-2.5, 3.1),
                (-2.5, 3.7),
                (-2.57, 3.7),
                (-2.9, 3.3),
            ]),
            fill(&[(-2.5, 3.1), (12.5, 3.1), (12.5, 3.6), (-2.5, 3.6)]),
            fill(&[(-1.2, 0.0), (-0.9, 0.0), (-0.9, 3.1), (-1.2, 3.1)]),
            fill(&[
                (12.9, 3.1),
                (12.9, 3.3),
                (12.9 - right_chamfer, 3.7),
                (12.5, 3.7),
                (12.5, 3.1),
            ]),
        ]
    }

    #[test]
    fn a_slab_reads_its_edge_band_from_both_ends_of_its_cut() {
        assert_eq!(
            read_profile(&page(floor_cut(0.33)), &section(), [3.1, 3.6]),
            Some(EdgeProfile {
                page: 20,
                bottom: 3.1,
                top: 3.7,
                depth_m: 0.4,
                tip_m: 0.2,
                chamfer_m: 0.33,
                face: ChamferFace::Top,
                tips: [-2.9, 12.9],
            })
        );
    }

    #[test]
    fn a_roof_band_slopes_underneath_and_its_parapet_is_no_band() {
        let content = page(vec![
            fill(&[
                (-2.9, 7.1),
                (-2.57, 6.7),
                (-2.5, 6.7),
                (-2.5, 7.3),
                (-2.9, 7.3),
            ]),
            fill(&[(-2.5, 7.2), (-2.4, 7.2), (-2.4, 7.75), (-2.5, 7.75)]),
            fill(&[
                (12.9, 7.3),
                (12.5, 7.3),
                (12.5, 6.7),
                (12.57, 6.7),
                (12.9, 7.1),
            ]),
        ]);
        let edge = read_profile(&content, &section(), [6.7, 7.2]).expect("the roof's band");
        assert_eq!(
            (edge.face, edge.bottom, edge.top, edge.tip_m, edge.chamfer_m),
            (ChamferFace::Bottom, 6.7, 7.3, 0.2, 0.33)
        );
    }

    #[test]
    fn ends_that_disagree_are_cut_square_or_are_alone_read_no_band() {
        assert_eq!(
            read_profile(&page(floor_cut(0.2)), &section(), [3.1, 3.6]),
            None,
            "chamfers of 0.33 and 0.20"
        );
        let square = page(vec![
            fill(&[(-2.9, 3.1), (-2.5, 3.1), (-2.5, 3.6), (-2.9, 3.6)]),
            fill(&[(12.5, 3.1), (12.9, 3.1), (12.9, 3.6), (12.5, 3.6)]),
        ]);
        assert_eq!(
            read_profile(&square, &section(), [3.1, 3.6]),
            None,
            "square ends"
        );
        let one_end = page(floor_cut(0.33).into_iter().take(3).collect());
        assert_eq!(
            read_profile(&one_end, &section(), [3.1, 3.6]),
            None,
            "one end"
        );
    }

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Polygon<f64> {
        geom::polygon_from_ring(&[(x0, y0), (x1, y0), (x1, y1), (x0, y1)]).remove(0)
    }

    /// A plan sheet holding only white `rooms` fills, in the model's metres.
    fn plan(page: usize, rooms: Vec<Polygon<f64>>) -> Sheet {
        Sheet {
            page,
            origin: Point::new(0.0, 0.0),
            origin_source: OriginSource::AxisBubbles,
            walls: Vec::new(),
            envelopes: Vec::new(),
            rooms,
        }
    }

    #[test]
    fn a_band_runs_along_the_edges_a_plan_draws_its_strips_beside() {
        let slab = rect(-2.5, -0.2, 12.5, 6.1);
        // Strips down both ends and along the front, none along the back, and
        // a room on the slab.
        let sheet = plan(
            15,
            vec![
                rect(-2.9, -0.6, -2.5, 6.1),
                rect(-2.5, -0.6, 12.5, -0.2),
                rect(12.5, -0.6, 12.9, 6.1),
                rect(0.0, 0.0, 4.0, 4.0),
            ],
        );
        let (runs, page) = band(&slab, &[&sheet], 0.4).expect("a band");
        assert_eq!(page, 15);
        // One run from the back of the west end round to the back of the east
        // one, mitred out at the front corners.
        assert_eq!(
            runs,
            [EdgeRun {
                inner: vec![[-2.5, 6.1], [-2.5, -0.2], [12.5, -0.2], [12.5, 6.1]],
                outer: vec![[-2.9, 6.1], [-2.9, -0.6], [12.9, -0.6], [12.9, 6.1]],
            }]
        );
    }

    #[test]
    fn a_cap_beside_one_corner_or_no_strip_at_all_runs_no_band() {
        let slab = rect(-2.5, -0.2, 12.5, 6.1);
        let cap = plan(14, vec![rect(-2.9, 5.5, -2.5, 6.1)]);
        assert_eq!(band(&slab, &[&cap], 0.4), None, "a cap");
        assert_eq!(band(&slab, &[&plan(14, Vec::new())], 0.4), None, "no strip");
    }

    #[test]
    fn the_roof_gives_up_its_band_and_its_parapet_stays_a_ring_on_it() {
        let parapet = geom::largest(
            rect(-2.53, -0.23, 12.53, 6.14).difference(&rect(-2.43, -0.13, 12.43, 6.04)),
        )
        .expect("a ring");
        // The outline the roof plan's front strip was joined to.
        let mut roof = Roof {
            outline: geom::to_polygon2d(&rect(-2.9, -0.6, 12.9, 6.1)),
            slab_z: [6.7, 7.2],
            parapet: vec![geom::to_polygon2d(&parapet)],
            parapet_z: [7.2, 7.78],
            edge: None,
            empty: false,
        };
        let edge = SlabEdge {
            runs: vec![EdgeRun {
                inner: vec![[-2.5, 6.1], [-2.5, -0.2], [12.5, -0.2], [12.5, 6.1]],
                outer: vec![[-2.9, 6.1], [-2.9, -0.6], [12.9, -0.6], [12.9, 6.1]],
            }],
            z: [6.7, 7.3],
            depth_m: 0.4,
            tip_m: 0.2,
            chamfer_m: 0.33,
            face: ChamferFace::Bottom,
            section_page: 20,
            plan_page: 15,
        };
        trim(&mut roof, &edge);
        assert!(
            (roof.outline.area_m2() - 15.0 * 6.3).abs() < 1e-6,
            "{:?}",
            roof.outline
        );
        assert_eq!(roof.parapet.len(), 1);
        let parapet = geom::from_polygon2d(&roof.parapet[0]);
        assert_eq!(parapet.interiors().len(), 1, "still a ring");
        let off_roof = parapet
            .difference(&geom::from_polygon2d(&roof.outline))
            .unsigned_area();
        assert!(off_roof < 1e-6, "{off_roof} m² off the roof");
    }
}
