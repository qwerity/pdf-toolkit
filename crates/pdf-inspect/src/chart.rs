//! Charts drawn as vector shapes: bars, pie wedges and plotted lines.
//!
//! A chart exported from a spreadsheet or a report tool is paths and a few
//! labels. The text layer carries the labels — axis ticks, category names —
//! but not what the chart says: which bar is 18 and which is 6 exists only in
//! the geometry. Such a chart often has no curves and a handful of paths, so
//! [`LlmReason::VectorDrawing`](crate::page_info::LlmReason::VectorDrawing),
//! which looks for line work, does not see it.
//!
//! The signatures were tuned on a corpus against pages whose vision reading
//! holds a bar, pie or line chart and pages whose reading holds none. Each
//! shape alone is common on a text page, so each rule asks for the chart's
//! own structure:
//!
//! * **bars** — three or more filled boxes standing on one baseline, of
//!   similar thickness and different lengths, in at least three separate
//!   groups (a table's shaded cells touch; a chart's bars leave gaps), with a
//!   numeric axis beside them whose zero or first tick is that baseline;
//! * **pie** — three or more coloured curved shapes whose boxes all hold the
//!   same centre, two of them at a tip (a pie's wedges meet there; a seal's
//!   concentric rings hold it dead centre);
//! * **line** — a stroked polyline that runs one way along x with sloping
//!   segments, on a page with a numeric axis.

use pdfium_render::prelude::{
    PdfMatrix, PdfPage, PdfPageObject, PdfPageObjectCommon, PdfPageObjectsCommon, PdfPageText,
    PdfPathFillMode, PdfPathSegmentType, PdfPathSegments,
};

/// Bars standing on one baseline, at the least.
const MIN_BARS: usize = 3;
/// Bars share a baseline when their base edges are this close (points).
const BASELINE_TOLERANCE: f32 = 1.0;
/// Thinner than this on either side, a box is a rule or a tick, not a bar.
const MIN_BAR_SIDE: f32 = 1.5;
/// Bars of one chart are about as thick as each other.
const MAX_THICKNESS_RATIO: f32 = 1.8;
/// ... and differ in length: the longest at least this many times the shortest.
const MIN_LENGTH_RATIO: f32 = 1.3;
/// Distinct bar lengths, at the least (half-point resolution).
const MIN_DISTINCT_LENGTHS: usize = 3;
/// Separate groups of bars, at the least: shaded table cells all touch.
const MIN_BAR_GROUPS: usize = 3;
/// Bars closer than this side by side belong to one group (points).
const TOUCH_TOLERANCE: f32 = 0.75;
/// A box covering this share of the page on a side is a background.
const MAX_PAGE_SHARE: f32 = 0.9;

/// Numeric labels in a run of evenly stepped values, at the least.
const MIN_AXIS_LABELS: usize = 3;
/// Labels of one axis share an edge or a centre within this (points).
const AXIS_ALIGN_TOLERANCE: f32 = 2.0;
/// Evenly spaced: consecutive gaps differ by at most this (points).
const AXIS_STEP_TOLERANCE: f32 = 1.5;
/// Closer than this, two labels are one wrapped number, not two ticks.
const MIN_AXIS_STEP: f32 = 3.0;
/// An axis beside the bars reaches this share of their extent.
const MIN_AXIS_COVERAGE: f32 = 0.3;

/// Coloured curved shapes smaller than this on a side are glyphs or icons.
const MIN_WEDGE_SIDE: f32 = 15.0;
/// Wedges meeting at one centre, at the least.
const MIN_WEDGES: usize = 3;
/// The centre is held within this share of the pie's size.
const CENTRE_TOLERANCE: f32 = 0.04;
/// Wedges whose tip is the centre, at the least: a wedge of less than half
/// the pie holds the centre off its own middle, while a ring or disc of a
/// seal holds it dead centre.
const MIN_WEDGE_TIPS: usize = 2;
/// Off the middle by more than this share of the wedge's size.
const TIP_OFFSET: f32 = 0.15;

/// Points of a plotted line, at the least, and its sloping segments.
const MIN_LINE_POINTS: usize = 4;
const MIN_LINE_SLOPES: usize = 2;
/// A plotted line spans at least this much of the page (points).
const MIN_LINE_SPAN: f32 = 60.0;
/// A segment moving more than this along both axes slopes.
const SLOPE_MIN_STEP: f32 = 0.5;

#[derive(Clone, Copy, Debug)]
struct Box2 {
    left: f32,
    bottom: f32,
    right: f32,
    top: f32,
}

impl Box2 {
    fn around(points: &[(f32, f32)]) -> Self {
        points.iter().fold(
            Box2 {
                left: f32::MAX,
                bottom: f32::MAX,
                right: f32::MIN,
                top: f32::MIN,
            },
            |b, &(x, y)| Box2 {
                left: b.left.min(x),
                bottom: b.bottom.min(y),
                right: b.right.max(x),
                top: b.top.max(y),
            },
        )
    }

    fn union(self, other: Box2) -> Box2 {
        Box2 {
            left: self.left.min(other.left),
            bottom: self.bottom.min(other.bottom),
            right: self.right.max(other.right),
            top: self.top.max(other.top),
        }
    }

    fn width(self) -> f32 {
        self.right - self.left
    }

    fn height(self) -> f32 {
        self.top - self.bottom
    }

    fn centre(self) -> (f32, f32) {
        (
            (self.left + self.right) / 2.0,
            (self.bottom + self.top) / 2.0,
        )
    }

    /// Whether the point lies inside, or within `tolerance` of an edge.
    fn holds(self, (x, y): (f32, f32), tolerance: f32) -> bool {
        x >= self.left - tolerance
            && x <= self.right + tolerance
            && y >= self.bottom - tolerance
            && y <= self.top + tolerance
    }

    /// The same box with x and y swapped: bars lying on their side stand up.
    fn transposed(self) -> Box2 {
        Box2 {
            left: self.bottom,
            bottom: self.left,
            right: self.top,
            top: self.right,
        }
    }
}

type Rgb = (u8, u8, u8);

/// A page's drawn shapes, in page space.
#[derive(Debug, Default)]
struct Shapes {
    /// Filled axis-aligned boxes that are not white.
    boxes: Vec<Box2>,
    /// Filled curved shapes that are not white: bounds and colour.
    curved: Vec<(Box2, Rgb)>,
    /// Stroked, unfilled, straight-segment polylines.
    polylines: Vec<Vec<(f32, f32)>>,
}

/// A numeric text label: value, right edge, vertical centre.
type Label = (f64, f32, f32);

/// A run of axis labels: its length, whether it runs vertically, the edge or
/// centre it shares, where it starts and ends along its run, and the values
/// there.
#[derive(Debug)]
struct AxisRun {
    labels: usize,
    vertical: bool,
    shared: f32,
    start: f32,
    end: f32,
    start_value: f64,
    end_value: f64,
}

/// Whether the page holds a chart drawn as vector shapes (see the module
/// documentation for what counts).
///
/// `text` is the page's text layer, when the caller has it open. Axis labels
/// are read from it only once the shapes look like a chart, so a page of text
/// and table rules costs one walk of its paths.
pub(crate) fn has_vector_chart(page: &PdfPage<'_>, text: Option<&PdfPageText<'_>>) -> bool {
    let mut shapes = Shapes::default();
    collect(page.objects().iter(), PdfMatrix::IDENTITY, &mut shapes);
    if pie_chart(&shapes.curved) {
        return true;
    }
    let (width, height) = (page.width().value, page.height().value);
    let lying: Vec<Box2> = shapes.boxes.iter().map(|b| b.transposed()).collect();
    let horizontal = bar_groups(lying, height, width);
    let vertical = bar_groups(shapes.boxes, width, height);
    let plotted = shapes.polylines.iter().any(|line| plotted_line(line));
    if vertical.is_empty() && horizontal.is_empty() && !plotted {
        return false;
    }
    let Some(text) = text else {
        return false;
    };
    let runs = axis_runs(&numeric_labels(text));
    (plotted && !runs.is_empty())
        || on_an_axis(&vertical, &runs, true)
        || on_an_axis(&horizontal, &runs, false)
}

/// Walks the page's paths, Form XObjects included, into page-space shapes.
fn collect<'a>(
    objects: impl IntoIterator<Item = PdfPageObject<'a>>,
    ctm: PdfMatrix,
    shapes: &mut Shapes,
) {
    let mut points: Vec<(f32, f32)> = Vec::new();
    let mut starts: Vec<usize> = Vec::new();
    for object in objects {
        if let Some(form) = object.as_x_object_form_object() {
            let inner = form.matrix().unwrap_or(PdfMatrix::IDENTITY);
            collect(form.iter(), inner.multiply(ctm), shapes);
            continue;
        }
        let Some(path) = object.as_path_object() else {
            continue;
        };
        let filled = path
            .fill_mode()
            .is_ok_and(|mode| mode != PdfPathFillMode::None);
        let colour = object
            .fill_color()
            .ok()
            .filter(|c| c.alpha() > 0)
            .map(|c| (c.red(), c.green(), c.blue()))
            .filter(|&rgb| !is_white(rgb));
        let outline = !filled && path.is_stroked().unwrap_or(false);
        let fill = if filled { colour } else { None };
        if fill.is_none() && !outline {
            continue;
        }

        let m = path.matrix().unwrap_or(PdfMatrix::IDENTITY).multiply(ctm);
        points.clear();
        starts.clear();
        let mut curved = false;
        for segment in path.segments().iter() {
            let kind = segment.segment_type();
            if kind == PdfPathSegmentType::MoveTo || starts.is_empty() {
                starts.push(points.len());
            }
            curved |= kind == PdfPathSegmentType::BezierTo;
            let (x, y) = segment.point();
            let (x, y) = m.apply_to_points(x, y);
            points.push((x.value, y.value));
        }
        let ends = starts.iter().skip(1).copied().chain([points.len()]);
        let subpaths = starts.iter().zip(ends).map(|(&from, to)| &points[from..to]);

        match fill {
            Some(rgb) if curved => {
                if points.len() >= 3 {
                    shapes.curved.push((Box2::around(&points), rgb));
                }
            }
            Some(_) => shapes.boxes.extend(subpaths.filter_map(as_box)),
            None if !curved => shapes.polylines.extend(
                subpaths
                    .filter(|s| s.len() >= MIN_LINE_POINTS)
                    .map(<[_]>::to_vec),
            ),
            None => {}
        }
    }
}

fn is_white((r, g, b): Rgb) -> bool {
    r > 245 && g > 245 && b > 245
}

/// A closed axis-aligned quadrilateral (its first corner may be repeated).
fn as_box(points: &[(f32, f32)]) -> Option<Box2> {
    let corners = match points {
        [first, .., last] if points.len() == 5 && near(*first, *last) => &points[..4],
        _ if points.len() == 4 => points,
        _ => return None,
    };
    let axis_aligned = (0..4).all(|i| {
        let (a, b) = (corners[i], corners[(i + 1) % 4]);
        (a.0 - b.0).abs() <= 0.5 || (a.1 - b.1).abs() <= 0.5
    });
    let bounds = Box2::around(corners);
    (axis_aligned && bounds.width() > 0.3 && bounds.height() > 0.3).then_some(bounds)
}

fn near(a: (f32, f32), b: (f32, f32)) -> bool {
    (a.0 - b.0).abs() < 0.5 && (a.1 - b.1).abs() < 0.5
}

/// Numeric labels on the page: plain numbers, percentages, amounts.
fn numeric_labels(text: &PdfPageText<'_>) -> Vec<Label> {
    let mut labels = Vec::new();
    for segment in text.segments().iter() {
        let bounds = segment.bounds();
        let centre = (bounds.top().value + bounds.bottom().value) / 2.0;
        for token in segment.text().split_whitespace() {
            if let Some(value) = parse_number(token) {
                labels.push((value, bounds.right().value, centre));
            }
        }
    }
    labels
}

fn parse_number(token: &str) -> Option<f64> {
    let cleaned = token
        .trim()
        .trim_end_matches('%')
        .trim_start_matches('$')
        .replace(',', "");
    let first = cleaned.chars().next()?;
    if !(first.is_ascii_digit() || first == '-') {
        return None;
    }
    cleaned.parse().ok()
}

/// Runs of aligned labels stepping by one value over one distance: a
/// vertical axis's labels share a right edge, a horizontal axis's a centre.
fn axis_runs(labels: &[Label]) -> Vec<AxisRun> {
    let mut runs = Vec::new();
    for vertical in [true, false] {
        let shared_of = |l: &Label| if vertical { l.1 } else { l.2 };
        let along_of = |l: &Label| if vertical { l.2 } else { l.1 };
        let mut seen = vec![false; labels.len()];
        for i in 0..labels.len() {
            if seen[i] {
                continue;
            }
            let shared = shared_of(&labels[i]);
            let mut line: Vec<(f64, f32)> = Vec::new();
            for (j, label) in labels.iter().enumerate() {
                if (shared_of(label) - shared).abs() <= AXIS_ALIGN_TOLERANCE {
                    seen[j] = true;
                    line.push((label.0, along_of(label)));
                }
            }
            if line.len() < MIN_AXIS_LABELS {
                continue;
            }
            line.sort_by(|a, b| a.1.total_cmp(&b.1));
            // A run that breaks at `end` leaves its last label to start the next.
            let mut start = 0;
            for end in 1..=line.len() {
                let continues =
                    end < line.len() && (end - start < 2 || evenly_stepped(&line[end - 2..=end]));
                if !continues {
                    if end - start >= MIN_AXIS_LABELS {
                        runs.push(AxisRun {
                            labels: end - start,
                            vertical,
                            shared,
                            start: line[start].1,
                            end: line[end - 1].1,
                            start_value: line[start].0,
                            end_value: line[end - 1].0,
                        });
                    }
                    start = end - 1;
                }
            }
        }
    }
    runs
}

/// Three labels one value step and one distance apart.
fn evenly_stepped(three: &[(f64, f32)]) -> bool {
    let (dv0, dv1) = (three[1].0 - three[0].0, three[2].0 - three[1].0);
    let (dp0, dp1) = (three[1].1 - three[0].1, three[2].1 - three[1].1);
    dv1 != 0.0
        && (dv1 - dv0).abs() <= 1e-6 * dv1.abs().max(1.0)
        && (dp1 - dp0).abs() <= AXIS_STEP_TOLERANCE
        && dp1.abs() > MIN_AXIS_STEP
}

/// The bounds of each group of bars standing on one baseline: a common
/// bottom, heights that differ. Bars lying on their side are passed in
/// transposed, with the page's sides swapped.
fn bar_groups(mut bars: Vec<Box2>, page_w: f32, page_h: f32) -> Vec<Box2> {
    bars.retain(|b| {
        b.width() >= MIN_BAR_SIDE
            && b.height() >= MIN_BAR_SIDE
            && b.width() < MAX_PAGE_SHARE * page_w
            && b.height() < MAX_PAGE_SHARE * page_h
    });
    // Groups are seeded in drawing order: a group holds every bar within
    // tolerance of its seed's baseline, so where it reaches depends on the seed.
    let mut by_base: Vec<usize> = (0..bars.len()).collect();
    by_base.sort_by(|&a, &b| bars[a].bottom.total_cmp(&bars[b].bottom));
    let mut grouped = vec![false; bars.len()];
    let mut found = Vec::new();
    for seed in 0..bars.len() {
        if grouped[seed] {
            continue;
        }
        let baseline = bars[seed].bottom;
        let from = by_base.partition_point(|&j| bars[j].bottom < baseline - BASELINE_TOLERANCE);
        let to = by_base.partition_point(|&j| bars[j].bottom <= baseline + BASELINE_TOLERANCE);
        let members = &by_base[from..to];
        members.iter().for_each(|&j| grouped[j] = true);
        if members.len() < MIN_BARS {
            continue;
        }
        let group: Vec<Box2> = members.iter().map(|&j| bars[j]).collect();
        if is_bar_group(&group) {
            found.extend(group.into_iter().reduce(Box2::union));
        }
    }
    found
}

/// Whether any of the bar groups has an axis beside it. `vertical`: the bars
/// stand upright; otherwise they, and the axis beside them, are transposed.
fn on_an_axis(groups: &[Box2], runs: &[AxisRun], vertical: bool) -> bool {
    groups.iter().any(|bounds| {
        runs.iter()
            .any(|run| run.vertical == vertical && axis_beside(run, bounds))
    })
}

fn is_bar_group(group: &[Box2]) -> bool {
    if group.len() < MIN_BARS {
        return false;
    }
    let (shortest, longest) = min_max(group.iter().map(|b| b.height()));
    let (thinnest, thickest) = min_max(group.iter().map(|b| b.width()));
    let mut lengths: Vec<i32> = group.iter().map(|b| (b.height() * 2.0) as i32).collect();
    lengths.sort_unstable();
    lengths.dedup();

    let mut spans: Vec<(f32, f32)> = group.iter().map(|b| (b.left, b.right)).collect();
    spans.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut groups = 1;
    let mut reach = spans[0].1;
    for &(start, end) in &spans[1..] {
        if start > reach + TOUCH_TOLERANCE {
            groups += 1;
        }
        reach = reach.max(end);
    }

    thickest <= MAX_THICKNESS_RATIO * thinnest
        && longest >= MIN_LENGTH_RATIO * shortest
        && lengths.len() >= MIN_DISTINCT_LENGTHS
        && groups >= MIN_BAR_GROUPS
}

fn min_max(values: impl Iterator<Item = f32>) -> (f32, f32) {
    values.fold((f32::MAX, f32::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)))
}

/// An axis runs up beside the bars, covers a good share of their height, and
/// puts its zero — or its first or last tick — at their baseline.
fn axis_beside(run: &AxisRun, bars: &Box2) -> bool {
    let beside = run.shared <= bars.left + 2.0 || run.shared >= bars.right - 2.0;
    let overlaps = run.start <= bars.top + 10.0 && run.end >= bars.bottom - 10.0;
    if !beside || !overlaps || (run.end - run.start) < MIN_AXIS_COVERAGE * bars.height() {
        return false;
    }
    let step = (run.end - run.start) / (run.labels as f32 - 1.0);
    let tolerance = (0.6 * step.abs()).max(3.0);
    let zero = (run.end_value != run.start_value).then(|| {
        run.start
            + ((0.0 - run.start_value) * f64::from(run.end - run.start)
                / (run.end_value - run.start_value)) as f32
    });
    [zero, Some(run.start), Some(run.end)]
        .into_iter()
        .flatten()
        .any(|tick| (bars.bottom - tick).abs() <= tolerance)
}

/// Coloured curved shapes, three or more, whose boxes all hold one centre,
/// at least two of them at their tip.
fn pie_chart(curved: &[(Box2, Rgb)]) -> bool {
    let wedges: Vec<(Box2, Rgb)> = curved
        .iter()
        .copied()
        .filter(|(b, _)| b.width() >= MIN_WEDGE_SIDE && b.height() >= MIN_WEDGE_SIDE)
        .collect();
    let mut colours: Vec<Rgb> = wedges.iter().map(|&(_, rgb)| rgb).collect();
    colours.sort_unstable();
    colours.dedup();
    if colours.len() < MIN_WEDGES {
        return false;
    }
    let pie = wedges
        .iter()
        .map(|&(b, _)| b)
        .reduce(Box2::union)
        .expect("non-empty");
    let centre = pie.centre();
    let tolerance = CENTRE_TOLERANCE * pie.width().max(pie.height());
    let holding: Vec<Box2> = wedges
        .iter()
        .map(|&(b, _)| b)
        .filter(|b| b.holds(centre, tolerance))
        .collect();
    let tips = holding
        .iter()
        .filter(|b| {
            let (mx, my) = b.centre();
            (mx - centre.0).abs() > TIP_OFFSET * b.width()
                || (my - centre.1).abs() > TIP_OFFSET * b.height()
        })
        .count();
    holding.len() >= MIN_WEDGES && tips >= MIN_WEDGE_TIPS
}

/// A stroked line running one way along x, sloping, over a real distance.
fn plotted_line(points: &[(f32, f32)]) -> bool {
    let rising = points.windows(2).all(|w| w[1].0 >= w[0].0 - 0.01);
    let falling = points.windows(2).all(|w| w[1].0 <= w[0].0 + 0.01);
    let slopes = points
        .windows(2)
        .filter(|w| {
            (w[1].0 - w[0].0).abs() > SLOPE_MIN_STEP && (w[1].1 - w[0].1).abs() > SLOPE_MIN_STEP
        })
        .count();
    let (left, right) = min_max(points.iter().map(|p| p.0));
    (rising || falling) && slopes >= MIN_LINE_SLOPES && right - left >= MIN_LINE_SPAN
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(left: f32, bottom: f32, right: f32, top: f32) -> Box2 {
        Box2 {
            left,
            bottom,
            right,
            top,
        }
    }

    fn bar_chart(boxes: &[Box2], runs: &[AxisRun]) -> bool {
        on_an_axis(&bar_groups(boxes.to_vec(), 612.0, 792.0), runs, true)
    }

    #[test]
    fn numbers_parse_with_units_and_separators() {
        assert_eq!(parse_number("1,250"), Some(1250.0));
        assert_eq!(parse_number("40%"), Some(40.0));
        assert_eq!(parse_number("$3.5"), Some(3.5));
        assert_eq!(parse_number("Table"), None);
        assert_eq!(parse_number(""), None);
    }

    #[test]
    fn an_axis_is_a_run_of_evenly_stepped_aligned_labels() {
        // 0, 10, 20, 30 right-aligned at x=90, 50 pt apart, and a stray 7.
        let labels = [
            (0.0, 90.0, 100.0),
            (10.0, 90.0, 150.0),
            (20.0, 90.0, 200.0),
            (30.0, 90.0, 250.0),
            (7.0, 300.0, 400.0),
        ];
        let runs = axis_runs(&labels);
        assert_eq!(runs.len(), 1);
        assert!(runs[0].vertical && runs[0].labels == 4);
        // Uneven values break the run.
        assert!(
            axis_runs(&[(0.0, 90.0, 100.0), (10.0, 90.0, 150.0), (25.0, 90.0, 200.0)]).is_empty()
        );
    }

    #[test]
    fn bars_on_an_axis_are_a_chart_but_touching_cells_are_not() {
        let runs = axis_runs(&[
            (0.0, 90.0, 100.0),
            (10.0, 90.0, 150.0),
            (20.0, 90.0, 200.0),
            (30.0, 90.0, 250.0),
        ]);
        let bars = [
            b(100.0, 100.0, 120.0, 180.0),
            b(140.0, 100.0, 160.0, 240.0),
            b(180.0, 100.0, 200.0, 130.0),
        ];
        assert!(bar_chart(&bars, &runs));
        // The same boxes side by side, touching, are a table's shaded cells.
        let cells = [
            b(100.0, 100.0, 140.0, 180.0),
            b(140.0, 100.0, 180.0, 240.0),
            b(180.0, 100.0, 220.0, 130.0),
        ];
        assert!(!bar_chart(&cells, &runs));
        // Bars floating above the axis's zero are not standing on it.
        let floating = bars.map(|x| b(x.left, x.bottom + 60.0, x.right, x.top + 60.0));
        assert!(!bar_chart(&floating, &runs));
        // Without an axis, bars alone decide nothing.
        assert!(!bar_chart(&bars, &[]));
        // The same bars lying on their side, beside a horizontal axis: passed
        // in transposed, they stand up again and the axis is taken as theirs.
        let across = axis_runs(&[
            (0.0, 100.0, 90.0),
            (10.0, 150.0, 90.0),
            (20.0, 200.0, 90.0),
            (30.0, 250.0, 90.0),
        ]);
        let lying = bars.map(Box2::transposed);
        let stood = lying.map(Box2::transposed).to_vec();
        assert!(on_an_axis(&bar_groups(stood, 792.0, 612.0), &across, false));
        // A horizontal axis is never an upright chart's.
        assert!(!bar_chart(&bars, &across));
    }

    #[test]
    fn equal_boxes_are_not_bars() {
        let runs = axis_runs(&[(0.0, 90.0, 100.0), (10.0, 90.0, 150.0), (20.0, 90.0, 200.0)]);
        let equal = [
            b(100.0, 100.0, 120.0, 180.0),
            b(140.0, 100.0, 160.0, 180.0),
            b(180.0, 100.0, 200.0, 180.0),
        ];
        assert!(!bar_chart(&equal, &runs));
    }

    #[test]
    fn wedges_meeting_at_a_centre_are_a_pie() {
        let (red, green, blue) = ((200, 30, 30), (30, 160, 60), (40, 60, 200));
        let wedges = [
            (b(100.0, 100.0, 200.0, 200.0), red),
            (b(150.0, 100.0, 200.0, 150.0), green),
            (b(100.0, 150.0, 150.0, 200.0), blue),
        ];
        assert!(pie_chart(&wedges));
        // Three rounded boxes in a row share no centre.
        let boxes = [
            (b(100.0, 100.0, 140.0, 140.0), red),
            (b(160.0, 100.0, 200.0, 140.0), green),
            (b(220.0, 100.0, 260.0, 140.0), blue),
        ];
        assert!(!pie_chart(&boxes));
        // A seal's concentric rings hold the centre dead centre: no tips.
        let rings = [
            (b(100.0, 100.0, 200.0, 200.0), red),
            (b(110.0, 110.0, 190.0, 190.0), green),
            (b(120.0, 120.0, 180.0, 180.0), blue),
        ];
        assert!(!pie_chart(&rings));
        // One colour is a shape, not a pie.
        assert!(!pie_chart(&wedges.map(|(bounds, _)| (bounds, red))));
    }

    #[test]
    fn a_plotted_line_runs_one_way_and_slopes() {
        assert!(plotted_line(&[
            (100.0, 100.0),
            (150.0, 160.0),
            (200.0, 130.0),
            (250.0, 190.0)
        ]));
        // A zigzag back and forth is not a series over x.
        assert!(!plotted_line(&[
            (100.0, 100.0),
            (150.0, 160.0),
            (120.0, 130.0),
            (250.0, 190.0)
        ]));
        // A ruled frame has no slopes.
        assert!(!plotted_line(&[
            (100.0, 100.0),
            (300.0, 100.0),
            (300.0, 200.0),
            (300.0, 300.0)
        ]));
    }
}
