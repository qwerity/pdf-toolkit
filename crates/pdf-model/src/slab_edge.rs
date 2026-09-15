//! A slab's edge band, as the section cuts it.
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

use pdf_geometry::model::{BBox, PageContent, PaintMode, Point, SubPath};

use crate::elevation::{Elevation, LevelScale};

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

/// The face of a band that slopes from its tip to full height.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChamferFace {
    /// The top slopes: the tip stands on the band's underside.
    Top,
    /// The underside slopes: the tip hangs from the band's top.
    Bottom,
}

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

fn millimetres(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::elevation::{Across, ModelAxis};
    use pdf_classify::sheet::SheetRole;
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
}
