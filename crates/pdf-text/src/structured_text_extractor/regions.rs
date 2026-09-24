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
use super::{RawChar, group_chars_into_spans, group_spans_into_lines_owned, line_to_text};

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

    let mut regions: Vec<Region> = ruled_regions(&graphics.rules)
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
    regions.extend(chip_tables(&free_fills));
    regions
}

fn centre(c: &RawChar) -> (f32, f32) {
    (c.x + c.width / 2.0, c.y - c.height / 2.0)
}

fn is_near_white((r, g, b): (u8, u8, u8)) -> bool {
    r >= NEAR_WHITE && g >= NEAR_WHITE && b >= NEAR_WHITE
}

// ── Ruled tables ───────────────────────────────────────────────────

fn ruled_regions(rules: &[Rule]) -> Vec<Region> {
    let merged = merge_rules(rules);
    components(&merged)
        .into_iter()
        .flat_map(|component| grid_regions(&component))
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

/// Groups of rules connected by crossings: each is one table or one frame.
fn components(rules: &[Rule]) -> Vec<Vec<Rule>> {
    let mut parent: Vec<usize> = (0..rules.len()).collect();
    for i in 0..rules.len() {
        for j in (i + 1)..rules.len() {
            let (a, b) = (&rules[i], &rules[j]);
            let touching = match (a.horizontal, b.horizontal) {
                (true, false) => crosses(a, b),
                (false, true) => crosses(b, a),
                _ => false,
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
    let xs = cluster(vs.iter().map(|r| r.pos).collect(), GRID_SNAP);
    let mut ys = cluster(hs.iter().map(|r| r.pos).collect(), GRID_SNAP);
    ys.reverse();
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

    let mut bounds: Vec<(usize, usize, usize, usize, usize)> = Vec::new(); // root, r0, c0, r1, c1
    for r in 0..rows {
        for c in 0..cols {
            let root = find(&mut parent, id(r, c));
            match bounds.iter_mut().find(|b| b.0 == root) {
                Some(b) => {
                    b.1 = b.1.min(r);
                    b.2 = b.2.min(c);
                    b.3 = b.3.max(r);
                    b.4 = b.4.max(c);
                }
                None => bounds.push((root, r, c, r, c)),
            }
        }
    }
    let cells: Vec<Cell> = bounds
        .into_iter()
        .map(|(_, r0, c0, r1, c1)| Cell {
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
/// space, with markdown table delimiters escaped.
fn cell_text(chars: &[RawChar]) -> String {
    let spans = group_chars_into_spans(chars);
    let lines = group_spans_into_lines_owned(spans);
    let text: Vec<String> = lines
        .iter()
        .map(|line| line_to_text(line).trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();
    text.join(" ").replace('|', "\\|")
}

/// A table region as markdown: a caption paragraph for full-width title rows
/// at its top, then the grid.
pub(super) fn render_table(grid: &Grid, chars: &[RawChar], fills: &[Fill]) -> String {
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

    let texts: Vec<String> = by_cell.iter().map(|cs| cell_text(cs)).collect();
    let colours: Vec<Option<&'static str>> = grid
        .cells
        .iter()
        .map(|cell| cell_fill(fills, &cell.rect).and_then(|f| colour_name(f.rgb)))
        .collect();
    let mut distinct: Vec<&str> = colours
        .iter()
        .zip(&texts)
        .filter(|(_, t)| !t.is_empty())
        .filter_map(|(c, _)| *c)
        .collect();
    distinct.sort_unstable();
    distinct.dedup();
    let annotate = distinct.len() >= 2;

    let mut matrix = vec![vec![String::new(); grid.cols]; grid.rows];
    for ((cell, text), colour) in grid.cells.iter().zip(&texts).zip(&colours) {
        if text.is_empty() {
            continue;
        }
        let value = match colour {
            Some(name) if annotate => format!("{text} [{name}]"),
            _ => text.clone(),
        };
        // A value merged over a few columns holds for each of them (a check
        // mark across "Ground Story" and "Upper Story"); a row-wide cell is a
        // heading and is written once.
        let repeat = if cell.col_span < grid.cols {
            cell.col_span
        } else {
            1
        };
        for slot in matrix[cell.row].iter_mut().skip(cell.col).take(repeat) {
            slot.clone_from(&value);
        }
    }
    matrix.retain(|row| row.iter().any(|v| !v.is_empty()));

    let mut md = String::new();
    while matrix.len() > 1 && matrix[0][1..].iter().all(String::is_empty) {
        md.push_str(&matrix.remove(0)[0]);
        md.push_str("\n\n");
    }
    if matrix.is_empty() {
        return md.trim_end().to_string();
    }
    for (i, row) in matrix.iter().enumerate() {
        md.push_str("| ");
        md.push_str(&row.join(" | "));
        md.push_str(" |\n");
        if i == 0 {
            md.push('|');
            md.push_str(&" --- |".repeat(row.len()));
            md.push('\n');
        }
    }
    md.trim_end().to_string()
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
        let regions = ruled_regions(&rules);
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
        let regions = ruled_regions(&rules);
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
        let regions = ruled_regions(&rules);
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
        let regions = ruled_regions(&rules);
        assert!(matches!(regions[0].kind, Kind::TextBox));
    }

    #[test]
    fn a_lone_underline_is_no_region() {
        assert!(ruled_regions(&[h(300.0, 0.0, 120.0)]).is_empty());
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
}
