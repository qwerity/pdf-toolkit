//! Tables and text boxes read from the page's drawing, and their markdown.
//!
//! A ruled table is the grid its rules draw: every place two rules cross is a
//! cell corner, and a cell edge with no rule under it is a merged cell. Each
//! character is placed in the cell its centre falls in before any words are
//! formed, so a value can never drift into the neighbouring column the way it
//! does when columns are inferred from where text lines up.
//!
//! A frame that draws no grid (one column, or one row) is a text box: a side
//! panel or a callout. Its text is rendered as its own block, which keeps two
//! boxes printed side by side from being read across as one line.
//!
//! Filled chips laid out in rows, with no rules at all, are a table too: the
//! colour-coded zoning keys of a comprehensive plan are drawn that way, and
//! there the colour is the content ("green: typically appropriate"), so each
//! cell carries its colour's name whenever a table uses more than one.

use super::graphics::{Fill, PageGraphics, Rect, Rule, colour_name};
use super::{
    RawChar, TextLine, TextSpan, escape_markdown_into, group_chars_into_spans,
    group_spans_into_lines_owned, is_function_word, line_to_text, needs_space_between,
    spans_to_markdown,
};

/// Rules closer than this are one line: a border drawn by each of two
/// adjacent cells, or a stroke and its fill.
const SNAP: f32 = 2.0;

/// Grid lines closer than this are one: double borders are not a column.
const GRID_SNAP: f32 = 3.0;

/// Share of a cell edge a rule must cover to count as that edge.
const EDGE_COVERAGE: f32 = 0.6;

/// Chips narrower or lower than this are bullets, not cells.
const CHIP_MIN_WIDTH: f32 = 8.0;
const CHIP_MIN_HEIGHT: f32 = 5.0;

/// A fill lighter than this in every channel is page background.
const NEAR_WHITE: u8 = 240;

pub(super) struct Region {
    pub rect: Rect,
    pub kind: Kind,
}

pub(super) enum Kind {
    Table(Grid),
    TextBox,
}

pub(super) struct Grid {
    pub rows: usize,
    pub cols: usize,
    pub cells: Vec<Cell>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Cell {
    pub row: usize,
    pub col: usize,
    pub row_span: usize,
    pub col_span: usize,
    pub rect: Rect,
}

/// Every table and text box the page draws around some of its text.
pub(super) fn detect(graphics: &PageGraphics, chars: &[RawChar], page_area: f32) -> Vec<Region> {
    let centres: Vec<(f32, f32)> = chars.iter().map(centre).collect();
    let holds_text = |rect: &Rect| centres.iter().any(|&(x, y)| rect.contains(x, y, 0.5));

    let mut regions: Vec<Region> = ruled_regions(&graphics.rules, chars)
        .into_iter()
        .filter(|region| holds_text(&region.rect))
        .collect();

    // A region inside a table is part of a cell's drawing; a box around a
    // table is a frame. Either way only one of the two is the structure.
    let nested: Vec<bool> = regions
        .iter()
        .enumerate()
        .map(|(i, inner)| {
            regions.iter().enumerate().any(|(j, outer)| {
                i != j
                    && outer.rect.contains_rect(&inner.rect, SNAP)
                    && outer.rect.area() > inner.rect.area()
                    && matches!(outer.kind, Kind::Table(_))
            })
        })
        .collect();
    let mut kept: Vec<Region> = Vec::new();
    for (region, is_nested) in regions.drain(..).zip(nested) {
        if !is_nested {
            kept.push(region);
        }
    }
    let boxes_around_other: Vec<bool> = kept
        .iter()
        .enumerate()
        .map(|(i, outer)| {
            matches!(outer.kind, Kind::TextBox)
                && kept.iter().enumerate().any(|(j, inner)| {
                    i != j
                        && outer.rect.contains_rect(&inner.rect, SNAP)
                        && outer.rect.area() > inner.rect.area()
                })
        })
        .collect();
    let mut regions: Vec<Region> = kept
        .into_iter()
        .zip(boxes_around_other)
        .filter_map(|(region, frames_other)| (!frames_other).then_some(region))
        .collect();

    let free_fills: Vec<&Fill> = graphics
        .fills
        .iter()
        .filter(|fill| {
            fill.rect.width() >= CHIP_MIN_WIDTH
                && fill.rect.height() >= CHIP_MIN_HEIGHT
                && fill.rect.area() < page_area * 0.2
                && !is_near_white(fill.rgb)
                && holds_text(&fill.rect)
                && !regions
                    .iter()
                    .any(|r| r.rect.contains_rect(&fill.rect, SNAP))
        })
        .collect();
    let chips = chip_tables(&free_fills);
    let boxes = filled_boxes(&free_fills, &chips, chars);
    regions.extend(chips);
    regions.extend(boxes);
    regions
}

/// Fills that are no chip of a table but hold a paragraph (two lines or
/// more): coloured panels printed side by side, each read as its own block
/// rather than line by line across all of them.
fn filled_boxes(fills: &[&Fill], chips: &[Region], chars: &[RawChar]) -> Vec<Region> {
    let mut boxes: Vec<Region> = Vec::new();
    for fill in fills {
        let rect = fill.rect;
        if chips.iter().any(|t| t.rect.contains_rect(&rect, SNAP))
            || boxes.iter().any(|b| same_rect(&b.rect, &rect))
        {
            continue;
        }
        let inside = chars_in(&rect, chars);
        if group_spans_into_lines_owned(group_chars_into_spans(&inside)).len() >= 2 {
            boxes.push(Region {
                rect,
                kind: Kind::TextBox,
            });
        }
    }
    boxes
}

fn centre(c: &RawChar) -> (f32, f32) {
    (c.x + c.width / 2.0, c.y - c.height / 2.0)
}

/// The characters whose centre falls in `rect`.
fn chars_in(rect: &Rect, chars: &[RawChar]) -> Vec<RawChar> {
    chars
        .iter()
        .filter(|c| {
            let (x, y) = centre(c);
            rect.contains(x, y, 0.5)
        })
        .cloned()
        .collect()
}

/// The lowest start and highest end of a set of rules.
fn extent<'a>(rules: impl IntoIterator<Item = &'a Rule>) -> (f32, f32) {
    rules.into_iter().fold((f32::MAX, f32::MIN), |(lo, hi), r| {
        (lo.min(r.start), hi.max(r.end))
    })
}

fn is_near_white((r, g, b): (u8, u8, u8)) -> bool {
    r >= NEAR_WHITE && g >= NEAR_WHITE && b >= NEAR_WHITE
}

// ── Ruled tables ───────────────────────────────────────────────────

fn ruled_regions(rules: &[Rule], chars: &[RawChar]) -> Vec<Region> {
    let merged = merge_rules(rules);
    components(&merged)
        .into_iter()
        .flat_map(|component| {
            let grids = grid_regions(&component);
            if grids.is_empty() {
                row_ruled_table(&component, chars).into_iter().collect()
            } else {
                grids
            }
        })
        .collect()
}

/// Joins collinear rules that touch or overlap into one.
fn merge_rules(rules: &[Rule]) -> Vec<Rule> {
    let mut out: Vec<Rule> = Vec::new();
    for horizontal in [true, false] {
        let mut group: Vec<Rule> = rules
            .iter()
            .filter(|r| r.horizontal == horizontal)
            .copied()
            .collect();
        group.sort_by(|a, b| a.pos.total_cmp(&b.pos).then(a.start.total_cmp(&b.start)));
        let mut merged: Vec<Rule> = Vec::new();
        for rule in group {
            match merged.iter_mut().rev().find(|m| {
                (m.pos - rule.pos).abs() <= SNAP
                    && rule.start <= m.end + SNAP
                    && rule.end >= m.start - SNAP
            }) {
                Some(m) => {
                    m.start = m.start.min(rule.start);
                    m.end = m.end.max(rule.end);
                }
                None => merged.push(rule),
            }
        }
        out.extend(merged);
    }
    out
}

fn crosses(h: &Rule, v: &Rule) -> bool {
    v.pos >= h.start - SNAP
        && v.pos <= h.end + SNAP
        && h.pos >= v.start - SNAP
        && h.pos <= v.end + SNAP
}

/// Tallest band two equal horizontal rules can border as one table row: a
/// group label like "Agricultural", or a note of up to three lines ("Loading
/// Space: ..."), printed across the full width with no vertical rule crossing
/// it to connect the rows above to the rows below.
const MAX_BAND_HEIGHT: f32 = 48.0;

/// Shortest rule that can join two parts of a table this way; a table's own
/// row rules are this long, an underline or a tick is not.
const MIN_BAND_RULE: f32 = 100.0;

/// Two horizontal rules of the same extent, one band apart: the top and
/// bottom of a full-width row inside one table.
fn borders_one_band(a: &Rule, b: &Rule) -> bool {
    a.end - a.start >= MIN_BAND_RULE
        && (a.start - b.start).abs() <= GRID_SNAP
        && (a.end - b.end).abs() <= GRID_SNAP
        && (a.pos - b.pos).abs() <= MAX_BAND_HEIGHT
}

/// Groups of rules connected by crossings, or by bordering one full-width
/// band: each is one table or one frame.
fn components(rules: &[Rule]) -> Vec<Vec<Rule>> {
    let mut parent: Vec<usize> = (0..rules.len()).collect();
    for i in 0..rules.len() {
        for j in (i + 1)..rules.len() {
            let (a, b) = (&rules[i], &rules[j]);
            let touching = match (a.horizontal, b.horizontal) {
                (true, false) => crosses(a, b),
                (false, true) => crosses(b, a),
                (true, true) => borders_one_band(a, b),
                (false, false) => false,
            };
            if touching {
                union(&mut parent, i, j);
            }
        }
    }
    let mut groups: Vec<(usize, Vec<Rule>)> = Vec::new();
    for (i, rule) in rules.iter().enumerate() {
        let root = find(&mut parent, i);
        match groups.iter_mut().find(|(r, _)| *r == root) {
            Some((_, g)) => g.push(*rule),
            None => groups.push((root, vec![*rule])),
        }
    }
    groups.into_iter().map(|(_, g)| g).collect()
}

fn find(parent: &mut [usize], mut i: usize) -> usize {
    while parent[i] != i {
        parent[i] = parent[parent[i]];
        i = parent[i];
    }
    i
}

fn union(parent: &mut [usize], a: usize, b: usize) {
    let (ra, rb) = (find(parent, a), find(parent, b));
    if ra != rb {
        parent[rb] = ra;
    }
}

/// Distinct positions, each the mean of the values within `snap` of it.
fn cluster(mut values: Vec<f32>, snap: f32) -> Vec<f32> {
    values.sort_by(f32::total_cmp);
    let mut groups: Vec<Vec<f32>> = Vec::new();
    for v in values {
        match groups.last_mut() {
            Some(g) if v - g[g.len() - 1] <= snap => g.push(v),
            _ => groups.push(vec![v]),
        }
    }
    groups
        .into_iter()
        .map(|g| g.iter().sum::<f32>() / g.len() as f32)
        .collect()
}

fn covered(rules: &[&Rule], pos: f32, from: f32, to: f32) -> bool {
    let span = (to - from).abs();
    if span <= 0.0 {
        return true;
    }
    let (lo, hi) = (from.min(to), from.max(to));
    let overlap: f32 = rules
        .iter()
        .filter(|r| (r.pos - pos).abs() <= GRID_SNAP)
        .map(|r| (r.end.min(hi) - r.start.max(lo)).max(0.0))
        .sum();
    overlap / span >= EDGE_COVERAGE
}

/// Column edges a row must gain and lose at once to start a new table: one
/// edge either way is a merged cell, two or more is a different column layout
/// (a header block a printer repeated with other widths, touching the table).
const LAYOUT_CHANGE_EDGES: usize = 2;

/// The tables (or text box) a connected set of rules draws. Rows are split
/// into separate tables where the column layout changes.
fn grid_regions(component: &[Rule]) -> Vec<Region> {
    let hs: Vec<&Rule> = component.iter().filter(|r| r.horizontal).collect();
    let vs: Vec<&Rule> = component.iter().filter(|r| !r.horizontal).collect();
    if hs.len() < 2 || vs.len() < 2 {
        return Vec::new();
    }
    let mut xs = cluster(vs.iter().map(|r| r.pos).collect(), GRID_SNAP);
    let mut ys = cluster(hs.iter().map(|r| r.pos).collect(), GRID_SNAP);
    ys.reverse();
    // A table may draw no outer side borders (Word's default, as in the LA
    // Generalized Summary of Zoning Regulations): its rows' rules still run to
    // the table's edges, and those edges bound the outer columns. The same
    // holds for an open top or bottom.
    let (rules_left, rules_right) = extent(hs.iter().copied());
    let (rules_bottom, rules_top) = extent(vs.iter().copied());
    if xs.first().is_some_and(|&x| rules_left < x - GRID_SNAP) {
        xs.insert(0, rules_left);
    }
    if xs.last().is_some_and(|&x| rules_right > x + GRID_SNAP) {
        xs.push(rules_right);
    }
    if ys.first().is_some_and(|&y| rules_top > y + GRID_SNAP) {
        ys.insert(0, rules_top);
    }
    if ys.last().is_some_and(|&y| rules_bottom < y - GRID_SNAP) {
        ys.push(rules_bottom);
    }
    if xs.len() < 2 || ys.len() < 2 {
        return Vec::new();
    }

    // The inner column edges each row actually draws.
    let cols = xs.len() - 1;
    let edges: Vec<Vec<usize>> = (0..ys.len() - 1)
        .map(|r| {
            (1..cols)
                .filter(|&c| covered(&vs, xs[c], ys[r + 1], ys[r]))
                .collect()
        })
        .collect();

    let mut segments: Vec<(usize, usize)> = Vec::new(); // first row, last row
    let mut system: Vec<usize> = Vec::new();
    let mut first = 0;
    for (r, row) in edges.iter().enumerate() {
        let gained = row.iter().filter(|c| !system.contains(c)).count();
        let lost = system.iter().filter(|c| !row.contains(c)).count();
        if !row.is_empty() && gained >= LAYOUT_CHANGE_EDGES && lost >= LAYOUT_CHANGE_EDGES {
            // Full-width rows just above the change are its title: they go with it.
            let mut start = r;
            while start > first && edges[start - 1].is_empty() {
                start -= 1;
            }
            if start > first {
                segments.push((first, start - 1));
            }
            first = start;
            system = row.clone();
        } else {
            for c in row {
                if !system.contains(c) {
                    system.push(*c);
                }
            }
        }
    }
    segments.push((first, edges.len() - 1));

    segments
        .into_iter()
        .filter_map(|(r0, r1)| {
            let used: Vec<usize> = (r0..=r1).flat_map(|r| edges[r].iter().copied()).collect();
            let seg_xs: Vec<f32> = (0..=cols)
                .filter(|&c| c == 0 || c == cols || used.contains(&c))
                .map(|c| xs[c])
                .collect();
            build_grid(&seg_xs, &ys[r0..=r1 + 1], &hs, &vs)
        })
        .collect()
}

/// The cells between grid lines `xs` (left to right) and `ys` (top to
/// bottom), merged wherever an edge has no rule; a text box when the grid has
/// fewer than two rows or no row divided into columns.
fn build_grid(xs: &[f32], ys: &[f32], hs: &[&Rule], vs: &[&Rule]) -> Option<Region> {
    let rect = Rect {
        left: xs[0],
        bottom: ys[ys.len() - 1],
        right: xs[xs.len() - 1],
        top: ys[0],
    };
    if rect.width() < 10.0 || rect.height() < 6.0 {
        return None;
    }

    let (rows, cols) = (ys.len() - 1, xs.len() - 1);
    let id = |r: usize, c: usize| r * cols + c;
    let mut parent: Vec<usize> = (0..rows * cols).collect();
    for (r, band) in ys.windows(2).enumerate() {
        for (c, &x) in xs.iter().enumerate().take(cols).skip(1) {
            if !covered(vs, x, band[1], band[0]) {
                union(&mut parent, id(r, c - 1), id(r, c));
            }
        }
    }
    for (r, &y) in ys.iter().enumerate().take(rows).skip(1) {
        for (c, span) in xs.windows(2).enumerate() {
            if !covered(hs, y, span[0], span[1]) {
                union(&mut parent, id(r - 1, c), id(r, c));
            }
        }
    }

    let mut groups: Vec<(usize, Vec<(usize, usize)>)> = Vec::new(); // root, member positions
    for r in 0..rows {
        for c in 0..cols {
            let root = find(&mut parent, id(r, c));
            match groups.iter_mut().find(|g| g.0 == root) {
                Some(g) => g.1.push((r, c)),
                None => groups.push((root, vec![(r, c)])),
            }
        }
    }
    let cells: Vec<Cell> = groups
        .iter()
        .flat_map(|(_, members)| rectangles(members))
        .map(|(r0, c0, r1, c1)| Cell {
            row: r0,
            col: c0,
            row_span: r1 - r0 + 1,
            col_span: c1 - c0 + 1,
            rect: Rect {
                left: xs[c0],
                bottom: ys[r1 + 1],
                right: xs[c1 + 1],
                top: ys[r0],
            },
        })
        .collect();

    // A frame with no inner columns is a text box, however many rows its
    // header bar or dividers give it.
    let is_table = rows >= 2 && cols >= 2 && cells.iter().any(|c| c.col_span < cols);
    Some(Region {
        rect,
        kind: if is_table {
            Kind::Table(Grid { rows, cols, cells })
        } else {
            Kind::TextBox
        },
    })
}

/// A merged region as rectangles `(r0, c0, r1, c1)`. Most are one rectangle;
/// where a drawing leaves out edges, merges can join into an L (a header cell
/// with no rule beneath it running into a merged data cell). Its bounding box
/// would overlap other cells and take their text, so it is cut into column
/// runs per row, and runs over the same columns in consecutive rows are
/// stacked back together.
fn rectangles(members: &[(usize, usize)]) -> Vec<(usize, usize, usize, usize)> {
    let (r0, r1) = members
        .iter()
        .fold((usize::MAX, 0), |(lo, hi), &(r, _)| (lo.min(r), hi.max(r)));
    let (c0, c1) = members
        .iter()
        .fold((usize::MAX, 0), |(lo, hi), &(_, c)| (lo.min(c), hi.max(c)));
    if members.len() == (r1 - r0 + 1) * (c1 - c0 + 1) {
        return vec![(r0, c0, r1, c1)];
    }
    let mut runs: Vec<(usize, usize, usize)> = Vec::new(); // row, first col, last col
    for r in r0..=r1 {
        let mut cols: Vec<usize> = members.iter().filter(|m| m.0 == r).map(|m| m.1).collect();
        cols.sort_unstable();
        for c in cols {
            match runs.last_mut() {
                Some(run) if run.0 == r && run.2 + 1 == c => run.2 = c,
                _ => runs.push((r, c, c)),
            }
        }
    }
    let mut out: Vec<(usize, usize, usize, usize)> = Vec::new();
    for (r, a, b) in runs {
        match out
            .iter_mut()
            .find(|o| o.1 == a && o.3 == b && o.2 + 1 == r)
        {
            Some(o) => o.2 = r,
            None => out.push((r, a, r, b)),
        }
    }
    out
}

// ── Chip tables ────────────────────────────────────────────────────

/// Rows of filled chips, two rows or more, read as a table whose cells are the
/// chips. Columns are the chips' left edges across all rows.
fn chip_tables(fills: &[&Fill]) -> Vec<Region> {
    let mut chips: Vec<Rect> = Vec::new();
    for fill in fills {
        if !chips.iter().any(|c| same_rect(c, &fill.rect)) {
            chips.push(fill.rect);
        }
    }
    chips.sort_by(|a, b| b.top.total_cmp(&a.top).then(a.left.total_cmp(&b.left)));

    let mut rows: Vec<Vec<Rect>> = Vec::new();
    for chip in chips {
        let mid = (chip.top + chip.bottom) / 2.0;
        match rows.iter_mut().find(|row| {
            let r = row[0];
            let ratio = chip.height() / r.height();
            (mid - (r.top + r.bottom) / 2.0).abs() < r.height() * 0.5
                && (0.67..1.5).contains(&ratio)
        }) {
            Some(row) => row.push(chip),
            None => rows.push(vec![chip]),
        }
    }
    rows.retain(|row| row.len() >= 2);
    for row in &mut rows {
        row.sort_by(|a, b| a.left.total_cmp(&b.left));
    }

    // Consecutive rows close enough to be one table.
    let mut tables: Vec<Vec<Vec<Rect>>> = Vec::new();
    for row in rows {
        let height = row[0].height();
        let top = row.iter().map(|c| c.top).fold(f32::MIN, f32::max);
        match tables.last_mut() {
            Some(table)
                if {
                    let last = &table[table.len() - 1];
                    let last_bottom = last.iter().map(|c| c.bottom).fold(f32::MAX, f32::min);
                    last_bottom - top <= height * 1.5
                        && (last[0].left - row[0].left).abs() <= height * 4.0
                } =>
            {
                table.push(row)
            }
            _ => tables.push(vec![row]),
        }
    }

    tables
        .into_iter()
        .filter(|table| table.len() >= 2)
        .map(|table| {
            let lefts = cluster(table.iter().flatten().map(|c| c.left).collect(), GRID_SNAP);
            let mut cells = Vec::new();
            let mut rect = table[0][0];
            for (r, row) in table.iter().enumerate() {
                for chip in row {
                    let col = lefts
                        .iter()
                        .enumerate()
                        .min_by(|a, b| (a.1 - chip.left).abs().total_cmp(&(b.1 - chip.left).abs()))
                        .map_or(0, |(i, _)| i);
                    rect = rect.union(chip);
                    cells.push(Cell {
                        row: r,
                        col,
                        row_span: 1,
                        col_span: 1,
                        rect: *chip,
                    });
                }
            }
            Region {
                rect,
                kind: Kind::Table(Grid {
                    rows: table.len(),
                    cols: lefts.len(),
                    cells,
                }),
            }
        })
        .collect()
}

fn same_rect(a: &Rect, b: &Rect) -> bool {
    (a.left - b.left).abs() < SNAP
        && (a.right - b.right).abs() < SNAP
        && (a.top - b.top).abs() < SNAP
        && (a.bottom - b.bottom).abs() < SNAP
}

// ── Tables ruled between rows only ─────────────────────────────────

/// Span starts closer than this begin one column.
const COLUMN_SNAP: f32 = 4.0;

/// A table that draws only the rules between its rows (Word's horizontal
/// borders, the LA summary's lists of districts): the rules give the rows,
/// and the columns begin where text starts in two rows or more. A text
/// running across a column's start (a caption band) merges the columns it
/// covers. Two such tables printed side by side are two sets of rules, so
/// two tables, never one row read across both.
fn row_ruled_table(component: &[Rule], chars: &[RawChar]) -> Option<Region> {
    if component.iter().any(|r| !r.horizontal) {
        return None;
    }
    let mut ys = cluster(component.iter().map(|r| r.pos).collect(), GRID_SNAP);
    ys.reverse();
    if ys.len() < 3 {
        return None;
    }
    let (left, right) = extent(component);
    let rect = Rect {
        left,
        bottom: ys[ys.len() - 1],
        right,
        top: ys[0],
    };

    // Each row's spans, as (start, end).
    let mut bands: Vec<Vec<(f32, f32)>> = vec![Vec::new(); ys.len() - 1];
    for span in group_chars_into_spans(&chars_in(&rect, chars)) {
        let mid = span.y - span.height / 2.0;
        if let Some(r) = ys.windows(2).position(|b| mid <= b[0] && mid >= b[1]) {
            bands[r].push((span.x, span.x + span.width));
        }
    }
    let starts = column_starts(&bands);
    let rows = bands.iter().filter(|b| !b.is_empty()).count();
    if starts.len() < 2 || rows < 2 {
        return None;
    }
    // The first column runs from the rules' left end, wherever its text starts.
    let mut xs = vec![rect.left];
    xs.extend(starts[1..].iter().map(|x| x - COLUMN_SNAP));
    xs.push(rect.right);

    Some(Region {
        rect,
        kind: Kind::Table(Grid {
            rows: ys.len() - 1,
            cols: xs.len() - 1,
            cells: band_cells(&bands, &xs, &ys),
        }),
    })
}

/// Where text starts in two rows or more, left to right.
fn column_starts(bands: &[Vec<(f32, f32)>]) -> Vec<f32> {
    let starts_in = |x: f32| {
        bands
            .iter()
            .filter(|b| b.iter().any(|s| (s.0 - x).abs() <= COLUMN_SNAP))
            .count()
    };
    cluster(bands.iter().flatten().map(|s| s.0).collect(), COLUMN_SNAP)
        .into_iter()
        .filter(|&x| starts_in(x) >= 2)
        .collect()
}

/// Each row's cells between column edges `xs`, a cell spanning every column
/// start its text runs across.
fn band_cells(bands: &[Vec<(f32, f32)>], xs: &[f32], ys: &[f32]) -> Vec<Cell> {
    let cols = xs.len() - 1;
    let crosses = |band: &[(f32, f32)], x: f32| {
        band.iter()
            .any(|s| s.0 < x - GRID_SNAP && s.1 > x + GRID_SNAP)
    };
    let mut cells = Vec::new();
    for (r, band) in bands.iter().enumerate() {
        let mut col = 0;
        while col < cols {
            let mut span = 1;
            while col + span < cols && crosses(band, xs[col + span]) {
                span += 1;
            }
            cells.push(Cell {
                row: r,
                col,
                row_span: 1,
                col_span: span,
                rect: Rect {
                    left: xs[col],
                    bottom: ys[r + 1],
                    right: xs[col + span],
                    top: ys[r],
                },
            });
            col += span;
        }
    }
    cells
}

// ── Assigning text and rendering ───────────────────────────────────

/// Splits the page's characters into those each region holds and those it
/// does not, keeping content order within each part.
pub(super) fn assign(regions: &[Region], chars: Vec<RawChar>) -> (Vec<RawChar>, Vec<Vec<RawChar>>) {
    let mut free = Vec::new();
    let mut held: Vec<Vec<RawChar>> = regions.iter().map(|_| Vec::new()).collect();
    for c in chars {
        let (x, y) = centre(&c);
        let innermost = regions
            .iter()
            .enumerate()
            .filter(|(_, r)| r.rect.contains(x, y, 0.5))
            .min_by(|a, b| a.1.rect.area().total_cmp(&b.1.rect.area()))
            .map(|(i, _)| i);
        match innermost {
            Some(i) => held[i].push(c),
            None => free.push(c),
        }
    }
    (free, held)
}

/// The text of a set of characters as one line of words, lines joined by a
/// space, escaped like the rest of the page's markdown ("R1**§" keeps its
/// note markers rather than turning bold). Bold is kept where it sets part of
/// a cell apart ("**Agriculture** One-Family Dwellings", "**Height:** 45 ft");
/// a cell set wholly in bold is plain text.
fn cell_text(chars: &[RawChar]) -> String {
    if chars.iter().filter(|c| c.vertical).count() * 2 > chars.len() {
        return vertical_text(chars);
    }
    let lines = with_raised_marks(group_spans_into_lines_owned(group_chars_into_spans(chars)));
    let wordy = |s: &&TextSpan| s.text.chars().any(char::is_alphanumeric);
    let spans = || lines.iter().flat_map(|l| &l.spans).filter(wordy);
    let mixed = spans().any(|s| s.bold) && spans().any(|s| !s.bold);

    // Escaping only prefixes a character, so the text written so far starts,
    // is empty and ends a broken word exactly as the plain text would.
    let mut out = String::new();
    let mut bold = false;
    for line in &lines {
        for (i, span) in line.spans.iter().enumerate() {
            let text = span.text.trim();
            if text.is_empty() {
                continue;
            }
            let space = !out.is_empty()
                && if i == 0 {
                    !ends_broken_word(&out)
                } else {
                    needs_space_between(&line.spans[i - 1], span)
                };
            let span_bold = mixed && if wordy(&span) { span.bold } else { bold };
            if bold && !span_bold {
                out.push_str("**");
            }
            if space {
                out.push(' ');
            }
            if span_bold && !bold {
                out.push_str("**");
            }
            bold = span_bold;
            escape_markdown_into(&mut out, text);
        }
    }
    if bold {
        out.push_str("**");
    }
    out
}

/// A cell set on its side ("Urban General" up a narrow column): its glyphs
/// in the order the page draws them, which is reading order, not the order
/// of their positions, which runs backwards or top to bottom.
fn vertical_text(chars: &[RawChar]) -> String {
    let mut text = String::new();
    for c in chars {
        if c.space_before && !text.is_empty() {
            text.push(' ');
        }
        text.push(c.ch);
    }
    let mut out = String::new();
    escape_markdown_into(&mut out, text.trim());
    out
}

/// Lines with a raised, smaller mark set back on the line it marks: a
/// footnote's "(1)" printed above "25" reads "25(1)", not "(1) 25".
fn with_raised_marks(lines: Vec<TextLine>) -> Vec<TextLine> {
    let size = |l: &TextLine| l.spans.iter().map(|s| s.font_size).fold(0.0_f32, f32::max);
    let bottom = |l: &TextLine| {
        l.spans
            .iter()
            .map(|s| s.y - s.height)
            .fold(f32::MAX, f32::min)
    };
    let mut out: Vec<TextLine> = Vec::with_capacity(lines.len());
    let mut raised: Vec<TextSpan> = Vec::new();
    for (k, line) in lines.iter().enumerate() {
        // Lines run top to bottom: a mark sits above the line it marks, set
        // smaller, its bottom inside that line's height.
        let mark = line_to_text(line);
        let marks = mark.trim().chars().count() <= RAISED_MARK_CHARS
            && !mark.trim().contains(' ')
            && mark.chars().filter(|c| c.is_alphabetic()).count() <= 1
            && lines.get(k + 1).is_some_and(|next| {
                size(line) < size(next) * RAISED_MARK_SIZE
                    && bottom(line) < next.y
                    && bottom(line) > bottom(next)
            });
        if marks {
            raised.extend(line.spans.iter().cloned());
        } else {
            let mut spans = line.spans.clone();
            spans.append(&mut raised);
            spans.sort_by(|a, b| a.x.total_cmp(&b.x));
            out.push(TextLine { y: line.y, spans });
        }
    }
    out
}

/// A line set below this share of the next line's size, and overlapping it,
/// is a mark raised on it, when it is one short token of numbers or signs
/// ("(1)", "(2,3)", "*", "(a)"): a smaller word above another ("FENCE") is a
/// label of its own.
const RAISED_MARK_SIZE: f32 = 0.85;
const RAISED_MARK_CHARS: usize = 6;

/// Whether text ends in a word broken at a hyphen ("cul-" before "de-sac"):
/// the next line continues the word, with no space.
pub(super) fn ends_broken_word(text: &str) -> bool {
    let mut tail = text.chars().rev();
    tail.next() == Some('-') && tail.next().is_some_and(char::is_alphabetic)
}

/// A table region as markdown: captions for title rows at its top, one header
/// row naming each column in full, then one row per data row with every value
/// it holds, merged values included.
pub(super) fn render_table(
    grid: &Grid,
    chars: &[RawChar],
    fills: &[Fill],
    carried: Option<&TableTail>,
) -> RenderedTable {
    let by_cell = chars_by_cell(grid, chars);
    let texts: Vec<String> = by_cell.iter().map(|cs| cell_text(cs)).collect();
    let labels = colour_labels(grid, &texts, fills);

    // Rows are read in grid order; a row holding nothing is skipped.
    let rows_with_text: Vec<usize> = (0..grid.rows)
        .filter(|&r| holding(grid, &texts, r).next().is_some())
        .collect();
    let (mut md, remaining) = captions(grid, &by_cell, &texts, &rows_with_text);
    let Some(&first) = remaining.first() else {
        return RenderedTable {
            markdown: md.trim_end().to_string(),
            tail: None,
        };
    };

    let header_height = header_rows(grid, first, &texts);
    let has_header = header_height > 1
        || (looks_like_header(grid, &by_cell, &texts, remaining)
            && !keyed_rows(grid, &by_cell, remaining));
    let data_start = if has_header {
        first + header_height
    } else {
        first
    };
    let (header_part, data_part) = split_at_header(grid, &by_cell, &texts, first, data_start);
    let mut matrix = filled_matrix(grid, &labels, &header_part, &data_part, data_start);
    let header = column_names(&matrix[first..data_start], grid.cols);
    if let Some(tail) = carried.filter(|t| t.header == header) {
        continue_from(grid, &mut matrix, &data_part, data_start, tail);
    }

    push_header(&mut md, &header);
    push_body(
        &mut md,
        grid,
        &by_cell,
        &texts,
        &matrix[data_start..],
        data_start,
        &header,
    );
    RenderedTable {
        markdown: md.trim_end().to_string(),
        tail: table_tail(grid, &matrix, data_start, header),
    }
}

/// Each column's name: the header rows' texts in it, top to bottom, joined
/// ("Maximum Height - Stories"). A part ending in a hyphen is the start of a
/// code the next part completes: "RS-", "1-", "1" name the zone "RS-1-1".
fn column_names(header_rows: &[Vec<String>], cols: usize) -> Vec<String> {
    (0..cols)
        .map(|c| {
            let mut parts: Vec<&str> = Vec::new();
            for row in header_rows {
                let part = row[c].as_str();
                if !part.is_empty() && !parts.contains(&part) {
                    parts.push(part);
                }
            }
            let mut name = String::new();
            for part in parts {
                if !name.is_empty() && !name.ends_with('-') {
                    name.push_str(" - ");
                }
                name.push_str(part);
            }
            name
        })
        .collect()
}

/// The data rows (`body`, grid rows from `data_start`), with a note printed
/// across the table written as paragraphs and the header repeated after it.
fn push_body(
    md: &mut String,
    grid: &Grid,
    by_cell: &[Vec<RawChar>],
    texts: &[String],
    body: &[Vec<String>],
    data_start: usize,
    header: &[String],
) {
    let mut under_header = true;
    let mut previous: Option<&Vec<String>> = None;
    for (r, row) in (data_start..).zip(body) {
        if let Some(i) = note_cell(grid, texts, r) {
            if grid.cells[i].row == r {
                md.push('\n');
                md.push_str(&spans_to_markdown(&group_chars_into_spans(&by_cell[i])));
                md.push_str("\n\n");
                under_header = false;
                previous = None;
            }
            continue;
        }
        // Rows the grid divides only for some column's sake can repeat an
        // earlier row exactly once every value is filled in.
        if !has_value(row) || previous == Some(row) {
            continue;
        }
        if !under_header {
            push_header(md, header);
            under_header = true;
        }
        push_row(md, row);
        previous = Some(row);
    }
}

fn has_value(row: &[String]) -> bool {
    row.iter().any(|v| !v.is_empty())
}

/// The cells holding text in grid row `r`, merged cells from rows above
/// included, with their indices.
fn holding<'a>(
    grid: &'a Grid,
    texts: &'a [String],
    r: usize,
) -> impl Iterator<Item = (usize, &'a Cell, &'a String)> {
    grid.cells
        .iter()
        .zip(texts)
        .enumerate()
        .filter(move |(_, (c, t))| !t.is_empty() && covers(c, r))
        .map(|(i, (c, t))| (i, c, t))
}

/// The one cell holding text in row `r`, if there is exactly one.
fn sole_text(grid: &Grid, texts: &[String], r: usize) -> Option<usize> {
    let mut cells = holding(grid, texts, r);
    match (cells.next(), cells.next()) {
        (Some((i, _, _)), None) => Some(i),
        _ => None,
    }
}

/// A table's markdown, and what the next page needs to continue it.
pub(super) struct RenderedTable {
    pub markdown: String,
    pub tail: Option<TableTail>,
}

/// A table's column names and its last row. A table printed across a page
/// break goes on under the same header, and a merged cell the break cuts
/// holds its value on both sides of it (the LA summary's "Unlimited (9)" for
/// R4 on one page and RAS4 and R5 on the next).
pub(super) struct TableTail {
    header: Vec<String>,
    /// Each column's value in the last row, with the columns of the merged
    /// cell that held it (`None` for a cell of its own).
    last: Vec<(String, Option<(usize, usize)>)>,
}

fn table_tail(
    grid: &Grid,
    matrix: &[Vec<String>],
    data_start: usize,
    header: Vec<String>,
) -> Option<TableTail> {
    let r = (data_start..grid.rows)
        .rev()
        .find(|&r| has_value(&matrix[r]))?;
    let last = (0..grid.cols)
        .map(|c| {
            let merged = grid
                .cells
                .iter()
                .find(|cell| {
                    covers(cell, r)
                        && (cell.col..cell.col + cell.col_span).contains(&c)
                        && (cell.row_span > 1 || cell.col_span > 1)
                })
                .map(|cell| (cell.col, cell.col_span));
            (matrix[r][c].clone(), merged)
        })
        .collect();
    Some(TableTail { header, last })
}

/// Writes the previous page's values into the cells that continue them: a
/// blank cell at the top of the table as wide as a merged cell ending the
/// previous page, and a cell completing a value the break cut mid-sentence
/// ("… pursuant to" / "Sec. 12.21 A.16 of the LAMC"). A table that starts
/// with a group label ("Commercial") starts afresh; a blank under a cell
/// that was not merged may mean "not permitted", and stays blank.
fn continue_from(
    grid: &Grid,
    matrix: &mut [Vec<String>],
    texts: &[String],
    data_start: usize,
    tail: &TableTail,
) {
    let Some(first) = (data_start..grid.rows).find(|&r| has_value(&matrix[r])) else {
        return;
    };
    if holding(grid, texts, first).any(|(_, c, _)| c.col_span == grid.cols) {
        return;
    }
    for (cell, text) in grid.cells.iter().zip(texts) {
        if !covers(cell, first) {
            continue;
        }
        for c in cell.col..cell.col + cell.col_span {
            let (value, merged) = &tail.last[c];
            if value.is_empty() {
                continue;
            }
            if text.is_empty() {
                if *merged == Some((cell.col, cell.col_span)) {
                    for row in &mut matrix[first..cell.row + cell.row_span] {
                        if row[c].is_empty() {
                            row[c].clone_from(value);
                        }
                    }
                }
            } else if cut_off(value) && matrix[first][c] == *text {
                matrix[first][c] = format!("{value} {text}");
            }
        }
    }
}

/// Whether a value stops mid-sentence, on a word that needs another after it.
fn cut_off(value: &str) -> bool {
    value
        .split_whitespace()
        .last()
        .is_some_and(is_function_word)
}

/// The cell holding a note printed across a table ("Loading Space: …; Open
/// Space: …" between two groups of zones): the only text in its row, as wide
/// as the table, longer than a group label and ending a sentence, which a
/// group label ("… Land Use Class/Type:") never does. It is read as
/// paragraphs between two parts of the table, each under the header.
fn note_cell(grid: &Grid, texts: &[String], r: usize) -> Option<usize> {
    sole_text(grid, texts, r).filter(|&i| {
        grid.cells[i].col_span == grid.cols
            && texts[i].split_whitespace().count() > HEADER_MAX_WORDS
            && texts[i].trim_end().ends_with('.')
    })
}

fn covers(cell: &Cell, r: usize) -> bool {
    cell.row <= r && r < cell.row + cell.row_span
}

/// Title bands at the top of a table (one text across most of the width) as
/// paragraphs, and the rows after them.
fn captions<'a>(
    grid: &Grid,
    by_cell: &[Vec<RawChar>],
    texts: &[String],
    rows: &'a [usize],
) -> (String, &'a [usize]) {
    let mut md = String::new();
    let mut remaining = rows;
    while let Some(&row) = remaining.first() {
        let Some(i) = sole_text(grid, texts, row)
            .filter(|&i| grid.cells[i].col_span * 2 > grid.cols && grid.cells[i].row == row)
        else {
            break;
        };
        // A caption of several lines (a title over a legend: "P = Permitted
        // Use", "[1] Mayor and Council …") is read as paragraphs, so its
        // entries stay apart; a one-line title is its text.
        let lines = group_spans_into_lines_owned(group_chars_into_spans(&by_cell[i]));
        if lines.len() > 1 {
            md.push_str(&spans_to_markdown(&group_chars_into_spans(&by_cell[i])));
        } else {
            md.push_str(&texts[i]);
        }
        md.push_str("\n\n");
        remaining = &remaining[1..];
    }
    (md, remaining)
}

/// Each cell's text as the header reads it and as the data rows read it.
///
/// A header cell with no rule beneath it runs on over data rows (the LA
/// summary's "Parking Required"): what it holds below the header's bottom
/// edge is the data rows' value (text carried over from the previous page),
/// what it holds above is the column's name.
fn split_at_header(
    grid: &Grid,
    by_cell: &[Vec<RawChar>],
    texts: &[String],
    first: usize,
    data_start: usize,
) -> (Vec<String>, Vec<String>) {
    let mut header_part = texts.to_vec();
    let mut data_part = texts.to_vec();
    let header_bottom = grid
        .cells
        .iter()
        .filter(|c| data_start > first && c.row >= first && c.row + c.row_span == data_start)
        .map(|c| c.rect.bottom)
        .fold(f32::NEG_INFINITY, f32::max);
    if !header_bottom.is_finite() {
        return (header_part, data_part);
    }
    for (i, cell) in grid.cells.iter().enumerate() {
        if cell.row < data_start && cell.row + cell.row_span > data_start {
            let (above, below): (Vec<RawChar>, Vec<RawChar>) = by_cell[i]
                .iter()
                .cloned()
                .partition(|c| centre(c).1 >= header_bottom);
            header_part[i] = cell_text(&above);
            data_part[i] = cell_text(&below);
        }
    }
    (header_part, data_part)
}

/// Each character in the cell its centre falls in (or the nearest).
fn chars_by_cell(grid: &Grid, chars: &[RawChar]) -> Vec<Vec<RawChar>> {
    let mut by_cell: Vec<Vec<RawChar>> = grid.cells.iter().map(|_| Vec::new()).collect();
    for c in chars {
        let (x, y) = centre(c);
        let index = grid
            .cells
            .iter()
            .position(|cell| cell.rect.contains(x, y, 0.5))
            .unwrap_or_else(|| nearest_cell(&grid.cells, x, y));
        by_cell[index].push(c.clone());
    }
    by_cell
}

/// Each cell's text, followed by its colour's name when colour is what the
/// table says (a zoning key of coloured chips, most cells coloured, two
/// colours or more), not when it only shades one column or the group rows of
/// a plain table (the zone column of the LA summary).
fn colour_labels(grid: &Grid, texts: &[String], fills: &[Fill]) -> Vec<Option<&'static str>> {
    let colours: Vec<Option<&'static str>> = grid
        .cells
        .iter()
        .map(|cell| cell_fill(fills, &cell.rect).and_then(|f| colour_name(f.rgb)))
        .collect();
    let mut distinct: Vec<&str> = colours
        .iter()
        .zip(texts)
        .filter(|(_, t)| !t.is_empty())
        .filter_map(|(c, _)| *c)
        .collect();
    let coloured = distinct.len();
    distinct.sort_unstable();
    distinct.dedup();
    let text_cells = texts.iter().filter(|t| !t.is_empty()).count();
    if distinct.len() >= 2 && coloured * 2 >= text_cells {
        colours
    } else {
        vec![None; colours.len()]
    }
}

/// The grid with every value written in each position it covers.
///
/// A value merged over rows or columns holds for each ("Unlimited (9)" for
/// every zone the cell spans, a check mark across "Ground Story" and "Upper
/// Story"), so a reader of one row never has to look at another. Two
/// exceptions: a cell as wide as the table (a title or a group label) is
/// written once, and so is a cell over most of the row with nothing after it
/// ("1.000 Residential" across every zone of Juneau's use table); a wide
/// value followed by other values in its row (RFAR across five height
/// districts of the LA summary) is still repeated.
fn filled_matrix(
    grid: &Grid,
    labels: &[Option<&'static str>],
    header_part: &[String],
    data_part: &[String],
    data_start: usize,
) -> Vec<Vec<String>> {
    let mut matrix = vec![vec![String::new(); grid.cols]; grid.rows];
    let text_at = |i: usize, r: usize| {
        if r < data_start {
            &header_part[i]
        } else {
            &data_part[i]
        }
    };
    for (i, cell) in grid.cells.iter().enumerate() {
        let last_row = cell.row + cell.row_span;
        let beside = || {
            grid.cells.iter().enumerate().filter(move |&(k, other)| {
                k != i && other.row < last_row && other.row + other.row_span > cell.row
            })
        };
        let followed = || {
            beside().any(|(k, other)| {
                !header_part[k].is_empty() && other.col >= cell.col + cell.col_span
            })
        };
        // A label alone in its row ("Variable Mass" over the rows it groups)
        // is written once, however many columns it spans.
        let alone = || !beside().any(|(k, _)| !data_part[k].is_empty());
        let full_width = cell.col_span == grid.cols;
        // A row's label spanning the label columns is written once.
        let label = cell.col == 0 && cell.row >= data_start;
        let data_cols = if cell.col_span > 1
            && !full_width
            && !label
            && (cell.col_span * 2 <= grid.cols || followed())
            && (cell.row < data_start || !alone())
        {
            cell.col_span
        } else {
            1
        };
        let rows = if full_width { 1 } else { cell.row_span };
        for (r, row) in matrix.iter_mut().enumerate().skip(cell.row).take(rows) {
            let text = text_at(i, r);
            if text.is_empty() {
                continue;
            }
            // In the header a group heading names every column under it
            // ("RS-" over RS-1-1 … RS-1-7), however wide.
            let cols = if r < data_start && !full_width {
                cell.col_span
            } else {
                data_cols
            };
            let value = match labels[i] {
                Some(name) => format!("{text} [{name}]"),
                None => text.clone(),
            };
            for slot in row.iter_mut().skip(cell.col).take(cols) {
                slot.clone_from(&value);
            }
        }
    }
    matrix
}

/// One markdown table row.
pub(super) fn push_row(md: &mut String, row: &[String]) {
    md.push_str("| ");
    md.push_str(&row.join(" | "));
    md.push_str(" |\n");
}

/// A markdown table's header row and the separator below it.
pub(super) fn push_header(md: &mut String, header: &[String]) {
    push_row(md, header);
    md.push('|');
    md.push_str(&" --- |".repeat(header.len()));
    md.push('\n');
}

/// A value, not a name: digits and punctuation only (".65", "50%", "6,000").
fn is_numeric(text: &str) -> bool {
    let t = text.trim();
    !t.is_empty() && t.chars().any(|c| c.is_ascii_digit()) && !t.chars().any(char::is_alphabetic)
}

/// Words above which a cell is content, not a column name ("Lot Size and
/// Residential Floor Area Ratio" is seven).
const HEADER_MAX_WORDS: usize = 8;

/// Whether a row's cells name columns: short cells, no values. A table
/// without a header (a continuation page, a two-column list of terms) starts
/// with data, and data must not be made the column names.
pub(super) fn is_header_row<S: AsRef<str>>(cells: &[S]) -> bool {
    short_cells(cells) && !holds_values(cells)
}

/// Whether a row has text, and no cell longer than a column name.
fn short_cells<S: AsRef<str>>(cells: &[S]) -> bool {
    let cells: Vec<&str> = cells
        .iter()
        .map(AsRef::as_ref)
        .filter(|t| !t.trim().is_empty())
        .collect();
    !cells.is_empty()
        && cells
            .iter()
            .all(|t| t.split_whitespace().count() <= HEADER_MAX_WORDS)
}

/// Whether every cell holding text in grid row `r` is set mostly in bold,
/// judged on letters and digits (a note mark like "‡" often has no bold face).
fn bold_row(grid: &Grid, by_cell: &[Vec<RawChar>], r: usize) -> bool {
    bold_cells(grid, by_cell, r).iter().all(|&(_, bold)| bold)
}

/// Each cell starting in grid row `r` that holds letters or digits, and
/// whether most of them are bold (a note mark like "‡" often has no bold
/// face, so marks are not counted).
fn bold_cells(grid: &Grid, by_cell: &[Vec<RawChar>], r: usize) -> Vec<(usize, bool)> {
    grid.cells
        .iter()
        .zip(by_cell)
        .filter(|(c, _)| c.row == r)
        .filter_map(|(c, cs)| {
            let (bold, all) = cs
                .iter()
                .filter(|ch| ch.ch.is_alphanumeric())
                .fold((0, 0), |(b, a), ch| (b + usize::from(ch.bold), a + 1));
            (all > 0).then_some((c.col, bold * 2 > all))
        })
        .collect()
}

/// Two values or more make a row data, whatever else it holds.
fn holds_values<S: AsRef<str>>(cells: &[S]) -> bool {
    cells.iter().filter(|t| is_numeric(t.as_ref())).count() >= 2
}

/// The texts of the cells starting in grid row `r`.
fn row_texts<'a>(grid: &Grid, texts: &'a [String], r: usize) -> Vec<&'a String> {
    grid.cells
        .iter()
        .zip(texts)
        .filter(|(c, _)| c.row == r)
        .map(|(_, t)| t)
        .collect()
}

/// Whether the first of `rows` names columns. Numbers can be column names
/// (height districts "1 1L 2 3 4"): a row of them is the header when it
/// alone is set in bold.
fn looks_like_header(
    grid: &Grid,
    by_cell: &[Vec<RawChar>],
    texts: &[String],
    rows: &[usize],
) -> bool {
    let [first, rest @ ..] = rows else {
        return false;
    };
    let cells = row_texts(grid, texts, *first);
    is_header_row(&cells)
        || (short_cells(&cells)
            && rest.first().is_some_and(|&next| {
                bold_row(grid, by_cell, *first) && !bold_row(grid, by_cell, next)
            }))
}

/// Whether the first two of `rows` are entries of a list keyed by bold terms
/// ("**O** Oil Drilling District"). A header is styled alike across its
/// columns; a row that bolds only some of its cells, exactly as the row below
/// does, is data.
fn keyed_rows(grid: &Grid, by_cell: &[Vec<RawChar>], rows: &[usize]) -> bool {
    let [a, b, ..] = rows else {
        return false;
    };
    let (first, second) = (bold_cells(grid, by_cell, *a), bold_cells(grid, by_cell, *b));
    first == second && first.iter().any(|p| p.1) && first.iter().any(|p| !p.1)
}

/// How many grid rows, from `first`, the header occupies. More than one only
/// when `first` holds a group heading (spanning several columns, not the
/// whole width) that the row below divides into sub-headings: the header
/// then ends where those sub-headings end ("Maximum Height" over "Stories"
/// and "Feet"). Taken from the headings, not the tallest cell: a header cell
/// with no rule beneath it ("Parking Required" in the LA summary) runs on
/// through data rows. A row below that holds values (".65 .55 .45") is data,
/// not sub-headings, whatever spans above it.
fn header_rows(grid: &Grid, first: usize, texts: &[String]) -> usize {
    if let Some(rows) = corner_rows(grid, first, texts) {
        return rows;
    }
    if holds_values(&row_texts(grid, texts, first + 1)) {
        return 1;
    }
    let end = grid
        .cells
        .iter()
        .filter(|g| g.row == first && g.row_span == 1 && g.col_span > 1 && g.col_span < grid.cols)
        .flat_map(|g| {
            grid.cells.iter().filter(move |c| {
                c.row == first + 1 && c.col >= g.col && c.col + c.col_span <= g.col + g.col_span
            })
        })
        .map(|c| c.row + c.row_span)
        .max();
    end.map_or(1, |end| end - first)
}

/// The rows a header's corner cell spans, when it spans several: the cell
/// at the top of the first column that names the row labels ("Development
/// Regulations" beside the four rows of San Diego's zone designator, "1st &
/// 2nd >> RS-", "3rd >> 1-", "4th >> 1"). The rows beside it are header rows
/// whatever they hold, numbers included; rows holding sentences are not.
fn corner_rows(grid: &Grid, first: usize, texts: &[String]) -> Option<usize> {
    let (corner, text) = grid
        .cells
        .iter()
        .zip(texts)
        .find(|(c, t)| c.row == first && c.col == 0 && !t.is_empty())?;
    let rows = corner.row_span;
    let beside_short = grid
        .cells
        .iter()
        .zip(texts)
        .filter(|(c, _)| c.col > 0 && c.row >= first && c.row < first + rows)
        .all(|(_, t)| t.split_whitespace().count() <= HEADER_MAX_WORDS);
    (rows > 1 && first + rows < grid.rows && !is_numeric(text) && beside_short).then_some(rows)
}

fn nearest_cell(cells: &[Cell], x: f32, y: f32) -> usize {
    let distance = |r: &Rect| {
        let dx = (r.left - x).max(0.0).max(x - r.right);
        let dy = (r.bottom - y).max(0.0).max(y - r.top);
        dx * dx + dy * dy
    };
    cells
        .iter()
        .enumerate()
        .min_by(|a, b| distance(&a.1.rect).total_cmp(&distance(&b.1.rect)))
        .map_or(0, |(i, _)| i)
}

/// The smallest fill under a cell's centre: its background, not the page's.
fn cell_fill<'a>(fills: &'a [Fill], rect: &Rect) -> Option<&'a Fill> {
    let (x, y) = (
        (rect.left + rect.right) / 2.0,
        (rect.top + rect.bottom) / 2.0,
    );
    fills
        .iter()
        .filter(|f| f.rect.contains(x, y, 0.0) && !is_near_white(f.rgb))
        .min_by(|a, b| a.rect.area().total_cmp(&b.rect.area()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(pos: f32, start: f32, end: f32) -> Rule {
        Rule {
            horizontal: true,
            pos,
            start,
            end,
        }
    }

    fn v(pos: f32, start: f32, end: f32) -> Rule {
        Rule {
            horizontal: false,
            pos,
            start,
            end,
        }
    }

    fn table(regions: &[Region]) -> &Grid {
        match &regions[0].kind {
            Kind::Table(grid) => grid,
            Kind::TextBox => panic!("expected a table"),
        }
    }

    #[test]
    fn a_full_grid_is_a_table_of_its_cells() {
        // 3 rows x 2 columns, every cell framed.
        let mut rules = vec![
            h(300.0, 0.0, 200.0),
            h(280.0, 0.0, 200.0),
            h(260.0, 0.0, 200.0),
            h(240.0, 0.0, 200.0),
        ];
        rules.extend([
            v(0.0, 240.0, 300.0),
            v(100.0, 240.0, 300.0),
            v(200.0, 240.0, 300.0),
        ]);
        let regions = ruled_regions(&rules, &[]);
        assert_eq!(regions.len(), 1);
        let grid = table(&regions);
        assert_eq!((grid.rows, grid.cols, grid.cells.len()), (3, 2, 6));
    }

    #[test]
    fn a_missing_inner_rule_merges_the_cells_it_would_have_divided() {
        // The first row has no vertical divider: one cell spanning both columns.
        let mut rules = vec![
            h(300.0, 0.0, 200.0),
            h(280.0, 0.0, 200.0),
            h(260.0, 0.0, 200.0),
        ];
        rules.extend([
            v(0.0, 260.0, 300.0),
            v(200.0, 260.0, 300.0),
            v(100.0, 260.0, 280.0),
        ]);
        let regions = ruled_regions(&rules, &[]);
        let grid = table(&regions);
        let title = grid.cells.iter().find(|c| c.row == 0).expect("title cell");
        assert_eq!(title.col_span, 2);
        assert_eq!(grid.cells.iter().filter(|c| c.row == 1).count(), 2);
    }

    #[test]
    fn a_repeated_header_with_other_column_widths_is_a_separate_table() {
        // Rows 300-280 divide at x=60/120, rows 280-240 at x=80/140: two layouts touching.
        let mut rules = vec![
            h(300.0, 0.0, 200.0),
            h(280.0, 0.0, 200.0),
            h(260.0, 0.0, 200.0),
            h(240.0, 0.0, 200.0),
        ];
        rules.extend([v(0.0, 240.0, 300.0), v(200.0, 240.0, 300.0)]);
        rules.extend([
            v(60.0, 280.0, 300.0),
            v(120.0, 280.0, 300.0),
            v(80.0, 240.0, 280.0),
            v(140.0, 240.0, 280.0),
        ]);
        let regions = ruled_regions(&rules, &[]);
        assert_eq!(regions.len(), 2);
        // The lone header row is too short to be a table on its own; the rows
        // below keep their own three columns instead of five slivers.
        assert!(matches!(regions[0].kind, Kind::TextBox));
        match &regions[1].kind {
            Kind::Table(grid) => assert_eq!((grid.rows, grid.cols), (2, 3)),
            Kind::TextBox => panic!("expected the lower rows to be a table"),
        }
    }

    #[test]
    fn a_frame_without_inner_columns_is_a_text_box() {
        let rules = [
            h(300.0, 0.0, 200.0),
            h(250.0, 0.0, 200.0),
            h(200.0, 0.0, 200.0),
            v(0.0, 200.0, 300.0),
            v(200.0, 200.0, 300.0),
        ];
        let regions = ruled_regions(&rules, &[]);
        assert!(matches!(regions[0].kind, Kind::TextBox));
    }

    #[test]
    fn a_lone_underline_is_no_region() {
        assert!(ruled_regions(&[h(300.0, 0.0, 120.0)], &[]).is_empty());
    }

    #[test]
    fn collinear_touching_rules_merge() {
        let merged = merge_rules(&[
            h(100.0, 0.0, 50.0),
            h(100.5, 50.0, 120.0),
            h(100.0, 200.0, 250.0),
        ]);
        assert_eq!(merged.len(), 2);
        assert!(merged.iter().any(|r| r.start == 0.0 && r.end == 120.0));
    }

    fn chip(left: f32, top: f32) -> Rect {
        Rect {
            left,
            bottom: top - 12.0,
            right: left + 30.0,
            top,
        }
    }

    #[test]
    fn rows_of_chips_are_a_table_with_columns_by_left_edge() {
        let green = (106, 168, 79);
        let fills: Vec<Fill> = [
            chip(0.0, 100.0),
            chip(32.0, 100.0),
            chip(64.0, 100.0),
            chip(0.0, 86.0),
            chip(32.0, 86.0),
        ]
        .into_iter()
        .map(|rect| Fill { rect, rgb: green })
        .collect();
        let refs: Vec<&Fill> = fills.iter().collect();
        let regions = chip_tables(&refs);
        assert_eq!(regions.len(), 1);
        let grid = table(&regions);
        assert_eq!((grid.rows, grid.cols, grid.cells.len()), (2, 3, 5));
    }

    #[test]
    fn a_single_row_of_chips_is_not_a_table() {
        let fills: Vec<Fill> = [chip(0.0, 100.0), chip(32.0, 100.0)]
            .into_iter()
            .map(|rect| Fill {
                rect,
                rgb: (204, 0, 0),
            })
            .collect();
        let refs: Vec<&Fill> = fills.iter().collect();
        assert!(chip_tables(&refs).is_empty());
    }

    #[test]
    fn an_l_shaped_merge_is_cut_into_rectangles() {
        // A header cell (0,2)-(1,2) with no rule beneath it joins a merged data
        // cell (2,0)-(2,2): its bounding box would swallow (0..1, 0..1).
        let l = [(0, 2), (1, 2), (2, 0), (2, 1), (2, 2)];
        let mut got = rectangles(&l);
        got.sort_unstable();
        assert_eq!(got, vec![(0, 2, 1, 2), (2, 0, 2, 2)]);
        assert_eq!(
            rectangles(&[(0, 0), (0, 1), (1, 0), (1, 1)]),
            vec![(0, 0, 1, 1)]
        );
    }

    /// One word's characters, 5 pt wide each, top at `top`.
    fn word(text: &str, x: f32, top: f32, bold: bool) -> Vec<RawChar> {
        text.chars()
            .enumerate()
            .map(|(i, ch)| RawChar {
                ch,
                x: x + i as f32 * 5.0,
                y: top,
                width: 5.0,
                height: 8.0,
                font_size: 8.0,
                font_name: "Helvetica".into(),
                bold,
                italic: false,
                space_before: false,
                vertical: false,
            })
            .collect()
    }

    /// A bold key and its plain name per row, between rules at `ys`.
    fn keyed_list(left: f32, ys: &[f32], keys: &[(&str, &str)]) -> (Vec<Rule>, Vec<RawChar>) {
        let rules = ys.iter().map(|&y| h(y, left, left + 250.0)).collect();
        let mut chars = Vec::new();
        for (band, (key, name)) in ys.windows(2).zip(keys) {
            chars.extend(word(key, left + 2.0, band[0] - 6.0, true));
            chars.extend(word(name, left + 100.0, band[0] - 6.0, false));
        }
        (rules, chars)
    }

    #[test]
    fn rules_between_rows_only_draw_a_table_with_columns_where_text_starts() {
        let (rules, chars) = keyed_list(
            50.0,
            &[100.0, 80.0, 60.0],
            &[("O", "Oil"), ("S", "Slaughter")],
        );
        let region = row_ruled_table(&rules, &chars).expect("a table");
        let grid = table(std::slice::from_ref(&region));
        assert_eq!((grid.rows, grid.cols, grid.cells.len()), (2, 2, 4));
        assert_eq!(
            render_table(grid, &chars, &[], None).markdown,
            "|  |  |\n| --- | --- |\n| O | Oil |\n| S | Slaughter |"
        );
    }

    #[test]
    fn row_ruled_lists_side_by_side_are_two_tables() {
        let ys = [100.0, 80.0, 60.0];
        let (mut rules, mut chars) = keyed_list(50.0, &ys, &[("O", "Oil"), ("S", "Slaughter")]);
        let (right_rules, right_chars) =
            keyed_list(320.0, &ys, &[("SN", "Sign"), ("RG", "Garage")]);
        rules.extend(right_rules);
        chars.extend(right_chars);
        let regions = ruled_regions(&rules, &chars);
        assert_eq!(regions.len(), 2);
        assert!(
            regions
                .iter()
                .all(|r| matches!(&r.kind, Kind::Table(g) if g.cols == 2))
        );
    }

    #[test]
    fn rules_with_one_column_of_text_are_no_table() {
        let rules = vec![
            h(100.0, 50.0, 300.0),
            h(80.0, 50.0, 300.0),
            h(60.0, 50.0, 300.0),
        ];
        let mut chars = word("First", 52.0, 94.0, false);
        chars.extend(word("Second", 52.0, 74.0, false));
        assert!(row_ruled_table(&rules, &chars).is_none());
    }

    #[test]
    fn a_header_names_columns_and_holds_no_values() {
        assert!(is_header_row(&[
            "Zone",
            "Lot Size and Residential Floor Area Ratio",
            ""
        ]));
        assert!(!is_header_row(&["Up to 6,000 SF", ".65", ".55"]));
        assert!(!is_header_row(&[
            "(T)",
            "Tentative zone classification, pending certain required dedications, payments or improvements"
        ]));
        assert!(!is_header_row(&["", ""]));
    }

    #[test]
    fn a_word_broken_at_a_line_end_hyphen_is_rejoined() {
        let mut chars = word("cul-", 0.0, 100.0, false);
        chars.extend(word("de-sac", 0.0, 90.0, false));
        assert_eq!(cell_text(&chars), "cul-de-sac");
        let mut chars = word("10", 0.0, 100.0, false);
        chars.extend(word("ft", 0.0, 90.0, false));
        assert_eq!(cell_text(&chars), "10 ft");
    }

    #[test]
    fn a_filled_panel_holding_a_paragraph_is_a_text_box() {
        let panel = Fill {
            rect: Rect {
                left: 0.0,
                bottom: 50.0,
                right: 150.0,
                top: 100.0,
            },
            rgb: (198, 217, 241),
        };
        let mut chars = word("Height", 2.0, 95.0, true);
        chars.extend(word("district", 2.0, 85.0, false));
        let boxes = filled_boxes(&[&panel], &[], &chars);
        assert!(matches!(
            boxes.as_slice(),
            [Region {
                kind: Kind::TextBox,
                ..
            }]
        ));
        assert!(filled_boxes(&[&panel], &[], &chars[..6]).is_empty());
    }

    fn cell(row: usize, col: usize, row_span: usize, col_span: usize) -> Cell {
        Cell {
            row,
            col,
            row_span,
            col_span,
            rect: Rect {
                left: col as f32 * 100.0,
                bottom: 100.0 - (row + row_span) as f32 * 20.0,
                right: (col + col_span) as f32 * 100.0,
                top: 100.0 - row as f32 * 20.0,
            },
        }
    }

    /// Words placed in grid cells: (row, col, text, bold).
    fn in_cells(words: &[(usize, usize, &str, bool)]) -> Vec<RawChar> {
        words
            .iter()
            .flat_map(|&(r, c, text, bold)| {
                word(
                    text,
                    c as f32 * 100.0 + 5.0,
                    100.0 - r as f32 * 20.0 - 6.0,
                    bold,
                )
            })
            .collect()
    }

    #[test]
    fn numbers_set_apart_in_bold_name_columns() {
        let grid = Grid {
            rows: 2,
            cols: 3,
            cells: vec![
                cell(0, 0, 1, 1),
                cell(0, 1, 1, 1),
                cell(0, 2, 1, 1),
                cell(1, 0, 1, 1),
                cell(1, 1, 1, 1),
                cell(1, 2, 1, 1),
            ],
        };
        let chars = in_cells(&[
            (0, 0, "Zone", true),
            (0, 1, "2", true),
            (0, 2, "3", true),
            (1, 0, "A1", true),
            (1, 1, "45", false),
            (1, 2, "n/a", false),
        ]);
        let md = render_table(&grid, &chars, &[], None).markdown;
        assert!(md.starts_with("| Zone | 2 | 3 |\n| --- |"), "{md}");
    }

    #[test]
    fn a_merged_value_cut_by_a_page_break_continues_on_the_next_page() {
        let header = [
            (0, 0, "Zone", true),
            (0, 1, "Stories", true),
            (0, 2, "Feet", true),
            (0, 3, "Front", true),
            (0, 4, "Rear", true),
        ];
        let header_cells = (0..5).map(|c| cell(0, c, 1, 1));
        // Previous page: R4, "Unlimited" merged over Stories and Feet.
        let before = Grid {
            rows: 2,
            cols: 5,
            cells: header_cells
                .clone()
                .chain([
                    cell(1, 0, 1, 1),
                    cell(1, 1, 1, 2),
                    cell(1, 3, 1, 1),
                    cell(1, 4, 1, 1),
                ])
                .collect(),
        };
        let mut words = header.to_vec();
        words.extend([
            (1, 0, "R4", false),
            (1, 1, "Unlimited", false),
            (1, 3, "15", false),
            (1, 4, "20", false),
        ]);
        let tail = render_table(&before, &in_cells(&words), &[], None).tail;

        // This page: RAS4 and R5 beside a blank cell merged over both rows
        // and both height columns; their yards are printed.
        let after = Grid {
            rows: 3,
            cols: 5,
            cells: header_cells
                .chain([
                    cell(1, 0, 1, 1),
                    cell(1, 1, 2, 2),
                    cell(1, 3, 1, 1),
                    cell(1, 4, 1, 1),
                    cell(2, 0, 1, 1),
                    cell(2, 3, 1, 1),
                    cell(2, 4, 1, 1),
                ])
                .collect(),
        };
        let mut words = header.to_vec();
        words.extend([
            (1, 0, "RAS4", false),
            (1, 3, "5", false),
            (1, 4, "5", false),
            (2, 0, "R5", false),
            (2, 3, "15", false),
            (2, 4, "15", false),
        ]);
        let chars = in_cells(&words);
        let md = render_table(&after, &chars, &[], tail.as_ref()).markdown;
        assert!(
            md.contains("| RAS4 | Unlimited | Unlimited | 5 | 5 |"),
            "{md}"
        );
        assert!(
            md.contains("| R5 | Unlimited | Unlimited | 15 | 15 |"),
            "{md}"
        );
        // Without the previous page, the page prints them blank.
        let alone = render_table(&after, &chars, &[], None).markdown;
        assert!(alone.contains("| RAS4 |  |  | 5 | 5 |"), "{alone}");
    }

    #[test]
    fn a_note_across_a_table_is_a_paragraph_between_two_headed_parts() {
        let grid = Grid {
            rows: 4,
            cols: 2,
            cells: vec![
                cell(0, 0, 1, 1),
                cell(0, 1, 1, 1),
                cell(1, 0, 1, 1),
                cell(1, 1, 1, 1),
                cell(2, 0, 1, 2),
                cell(3, 0, 1, 1),
                cell(3, 1, 1, 1),
            ],
        };
        let mut chars = in_cells(&[
            (0, 0, "Zone", true),
            (0, 1, "Height", true),
            (1, 0, "R5", false),
            (1, 1, "45", false),
            (3, 0, "CR", false),
            (3, 1, "75", false),
        ]);
        let note = "Loading Space: required for the zones in accordance with the code.";
        chars.extend(word(note, 5.0, 100.0 - 2.0 * 20.0 - 6.0, false));
        let md = render_table(&grid, &chars, &[], None).markdown;
        assert_eq!(md.matches("| Zone | Height |").count(), 2, "{md}");
        assert!(md.contains("\n\nLoading Space:"), "{md}");
    }

    #[test]
    fn table_cells_escape_markdown_and_mark_bold_that_sets_part_apart() {
        let mut chars = word("Height:", 0.0, 100.0, true);
        chars.extend(word("45", 50.0, 100.0, false));
        assert_eq!(cell_text(&chars), "**Height:** 45");
        assert_eq!(cell_text(&word("R1**§", 0.0, 100.0, true)), "R1\\*\\*§");
    }

    #[test]
    fn a_corner_cell_over_several_rows_spans_the_header() {
        // San Diego's zone designator: "RS-" over "1-" over the zone numbers,
        // beside a corner cell naming the row labels.
        let grid = Grid {
            rows: 4,
            cols: 3,
            cells: vec![
                cell(0, 0, 3, 1),
                cell(0, 1, 1, 2),
                cell(1, 1, 1, 2),
                cell(2, 1, 1, 1),
                cell(2, 2, 1, 1),
                cell(3, 0, 1, 1),
                cell(3, 1, 1, 1),
                cell(3, 2, 1, 1),
            ],
        };
        let chars = in_cells(&[
            (0, 0, "Regulations", true),
            (0, 1, "RS-", false),
            (1, 1, "1-", false),
            (2, 1, "1", false),
            (2, 2, "2", false),
            (3, 0, "Density", false),
            (3, 1, "1", false),
            (3, 2, "1", false),
        ]);
        let md = render_table(&grid, &chars, &[], None).markdown;
        assert!(md.starts_with("| Regulations | RS-1-1 | RS-1-2 |"), "{md}");
        assert!(md.contains("| Density | 1 | 1 |"), "{md}");
    }

    #[test]
    fn a_row_label_spanning_the_label_columns_is_written_once() {
        let grid = Grid {
            rows: 2,
            cols: 3,
            cells: vec![
                cell(0, 0, 1, 1),
                cell(0, 1, 1, 1),
                cell(0, 2, 1, 1),
                cell(1, 0, 1, 2),
                cell(1, 2, 1, 1),
            ],
        };
        let chars = in_cells(&[
            (0, 0, "Use", true),
            (0, 1, "Zone", true),
            (0, 2, "RS", true),
            (1, 0, "Offices", false),
            (1, 2, "P", false),
        ]);
        let md = render_table(&grid, &chars, &[], None).markdown;
        assert!(md.contains("| Offices |  | P |"), "{md}");
    }

    #[test]
    fn a_raised_footnote_mark_follows_its_value() {
        let mut chars = word("25", 0.0, 100.0, false);
        let mark: Vec<RawChar> = word("(1)", 11.0, 103.0, false)
            .into_iter()
            .map(|c| RawChar {
                height: 5.0,
                font_size: 5.0,
                width: 3.0,
                ..c
            })
            .collect();
        chars.extend(mark);
        assert_eq!(cell_text(&chars), "25(1)");
        // A smaller word above another is a label of its own.
        let mut chars = word("FRONT", 0.0, 100.0, false);
        chars.extend(
            word("FENCE", 40.0, 103.0, false)
                .into_iter()
                .map(|c| RawChar {
                    height: 5.0,
                    font_size: 5.0,
                    ..c
                }),
        );
        assert_ne!(cell_text(&chars), "FRONTFENCE");
    }

    #[test]
    fn a_cell_set_on_its_side_reads_in_drawing_order() {
        let chars: Vec<RawChar> = "Urban General"
            .chars()
            .enumerate()
            .filter(|(_, ch)| *ch != ' ')
            .map(|(i, ch)| RawChar {
                ch,
                x: 10.0,
                y: 10.0 + i as f32 * 5.0,
                width: 8.0,
                height: 5.0,
                font_size: 8.0,
                font_name: "Helvetica".into(),
                bold: true,
                italic: false,
                space_before: ch == 'G',
                vertical: true,
            })
            .collect();
        assert_eq!(cell_text(&chars), "Urban General");
    }
}
