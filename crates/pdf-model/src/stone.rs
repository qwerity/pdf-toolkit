//! The stone piers' heights, read off the elevations that draw them.
//!
//! A plan gives a pier its footprint and nothing of its height. An elevation
//! draws its face: a filled polygon as wide as the footprint where the sheet's
//! grid axes put it, standing up from below the first upper storey.
//! [`read_tops`] projects every pier onto every elevation sheet the parameters
//! name, gives each face to the pier nearest the viewer that projects onto it
//! (a pier behind the house is hidden there), and reads the face's top against
//! the sheet's level marks. A pier the elevations agree on gets that top; one
//! they disagree on, or that none draws, keeps none, and a warning says so.

use pdf_classify::sheet::SheetRole;
use pdf_geometry::PdfDocument;
use pdf_geometry::model::{BBox, PageContent, PaintMode};

use crate::elevation::{self, Across, Elevation, ModelAxis, PlanAxes};
use crate::model::{Polygon2D, Stone};
use crate::params::Params;
use crate::sheet::Sheet;

/// A face's sides lie within this (page units) of a pier's projection.
const FACE_EDGE: f64 = 2.0;
/// A pier's face is at least this tall (m).
const MIN_FACE_M: f64 = 2.0;
/// One pier's readings within this (m) of each other agree.
const TOPS_AGREE_M: f64 = 0.05;
/// A pier's top is taken only when this many sheets draw it: one sheet alone
/// may be a section filed as an elevation, whose cut walls read like faces.
const MIN_READINGS: usize = 2;

/// Reads the piers' tops from the elevation pages `params` names into
/// `stone`; `ground` is the ground-floor sheet the piers were cut from.
pub fn read_tops(
    document: &PdfDocument,
    params: &Params,
    ground: &Sheet,
    stone: &mut [Stone],
    warnings: &mut Vec<String>,
) {
    if stone.is_empty() || params.elevation_pages.is_empty() {
        return;
    }
    let Ok(plan) = document.page_content(ground.page) else {
        return;
    };
    let axes = PlanAxes::read(&plan, ground.origin, params.mm_per_point() / 1000.0);
    let below = first_upper_soffit(params);
    let mut readings: Vec<Vec<(usize, f64)>> = vec![Vec::new(); stone.len()];
    let mut unreadable: Vec<String> = Vec::new();
    for &page in &params.elevation_pages {
        let sheet = document.page_content(page).ok().and_then(|content| {
            elevation::read(
                &content,
                SheetRole::Elevation,
                params.scale_denominator,
                &axes,
            )
            .map(|sheet| (content, sheet))
        });
        let Some((content, sheet)) = sheet else {
            unreadable.push(page.to_string());
            continue;
        };
        for (index, top) in tops_on(&faces(&content), &sheet, stone, below) {
            readings[index].push((page, top));
        }
    }
    if !unreadable.is_empty() {
        warnings.push(format!(
            "page(s) {}: the elevation's level marks or grid axes do not read; no pier height is taken from them",
            unreadable.join(", ")
        ));
    }
    let mut unread = 0;
    for (index, (pier, readings)) in stone.iter_mut().zip(readings).enumerate() {
        match settle(&readings) {
            Some((top, pages)) => {
                pier.top = Some(top);
                pier.top_pages = pages;
            }
            None if sheets(&readings).len() < MIN_READINGS => unread += 1,
            None => warnings.push(format!(
                "stone piece {}: the elevations draw its top at {}; it stands to the parapet",
                index + 1,
                readings
                    .iter()
                    .map(|(page, top)| format!("{top:.3} (page {page})"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }
    if unread > 0 {
        warnings.push(format!(
            "{unread} of {} stone pieces are drawn on fewer than {MIN_READINGS} elevations; they stand to the parapet",
            stone.len()
        ));
    }
}

/// A face that starts at or above this level (m) belongs to an upper storey,
/// not to a pier: the soffit of the first upper storey's slab, or of the roof.
fn first_upper_soffit(params: &Params) -> f64 {
    params
        .floors
        .get(1)
        .map_or(params.roof_level_m, |floor| floor.level_m)
        - params.slab_thickness_m
}

/// The bounding boxes of the sheet's filled polygons.
fn faces(content: &PageContent) -> Vec<BBox> {
    content
        .paths
        .iter()
        .filter(|path| matches!(path.mode, PaintMode::Fill | PaintMode::FillStroke))
        .flat_map(|path| path.subpaths.iter().map(|subpath| subpath.bbox()))
        .filter(|bbox| !bbox.is_empty())
        .collect()
}

/// The tops one elevation draws, by pier index. A pier's projection is the
/// page span of its footprint along the sheet's axis; a face whose sides both
/// lie within [`FACE_EDGE`] of it, at least [`MIN_FACE_M`] tall and starting
/// below `below`, is the pier's — unless another pier projecting onto it stands
/// nearer the viewer. A pier shown by several faces takes the highest.
fn tops_on(faces: &[BBox], sheet: &Elevation, stone: &[Stone], below: f64) -> Vec<(usize, f64)> {
    let across = &sheet.across;
    let levels = &sheet.levels;
    let spans: Vec<(f64, f64)> = stone
        .iter()
        .map(|pier| {
            let (lo, hi) = extent(&pier.polygon, across.axis);
            let (a, b) = (across.page_x(lo), across.page_x(hi));
            (a.min(b), a.max(b))
        })
        .collect();
    let mut tops: Vec<Option<f64>> = vec![None; stone.len()];
    for face in faces {
        if face.height() < MIN_FACE_M * levels.units_per_m || levels.level_at(face.min_y) >= below {
            continue;
        }
        let owner = spans
            .iter()
            .enumerate()
            .filter(|(_, (a, b))| {
                (face.min_x - a).abs() <= FACE_EDGE && (face.max_x - b).abs() <= FACE_EDGE
            })
            .map(|(index, _)| index)
            .max_by(|&i, &j| {
                nearness(&stone[i].polygon, across).total_cmp(&nearness(&stone[j].polygon, across))
            });
        if let Some(index) = owner {
            let top = levels.level_at(face.max_y);
            if tops[index].is_none_or(|kept| top > kept) {
                tops[index] = Some(top);
            }
        }
    }
    tops.into_iter()
        .enumerate()
        .filter_map(|(index, top)| top.map(|top| (index, top)))
        .collect()
}

/// A footprint's extent along a model axis, metres.
fn extent(polygon: &Polygon2D, axis: ModelAxis) -> (f64, f64) {
    polygon
        .outer
        .iter()
        .map(|p| match axis {
            ModelAxis::X => p[0],
            ModelAxis::Y => p[1],
        })
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| {
            (lo.min(v), hi.max(v))
        })
}

/// How near a pier stands to an elevation's viewer, larger nearer. A sheet
/// whose columns run left to right is seen from before the first row (−y),
/// one whose rows do from beyond the last column (+x); a reversed sheet from
/// the opposite side.
fn nearness(polygon: &Polygon2D, across: &Across) -> f64 {
    let (depth, toward_viewer_positive) = match (across.axis, across.per_m > 0.0) {
        (ModelAxis::X, true) => (ModelAxis::Y, false),
        (ModelAxis::X, false) => (ModelAxis::Y, true),
        (ModelAxis::Y, true) => (ModelAxis::X, true),
        (ModelAxis::Y, false) => (ModelAxis::X, false),
    };
    let (lo, hi) = extent(polygon, depth);
    if toward_viewer_positive { hi } else { -lo }
}

/// The distinct sheets a pier's readings come from.
fn sheets(readings: &[(usize, f64)]) -> Vec<usize> {
    let mut pages: Vec<usize> = readings.iter().map(|(page, _)| *page).collect();
    pages.sort_unstable();
    pages.dedup();
    pages
}

/// A pier's top from its readings: their median to the millimetre, with the
/// pages that drew it, when at least [`MIN_READINGS`] sheets drew it and they
/// agree within [`TOPS_AGREE_M`].
fn settle(readings: &[(usize, f64)]) -> Option<(f64, Vec<usize>)> {
    let pages = sheets(readings);
    if pages.len() < MIN_READINGS {
        return None;
    }
    let mut tops: Vec<f64> = readings.iter().map(|(_, top)| *top).collect();
    tops.sort_by(f64::total_cmp);
    let (first, last) = (*tops.first()?, *tops.last()?);
    if last - first > TOPS_AGREE_M {
        return None;
    }
    let middle = tops.len() / 2;
    let median = if tops.len() % 2 == 1 {
        tops[middle]
    } else {
        (tops[middle - 1] + tops[middle]) / 2.0
    };
    Some(((median * 1000.0).round() / 1000.0, pages))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::elevation::LevelScale;

    /// Page units per metre at 1:75.
    const UNITS_PER_M: f64 = 72.0 / (75.0 * 0.0254);

    fn pier(x0: f64, y0: f64, x1: f64, y1: f64) -> Stone {
        Polygon2D {
            outer: vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]],
            holes: Vec::new(),
        }
        .into()
    }

    /// The reference house's two stone walls, either end of it.
    fn walls() -> Vec<Stone> {
        vec![pier(-3.2, 2.8, -1.2, 3.2), pier(11.2, 2.8, 13.2, 3.2)]
    }

    /// An end elevation: the rows across it, left to right as seen from beyond
    /// the last column, right to left from before the first.
    fn end_view(rightwards: bool) -> Elevation {
        Elevation {
            page: 16,
            role: SheetRole::Elevation,
            levels: LevelScale {
                datum_y: 298.06,
                units_per_m: UNITS_PER_M,
                marks: 6,
            },
            across: Across {
                axis: ModelAxis::Y,
                at_zero: if rightwards { 492.4 } else { 719.2 },
                per_m: if rightwards {
                    UNITS_PER_M
                } else {
                    -UNITS_PER_M
                },
            },
        }
    }

    /// A drawn face spanning `from`–`to` along the sheet's axis and `bottom`–
    /// `top` in level.
    fn face(sheet: &Elevation, from: f64, to: f64, bottom: f64, top: f64) -> BBox {
        let (a, b) = (sheet.across.page_x(from), sheet.across.page_x(to));
        BBox {
            min_x: a.min(b),
            min_y: sheet.levels.y_of(bottom),
            max_x: a.max(b),
            max_y: sheet.levels.y_of(top),
        }
    }

    fn millimetres(tops: Vec<(usize, f64)>) -> Vec<(usize, f64)> {
        tops.into_iter()
            .map(|(index, top)| (index, (top * 1000.0).round() / 1000.0))
            .collect()
    }

    #[test]
    fn the_pier_nearest_the_viewer_takes_the_face_both_project_onto() {
        // Seen from beyond the last column the east wall is in front; from
        // before the first, the west wall.
        for (rightwards, front) in [(true, 1), (false, 0)] {
            let sheet = end_view(rightwards);
            let faces = [
                face(&sheet, 2.8, 3.2, -0.45, 4.8),
                // The upper storey's wall above the pier, as wide: not stone.
                face(&sheet, 2.8, 3.2, 4.85, 6.67),
            ];
            assert_eq!(
                millimetres(tops_on(&faces, &sheet, &walls(), 3.1)),
                vec![(front, 4.8)],
                "seen rightwards: {rightwards}"
            );
        }
    }

    #[test]
    fn a_face_off_the_projection_or_too_short_is_not_a_pier() {
        let sheet = end_view(true);
        let faces = [
            face(&sheet, 2.9, 3.3, -0.45, 4.8),
            face(&sheet, 2.8, 3.2, 0.0, 1.5),
        ];
        assert!(tops_on(&faces, &sheet, &walls(), 3.1).is_empty());
    }

    #[test]
    fn readings_settle_on_their_median_or_on_nothing() {
        assert_eq!(
            settle(&[(19, 4.7998), (16, 4.8003), (18, 4.8)]),
            Some((4.8, vec![16, 18, 19]))
        );
        assert_eq!(settle(&[(16, 4.8), (18, 4.95)]), None, "15 cm apart");
        assert_eq!(settle(&[(40, 4.713)]), None, "one sheet alone");
        assert_eq!(settle(&[]), None);
    }
}
