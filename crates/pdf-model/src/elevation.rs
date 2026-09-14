//! Elevation and section sheets read against the model: at which page height
//! a level lies, and at which page position a grid axis does.
//!
//! An elevation prints its levels down one side — a value beside a small
//! filled triangle whose apex touches the level's line — and names the grid
//! axes along its foot in bubbles. At the sheet's scale both readings are
//! linear: a level lies at the ±0.000 line plus the level times the page units
//! per metre, and an axis at its model coordinate times the same, along the
//! model axis its labels belong to. [`read`] recovers both from the sheet
//! alone, so that a rule can ask how high something drawn on a facade stands.
//! Nothing is guessed: a sheet whose marks do not agree on one ±0.000 line, or
//! whose bubbles do not name two of the plan's axes at its scale, reads as
//! `None`.

use pdf_classify::sheet::{self, SheetRole};
use pdf_geo::local::metres_per_page_unit;
use pdf_geometry::model::{PageContent, PaintMode, Point};

use crate::sheet::axis_bubbles;

/// A level mark's triangle lies within this many font sizes of its text.
const MARK_REACH_SIZES: f64 = 3.0;
/// Two proposed ±0.000 lines within this (page units) are the same line.
const DATUM_AGREE: f64 = 1.0;
/// A sheet's levels read when at least this many marks agree on the line.
const MIN_AGREEING_MARKS: usize = 3;
/// The width and height of a level mark's triangle, page units: the reference
/// set draws them 11 wide on one facade and 17 on another. A triangle that is
/// not a mark proposes a line no other mark agrees with.
const TRIANGLE_SIZE: std::ops::RangeInclusive<f64> = 2.0..=25.0;
/// Two vertices this close (page units) are one; two heights this close level.
const VERTEX_EPSILON: f64 = 0.1;
/// A bubble lies within this (page units) of where the scale puts its axis.
const AXIS_AGREE: f64 = 1.5;

/// The model axis a grid label belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelAxis {
    /// Columns, labelled with digits, run along `x`.
    X,
    /// Rows, labelled with letters, run along `y`.
    Y,
}

/// The plan's grid axes: each label and its coordinate in model metres.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlanAxes {
    pub columns: Vec<(String, f64)>,
    pub rows: Vec<(String, f64)>,
}

impl PlanAxes {
    /// Reads a plan sheet's bubbles into the model's metres about `origin`
    /// (page units): digit labels are columns at their bubble's x, letters are
    /// rows at its y. A label drawn more than once is taken at its bottom-most
    /// bubble (a column) or left-most (a row) — the grid's own edge, where
    /// [`crate::sheet`] finds the origin too.
    pub fn read(content: &PageContent, origin: Point, metres_per_unit: f64) -> Self {
        let mut columns: Vec<(String, Point)> = Vec::new();
        let mut rows: Vec<(String, Point)> = Vec::new();
        for bubble in axis_bubbles(content) {
            let is_column = bubble.label.chars().all(|c| c.is_ascii_digit());
            let list = if is_column { &mut columns } else { &mut rows };
            match list.iter_mut().find(|(label, _)| *label == bubble.label) {
                Some((_, kept)) => {
                    let nearer_the_edge = if is_column {
                        bubble.centre.y < kept.y
                    } else {
                        bubble.centre.x < kept.x
                    };
                    if nearer_the_edge {
                        *kept = bubble.centre;
                    }
                }
                None => list.push((bubble.label, bubble.centre)),
            }
        }
        Self {
            columns: columns
                .into_iter()
                .map(|(label, centre)| (label, (centre.x - origin.x) * metres_per_unit))
                .collect(),
            rows: rows
                .into_iter()
                .map(|(label, centre)| (label, (centre.y - origin.y) * metres_per_unit))
                .collect(),
        }
    }

    /// A label's coordinate along `axis`, when the plan names it.
    pub fn coordinate(&self, axis: ModelAxis, label: &str) -> Option<f64> {
        let list = match axis {
            ModelAxis::X => &self.columns,
            ModelAxis::Y => &self.rows,
        };
        list.iter()
            .find(|(name, _)| name == label)
            .map(|(_, value)| *value)
    }
}

/// A sheet's levels: where ±0.000 lies on the page, and how far a metre is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LevelScale {
    /// Page y of the ±0.000 line.
    pub datum_y: f64,
    /// Page units per metre of height.
    pub units_per_m: f64,
    /// How many level marks agree on that line.
    pub marks: usize,
}

impl LevelScale {
    /// The level, in metres, of a page height.
    pub fn level_at(&self, y: f64) -> f64 {
        (y - self.datum_y) / self.units_per_m
    }

    /// The page height of a level.
    pub fn y_of(&self, level: f64) -> f64 {
        self.datum_y + level * self.units_per_m
    }
}

/// Where a sheet's horizontal runs in the model:
/// `page x = at_zero + per_m · coordinate` along `axis`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Across {
    pub axis: ModelAxis,
    /// Page x of the axis' zero.
    pub at_zero: f64,
    /// Page units per metre, negative when the axis runs right to left.
    pub per_m: f64,
}

impl Across {
    /// The page x of a model coordinate along the axis.
    pub fn page_x(&self, coordinate: f64) -> f64 {
        self.at_zero + self.per_m * coordinate
    }

    /// The model coordinate along the axis of a page x.
    pub fn coordinate(&self, x: f64) -> f64 {
        (x - self.at_zero) / self.per_m
    }
}

/// One elevation or section sheet, read against the model.
#[derive(Debug, Clone, PartialEq)]
pub struct Elevation {
    pub page: usize,
    pub role: SheetRole,
    pub levels: LevelScale,
    pub across: Across,
}

/// Reads an elevation or section sheet at its scale against the plan's axes:
/// `None` unless both its levels and its axes read.
pub fn read(
    content: &PageContent,
    role: SheetRole,
    scale_denominator: f64,
    axes: &PlanAxes,
) -> Option<Elevation> {
    let units_per_m = 1.0 / metres_per_page_unit(scale_denominator);
    Some(Elevation {
        page: content.page_number,
        role,
        levels: level_scale(content, units_per_m)?,
        across: across(content, units_per_m, axes)?,
    })
}

/// The ±0.000 line the sheet's level marks agree on. Each mark is paired with
/// every triangle near its text, and each pairing proposes the line the
/// triangle's apex implies for the mark's value; the line most marks propose
/// wins. A mark printed above a downward triangle and one printed below an
/// upward triangle land on the same line; a stacked label's wrong neighbour
/// does not.
fn level_scale(content: &PageContent, units_per_m: f64) -> Option<LevelScale> {
    let apexes = triangle_apexes(content);
    let marks = sheet::placed_level_marks(content);
    let proposals: Vec<(usize, f64)> = marks
        .iter()
        .enumerate()
        .flat_map(|(index, mark)| {
            let reach = MARK_REACH_SIZES * mark.size.max(1.0);
            apexes
                .iter()
                .filter(move |apex| {
                    (apex.x - mark.origin.x).abs() <= reach
                        && (apex.y - mark.origin.y).abs() <= reach
                })
                .map(move |apex| (index, apex.y - mark.value * units_per_m))
        })
        .collect();
    // The marks agreeing with a proposed line, each by its nearest proposal.
    let agreeing = |datum: f64| {
        let mut kept: Vec<(usize, f64)> = Vec::new();
        for &(index, proposed) in &proposals {
            if (proposed - datum).abs() > DATUM_AGREE {
                continue;
            }
            match kept.iter_mut().find(|(i, _)| *i == index) {
                Some(entry) if (proposed - datum).abs() < (entry.1 - datum).abs() => {
                    entry.1 = proposed;
                }
                Some(_) => {}
                None => kept.push((index, proposed)),
            }
        }
        kept
    };
    let support = proposals
        .iter()
        .map(|&(_, datum)| agreeing(datum))
        .max_by_key(Vec::len)?;
    if support.len() < MIN_AGREEING_MARKS {
        return None;
    }
    let datum_y = support.iter().map(|(_, datum)| datum).sum::<f64>() / support.len() as f64;
    Some(LevelScale {
        datum_y,
        units_per_m,
        marks: support.len(),
    })
}

/// The apex of every small filled triangle on the sheet: the vertex whose
/// height the other two do not share, which is where a level mark's triangle
/// touches its line whether it points up or down.
fn triangle_apexes(content: &PageContent) -> Vec<Point> {
    let mut apexes = Vec::new();
    for path in &content.paths {
        if !matches!(path.mode, PaintMode::Fill | PaintMode::FillStroke) {
            continue;
        }
        for subpath in &path.subpaths {
            let bbox = subpath.bbox();
            if !TRIANGLE_SIZE.contains(&bbox.width()) || !TRIANGLE_SIZE.contains(&bbox.height()) {
                continue;
            }
            let mut vertices: Vec<Point> = Vec::with_capacity(3);
            for point in &subpath.points {
                if !vertices.iter().any(|v| v.distance(point) < VERTEX_EPSILON) {
                    vertices.push(*point);
                }
            }
            let &[a, b, c] = &vertices[..] else {
                continue;
            };
            let level = |p: Point, q: Point| (p.y - q.y).abs() < VERTEX_EPSILON;
            let apex = match (level(b, c), level(a, c), level(a, b)) {
                (true, false, false) => a,
                (false, true, false) => b,
                (false, false, true) => c,
                _ => continue,
            };
            apexes.push(apex);
        }
    }
    apexes
}

/// The model axis the sheet's bubbles name, and where it runs on the page: at
/// least two bubbles naming axes of one kind, each within [`AXIS_AGREE`] of
/// where the sheet's scale puts it, whichever way round the facade shows them.
/// When both kinds read, the kind more bubbles name wins.
fn across(content: &PageContent, units_per_m: f64, axes: &PlanAxes) -> Option<Across> {
    let bubbles = axis_bubbles(content);
    [ModelAxis::X, ModelAxis::Y]
        .into_iter()
        .filter_map(|axis| {
            let known: Vec<(f64, f64)> = bubbles
                .iter()
                .filter_map(|bubble| {
                    axes.coordinate(axis, &bubble.label)
                        .map(|coordinate| (coordinate, bubble.centre.x))
                })
                .collect();
            let first = known.iter().min_by(|p, q| p.0.total_cmp(&q.0))?;
            let last = known.iter().max_by(|p, q| p.0.total_cmp(&q.0))?;
            if last.0 - first.0 < 1e-6 {
                return None;
            }
            let per_m = if last.1 >= first.1 {
                units_per_m
            } else {
                -units_per_m
            };
            let at_zero = known
                .iter()
                .map(|(coordinate, x)| x - per_m * coordinate)
                .sum::<f64>()
                / known.len() as f64;
            known
                .iter()
                .all(|(coordinate, x)| (at_zero + per_m * coordinate - x).abs() <= AXIS_AGREE)
                .then_some((
                    known.len(),
                    Across {
                        axis,
                        at_zero,
                        per_m,
                    },
                ))
        })
        .max_by_key(|(count, _)| *count)
        .map(|(_, across)| across)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdf_geometry::model::{BBox, Rgb, SubPath, TextItem, VectorPath};

    /// Page units per metre at 1:75.
    const UNITS_PER_M: f64 = 72.0 / (75.0 * 0.0254);

    fn path(points: Vec<Point>, mode: PaintMode) -> VectorPath {
        VectorPath {
            subpaths: vec![SubPath {
                points,
                closed: true,
            }],
            mode,
            stroke: Rgb::BLACK,
            fill: Rgb::BLACK,
            width: 0.5,
            layer: None,
        }
    }

    /// A level mark's triangle with its apex on `line`, pointing down (its
    /// text above it) or up (its text below).
    fn triangle(x: f64, line: f64, down: bool) -> VectorPath {
        let base = if down { line + 5.7 } else { line - 5.7 };
        path(
            vec![
                Point::new(x, line),
                Point::new(x - 5.7, base),
                Point::new(x + 5.7, base),
                Point::new(x, line),
            ],
            PaintMode::Fill,
        )
    }

    fn circle(cx: f64, cy: f64) -> VectorPath {
        let points = (0..24)
            .map(|i| {
                let a = f64::from(i) * std::f64::consts::TAU / 24.0;
                Point::new(cx + 11.3 * a.cos(), cy + 11.3 * a.sin())
            })
            .collect();
        path(points, PaintMode::Stroke)
    }

    fn text(t: &str, x: f64, y: f64) -> TextItem {
        TextItem {
            text: t.into(),
            origin: Point::new(x, y),
            size: 7.0,
            angle: 0.0,
            color: Rgb::BLACK,
        }
    }

    /// A bubble: its circle, and the label run placed so its centre is the
    /// circle's (the sheet reader's glyph estimate for a 7 pt label).
    fn bubble(label: &str, cx: f64, cy: f64) -> (VectorPath, TextItem) {
        let chars = label.chars().count() as f64;
        (
            circle(cx, cy),
            text(label, cx - 0.6 * 7.0 * chars / 2.0, cy - 0.35 * 7.0),
        )
    }

    fn page(paths: Vec<VectorPath>, texts: Vec<TextItem>) -> PageContent {
        PageContent {
            page_number: 18,
            media_box: BBox {
                min_x: 0.0,
                min_y: 0.0,
                max_x: 1190.0,
                max_y: 842.0,
            },
            paths,
            texts,
            image_count: 0,
            image_coverage: 0.0,
        }
    }

    #[test]
    fn the_marks_agree_on_one_line_whichever_way_their_triangles_point() {
        let datum = 291.4;
        let at = |level: f64| datum + level * UNITS_PER_M;
        let content = page(
            vec![
                triangle(994.0, at(7.2), true),
                // A stacked neighbour within reach of the +7,200 text.
                triangle(994.0, at(7.2) + 12.0, true),
                triangle(994.0, at(4.85), true),
                triangle(994.0, at(3.6), false),
                triangle(994.0, at(0.0), false),
            ],
            vec![
                text("+7,200", 994.0, at(7.2) + 7.0),
                text("+4,850", 994.0, at(4.85) + 7.0),
                text("+3,600", 994.0, at(3.6) - 12.5),
                text("±0,000", 994.0, at(0.0) - 12.5),
            ],
        );
        let levels = level_scale(&content, UNITS_PER_M).expect("the levels read");
        assert_eq!(levels.marks, 4, "{levels:?}");
        assert!((levels.datum_y - datum).abs() < 1e-6, "{levels:?}");
        assert!((levels.level_at(at(4.85)) - 4.85).abs() < 1e-9);
        assert!((levels.y_of(3.6) - at(3.6)).abs() < 1e-9);
    }

    #[test]
    fn two_marks_are_not_enough_to_read_the_levels() {
        let at = |level: f64| 291.4 + level * UNITS_PER_M;
        let content = page(
            vec![
                triangle(994.0, at(7.2), true),
                triangle(994.0, at(0.0), false),
            ],
            vec![
                text("+7,200", 994.0, at(7.2) + 7.0),
                text("±0,000", 994.0, at(0.0) - 12.5),
            ],
        );
        assert_eq!(level_scale(&content, UNITS_PER_M), None);
    }

    fn axes() -> PlanAxes {
        PlanAxes {
            columns: vec![("1".into(), 0.0), ("2".into(), 4.0), ("3".into(), 11.0)],
            rows: vec![("Ա".into(), 0.0), ("Բ".into(), 5.9)],
        }
    }

    fn facade(labels: &[(&str, f64)]) -> PageContent {
        let (paths, texts) = labels
            .iter()
            .map(|(label, x)| bubble(label, *x, 204.5))
            .unzip();
        page(paths, texts)
    }

    #[test]
    fn the_bubbles_map_the_axis_either_way_round() {
        let rightwards = |coordinate: f64| 422.7 + coordinate * UNITS_PER_M;
        let content = facade(&[
            ("1", rightwards(0.0)),
            ("2", rightwards(4.0)),
            ("3", rightwards(11.0)),
        ]);
        let columns = across(&content, UNITS_PER_M, &axes()).expect("axis 1-3");
        assert_eq!(columns.axis, ModelAxis::X);
        assert!((columns.per_m - UNITS_PER_M).abs() < 1e-9);
        assert!((columns.page_x(11.0) - rightwards(11.0)).abs() < 1e-6);

        let leftwards = |coordinate: f64| 763.1 - coordinate * UNITS_PER_M;
        let content = facade(&[("Բ", leftwards(5.9)), ("Ա", leftwards(0.0))]);
        let rows = across(&content, UNITS_PER_M, &axes()).expect("axis Բ-Ա");
        assert_eq!(rows.axis, ModelAxis::Y);
        assert!(rows.per_m < 0.0);
        assert!((rows.coordinate(leftwards(5.9)) - 5.9).abs() < 1e-6);
    }

    #[test]
    fn bubbles_off_the_scale_do_not_map_an_axis() {
        // Axis 3 drawn a metre from where the scale puts it.
        let x = |coordinate: f64| 422.7 + coordinate * UNITS_PER_M;
        let content = facade(&[("1", x(0.0)), ("3", x(12.0))]);
        assert_eq!(across(&content, UNITS_PER_M, &axes()), None);
    }

    #[test]
    fn the_plan_axes_are_read_in_metres_from_the_grid_edge() {
        let origin = Point::new(399.9, 334.5);
        let x = |coordinate: f64| origin.x + coordinate * UNITS_PER_M;
        let y = |coordinate: f64| origin.y + coordinate * UNITS_PER_M;
        let (paths, texts): (Vec<_>, Vec<_>) = [
            bubble("1", x(0.0), 175.7),
            bubble("2", x(4.0), 175.7),
            bubble("3", x(11.0), 175.7),
            // Axis 1 labelled again at the top, off by a millimetre: the
            // bottom bubble is the one kept.
            bubble("1", x(0.0) + 3.0, 720.0),
            bubble("Ա", 237.3, y(0.0)),
            bubble("Բ", 237.3, y(5.9)),
        ]
        .into_iter()
        .unzip();
        let axes = PlanAxes::read(&page(paths, texts), origin, 1.0 / UNITS_PER_M);
        let round = |v: Option<f64>| v.map(|v| (v * 1000.0).round() / 1000.0);
        assert_eq!(round(axes.coordinate(ModelAxis::X, "1")), Some(0.0));
        assert_eq!(round(axes.coordinate(ModelAxis::X, "3")), Some(11.0));
        assert_eq!(round(axes.coordinate(ModelAxis::Y, "Բ")), Some(5.9));
        assert_eq!(axes.coordinate(ModelAxis::Y, "Գ"), None);
    }
}
