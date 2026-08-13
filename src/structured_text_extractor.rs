use crate::document::{open_document, page_count_u16, page_index};
use crate::error::{PdfCutterError, Result};
use crate::extractor::ExtractionWarning;
use crate::page_selection::PageSelection;
use crate::text_quality::is_probably_unreadable_text;
use pdfium_render::prelude::{
    PdfFontWeight, PdfPage, PdfPageRenderRotation, PdfPages, Pdfium, PdfiumError,
};
use serde::Serialize;
use std::num::NonZeroU16;
use std::path::Path;
use std::rc::Rc;

// ── Tolerance constants ────────────────────────────────────────────

/// Minimum vertical distance (in points) within which characters are on the
/// same line; scaled up proportionally for larger fonts (see [`same_line_y`]).
const LINE_Y_TOLERANCE: f32 = 2.0;

/// Fraction of the font size used as the vertical same-line tolerance.
const LINE_Y_TOLERANCE_FACTOR: f32 = 0.25;

/// Fraction of the font size a horizontal gap must exceed to count as a word
/// space between adjacent spans (typical space glyphs are ~0.25em wide).
const SPAN_SPACE_GAP_FACTOR: f32 = 0.15;

/// Characters with font properties within these tolerances are merged into one span.
const FONT_SIZE_TOLERANCE: f32 = 0.3;

/// Horizontal gap (in points) beyond which we start a new span on the same line.
const SPAN_GAP_TOLERANCE_FACTOR: f32 = 1.8;

/// Minimum number of aligned columns to consider a region a table.
const MIN_TABLE_COLUMNS: usize = 2;

/// Minimum number of rows sharing column alignment to be considered a table.
const MIN_TABLE_ROWS: usize = 2;

/// Column anchor tolerance (points). Wider than line tolerance to bridge
/// drift between left-aligned headers and right-aligned numbers.
const COLUMN_ANCHOR_TOLERANCE: f32 = 30.0;

/// Font size ratio relative to body text above which a line is a heading.
const H1_SIZE_RATIO: f32 = 1.7;
const H2_SIZE_RATIO: f32 = 1.25;

/// Vertical-gap multiplier (× line height) above which a paragraph break starts.
const PARAGRAPH_GAP_FACTOR: f32 = 1.6;

/// Minimum width (in points) of a vertical whitespace run to qualify as a
/// column gutter.
const COLUMN_GUTTER_MIN_WIDTH: f32 = 18.0;

/// Minimum number of lines each side of a gutter needs to count as a column.
const MIN_COLUMN_LINES: usize = 4;

/// Fraction of lines allowed to cross a candidate gutter (e.g. a title that
/// spans both columns) before the gap is rejected.
const MAX_GUTTER_CROSSING_FRACTION: f32 = 0.15;

/// Width of the histogram bins used to find low-coverage vertical gaps.
const GUTTER_BIN_WIDTH: f32 = 4.0;

/// Vertical-gap multiplier (× average line height) above which lines are
/// split into separate bands before column detection, and its floor.
const BAND_SPLIT_GAP_FACTOR: f32 = 2.5;
const BAND_SPLIT_MIN_GAP: f32 = 24.0;

/// Maximum recursion depth for column splitting (2 → up to 4 columns).
const MAX_COLUMN_SPLIT_DEPTH: usize = 2;

// ── Public types ───────────────────────────────────────────────────

pub struct StructuredExtractionReport {
    pub pages: Vec<StructuredPage>,
    pub selected_page_count: usize,
    pub warnings: Vec<ExtractionWarning>,
}

#[derive(Serialize)]
pub struct StructuredPage {
    pub page: u16,
    pub spans: Vec<TextSpan>,
    pub markdown: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct TextSpan {
    pub text: String,
    pub font_size: f32,
    pub font_name: String,
    pub bold: bool,
    pub italic: bool,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum StructuredOutputFormat {
    Full,
    MarkdownOnly,
}

// ── Internal intermediate types ────────────────────────────────────

struct RawChar {
    ch: char,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    font_size: f32,
    font_name: Rc<str>,
    bold: bool,
    italic: bool,
    space_before: bool,
}

struct TextLine {
    y: f32,
    spans: Vec<TextSpan>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum LineKind {
    Heading(u8),
    Paragraph,
    ListItem,
    Empty,
}

struct DetectedTable {
    /// First line of the table. Derived: equals `rows[0][0]`.
    start_line: usize,
    /// One past the table's last line. Derived: equals the last row's last
    /// line index + 1.
    end_line: usize,
    columns: Vec<f32>,
    /// Logical rows as groups of line indices: wrapped cell text lines are
    /// grouped with the row line they continue.
    rows: Vec<Vec<usize>>,
}

enum Block {
    Heading {
        level: u8,
        line_indices: Vec<usize>,
    },
    Paragraph {
        line_indices: Vec<usize>,
    },
    List {
        items: Vec<Vec<usize>>, // each item is one or more line indices (continuations)
    },
    Table {
        index: usize,
    },
}

// ── Entry point ────────────────────────────────────────────────────

pub fn extract_structured_text(
    pdfium: &Pdfium,
    pdf_path: &Path,
    selection: PageSelection,
    output_format: StructuredOutputFormat,
) -> Result<StructuredExtractionReport> {
    let document = open_document(pdfium, pdf_path)?;
    let pages = document.pages();
    let total_pages = page_count_u16(pages)?;

    if total_pages == 0 {
        return Err(PdfCutterError::EmptyDocument);
    }

    let resolved_selection = selection.resolve(total_pages);
    let mut warnings: Vec<ExtractionWarning> = resolved_selection
        .out_of_bounds
        .into_iter()
        .map(|page_number| ExtractionWarning::PageOutOfBounds {
            page_number: page_number.get(),
            total_pages,
        })
        .collect();

    let page_numbers = resolved_selection.page_numbers;
    let selected_page_count = page_numbers.len();

    let mut structured_pages = Vec::with_capacity(selected_page_count);
    for &page_number in &page_numbers {
        match extract_page_structured(pages, page_number, output_format) {
            Ok(page) => structured_pages.push(page),
            Err(warning) => warnings.push(warning),
        }
    }

    Ok(StructuredExtractionReport {
        pages: structured_pages,
        selected_page_count,
        warnings,
    })
}

// ── Per-page extraction ────────────────────────────────────────────

fn extract_page_structured(
    pages: &PdfPages<'_>,
    page_number: NonZeroU16,
    format: StructuredOutputFormat,
) -> std::result::Result<StructuredPage, ExtractionWarning> {
    let page = pages
        .get(page_index(page_number))
        .map_err(|error| ExtractionWarning::LoadPage {
            page_number: page_number.get(),
            reason: error.to_string(),
        })?;

    let raw_chars = extract_raw_chars(&page).map_err(|error| ExtractionWarning::LoadPage {
        page_number: page_number.get(),
        reason: error.to_string(),
    })?;

    if is_probably_unreadable_text(&raw_chars_to_text(&raw_chars)) {
        return Err(ExtractionWarning::UnreadableText {
            page_number: page_number.get(),
        });
    }

    let spans = group_chars_into_spans(&raw_chars);
    let markdown = spans_to_markdown(&spans);

    let output_spans = match format {
        StructuredOutputFormat::Full => spans,
        StructuredOutputFormat::MarkdownOnly => Vec::new(),
    };

    Ok(StructuredPage {
        page: page_number.get(),
        spans: output_spans,
        markdown,
    })
}

// ── Step 1: Extract raw characters ─────────────────────────────────

fn extract_raw_chars(page: &PdfPage<'_>) -> std::result::Result<Vec<RawChar>, PdfiumError> {
    let text = page.text()?;
    let rotation = page.rotation().unwrap_or(PdfPageRenderRotation::None);
    let page_width = page.width().value;
    let page_height = page.height().value;
    let mut chars = Vec::new();
    let mut pending_space = false;
    let mut last_font: Option<(Rc<str>, bool, bool)> = None;

    for char_ref in text.chars().iter() {
        let ch = match char_ref.unicode_char() {
            Some(c) => c,
            None => continue,
        };

        if ch.is_control() && !matches!(ch, '\n' | '\r' | '\t') {
            continue;
        }

        if ch.is_whitespace() {
            pending_space = true;
            continue;
        }

        let bounds = match char_ref.loose_bounds() {
            Ok(rect) => rect,
            Err(_) => continue,
        };

        // Rotated text matrices can make the scaled size zero or negative;
        // fall back to the unscaled /Tf size before giving up on the char.
        let mut font_size = char_ref.scaled_font_size().value;
        if font_size <= 0.0 {
            font_size = char_ref.unscaled_font_size().value;
        }
        if font_size <= 0.0 {
            continue;
        }

        // Consecutive characters usually share a font; weight, flags, and the
        // name-derived style are font properties, so they are computed once
        // per font run and reused. Only the bold-reenforced check remains per
        // character (it reflects the text render mode, not the font).
        let raw_font_name = char_ref.font_name();
        let (font_name, font_bold, font_italic) = match &last_font {
            Some((name, font_bold, font_italic)) if **name == raw_font_name => {
                (Rc::clone(name), *font_bold, *font_italic)
            }
            _ => {
                let name: Rc<str> = raw_font_name.into();
                let (name_bold, name_italic) = font_name_style(&name);
                let font_bold = name_bold
                    || char_ref
                        .font_weight()
                        .is_some_and(|w| font_weight_numeric(&w) >= 600);
                let font_italic = name_italic || char_ref.font_is_italic();
                last_font = Some((Rc::clone(&name), font_bold, font_italic));
                (name, font_bold, font_italic)
            }
        };

        let bold = font_bold || char_ref.font_is_bold_reenforced();

        let left = bounds.left().value;
        let right = bounds.right().value;
        let top = bounds.top().value;
        let bottom = bounds.bottom().value;
        let (x, y, width, height) = rotate_to_visual(
            left,
            top,
            (right - left).abs(),
            (top - bottom).abs(),
            rotation,
            page_width,
            page_height,
        );
        chars.push(RawChar {
            ch,
            x,
            y,
            width,
            height,
            font_size,
            font_name,
            bold,
            italic: font_italic,
            space_before: pending_space,
        });
        pending_space = false;
    }

    Ok(chars)
}

/// Maps a glyph box from unrotated page space into visual (display) space
/// for pages carrying a `/Rotate` entry, so line and column detection see
/// the text the way the reader does. Input and output are `(left, top,
/// width, height)` in y-up coordinates.
fn rotate_to_visual(
    left: f32,
    top: f32,
    width: f32,
    height: f32,
    rotation: PdfPageRenderRotation,
    page_width: f32,
    page_height: f32,
) -> (f32, f32, f32, f32) {
    match rotation {
        PdfPageRenderRotation::None => (left, top, width, height),
        // 90° clockwise: visual point = (y, page_width - x).
        PdfPageRenderRotation::Degrees90 => (top - height, page_width - left, height, width),
        // 180°: visual point = (page_width - x, page_height - y).
        PdfPageRenderRotation::Degrees180 => (
            page_width - left - width,
            page_height - top + height,
            width,
            height,
        ),
        // 270° clockwise: visual point = (page_height - y, x).
        PdfPageRenderRotation::Degrees270 => (page_height - top, left + width, height, width),
    }
}

/// Rebuilds plain text (with word spaces) from raw characters so the shared
/// garbled-text heuristic can judge the page.
fn raw_chars_to_text(chars: &[RawChar]) -> String {
    let mut text = String::with_capacity(chars.len() * 2);
    for raw in chars {
        if raw.space_before && !text.is_empty() {
            text.push(' ');
        }
        text.push(raw.ch);
    }
    text
}

/// Fallback style detection from the font name, for fonts that carry no
/// usable weight metadata (e.g. pdfium reports weight 0 for the standard
/// Helvetica-Bold). Returns `(bold, italic)`.
fn font_name_style(name: &str) -> (bool, bool) {
    let name = name.to_ascii_lowercase();
    (
        name.contains("bold") || name.contains("black") || name.contains("heavy"),
        name.contains("italic") || name.contains("oblique"),
    )
}

fn font_weight_numeric(weight: &PdfFontWeight) -> u32 {
    match weight {
        PdfFontWeight::Weight100 => 100,
        PdfFontWeight::Weight200 => 200,
        PdfFontWeight::Weight300 => 300,
        PdfFontWeight::Weight400Normal => 400,
        PdfFontWeight::Weight500 => 500,
        PdfFontWeight::Weight600 => 600,
        PdfFontWeight::Weight700Bold => 700,
        PdfFontWeight::Weight800 => 800,
        PdfFontWeight::Weight900 => 900,
        PdfFontWeight::Custom(n) => *n,
    }
}

// ── Step 2: Group characters into spans ────────────────────────────

fn group_chars_into_spans(chars: &[RawChar]) -> Vec<TextSpan> {
    if chars.is_empty() {
        return Vec::new();
    }

    let mut spans: Vec<TextSpan> = Vec::new();

    for raw in chars {
        let should_merge = spans.last().is_some_and(|prev| {
            same_line_y(prev.y, raw.y, prev.font_size)
                && same_font(prev, raw)
                && horizontal_gap_ok(prev, raw)
        });

        if should_merge {
            let prev = spans.last_mut().unwrap();
            if raw.space_before {
                prev.text.push(' ');
            }
            prev.text.push(raw.ch);
            let new_right = raw.x + raw.width;
            prev.width = new_right - prev.x;
            if raw.height > prev.height {
                prev.height = raw.height;
            }
        } else {
            spans.push(TextSpan {
                text: raw.ch.to_string(),
                font_size: raw.font_size,
                font_name: raw.font_name.to_string(),
                bold: raw.bold,
                italic: raw.italic,
                x: raw.x,
                y: raw.y,
                width: raw.width,
                height: raw.height,
            });
        }
    }

    spans
}

/// Same-line check with a tolerance proportional to the font size, so large
/// headings with baseline jitter stay on one line while small text does not
/// absorb its neighbors.
fn same_line_y(y1: f32, y2: f32, font_size: f32) -> bool {
    let tolerance = (font_size * LINE_Y_TOLERANCE_FACTOR).max(LINE_Y_TOLERANCE);
    (y1 - y2).abs() < tolerance
}

fn same_font(span: &TextSpan, raw: &RawChar) -> bool {
    (span.font_size - raw.font_size).abs() < FONT_SIZE_TOLERANCE
        && span.bold == raw.bold
        && span.italic == raw.italic
}

fn horizontal_gap_ok(span: &TextSpan, raw: &RawChar) -> bool {
    let gap = raw.x - (span.x + span.width);
    gap < span.font_size * SPAN_GAP_TOLERANCE_FACTOR && gap > -span.font_size * 0.5
}

// ── Step 3: Group spans into lines ─────────────────────────────────

/// Clusters spans into lines by vertical position and orders the result
/// visually: lines top-to-bottom, spans within a line left-to-right.
///
/// Spans are matched against every existing line (not only the previous one)
/// because PDF content streams frequently emit text out of visual order
/// (e.g. footers first, or two-column layouts interleaved).
fn group_spans_into_lines(spans: &[TextSpan]) -> Vec<TextLine> {
    group_spans_into_lines_owned(spans.to_vec())
}

fn group_spans_into_lines_owned(mut spans: Vec<TextSpan>) -> Vec<TextLine> {
    if spans.is_empty() {
        return Vec::new();
    }

    // PDF y grows upward, so descending y is top-to-bottom reading order.
    // Sorting first lets adjacent spans cluster into lines in a single pass.
    spans.sort_by(|a, b| b.y.partial_cmp(&a.y).unwrap_or(std::cmp::Ordering::Equal));

    let mut lines: Vec<TextLine> = Vec::new();

    for span in spans {
        match lines.last_mut() {
            Some(line) if same_line_y(line.y, span.y, span.font_size) => {
                line.spans.push(span);
            }
            _ => lines.push(TextLine {
                y: span.y,
                spans: vec![span],
            }),
        }
    }

    for line in &mut lines {
        line.spans
            .sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal));
    }

    lines
}

/// True when the horizontal gap between two adjacent spans on one line is
/// wide enough to represent a word space (kerning gaps are far narrower).
fn needs_space_between(previous: &TextSpan, next: &TextSpan) -> bool {
    let gap = next.x - (previous.x + previous.width);
    gap > previous.font_size * SPAN_SPACE_GAP_FACTOR
}

/// Column-assignment rule shared by the column split and its plausibility
/// check: a span belongs to the left of a gutter when its center is left
/// of it.
fn span_is_left_of(span: &TextSpan, gutter_x: f32) -> bool {
    span.x + span.width * 0.5 < gutter_x
}

// ── Step 3b: Bands and columns ─────────────────────────────────────

/// Renders spans to markdown in visual reading order: the page is split into
/// vertical bands at large line gaps, each band is split into columns where
/// a whitespace gutter separates independent line flows, and blocks are then
/// rendered band by band, column by column.
fn spans_to_markdown(spans: &[TextSpan]) -> String {
    let lines = group_spans_into_lines(spans);
    // Body size must be judged page-wide: a heading isolated in its own band
    // or column has no body text to compare against.
    let body_size = estimate_body_font_size(&lines);
    let mut md = String::new();

    for band in split_into_bands(lines) {
        for column in split_band_into_columns(band, 0) {
            let rendered = lines_to_markdown(&column, body_size);
            if !rendered.is_empty() {
                md.push_str(&rendered);
                md.push_str("\n\n");
            }
        }
    }

    md.trim_end().to_string()
}

/// Splits top-to-bottom ordered lines into bands at unusually large vertical
/// gaps, so a full-width title does not mask the column gutter beneath it.
fn split_into_bands(lines: Vec<TextLine>) -> Vec<Vec<TextLine>> {
    if lines.is_empty() {
        return Vec::new();
    }

    let mean_line_height = {
        let heights: f32 = lines.iter().map(average_line_height).sum();
        heights / lines.len() as f32
    };
    let gap_threshold = (mean_line_height * BAND_SPLIT_GAP_FACTOR).max(BAND_SPLIT_MIN_GAP);

    let mut bands: Vec<Vec<TextLine>> = Vec::new();
    let mut current: Vec<TextLine> = Vec::new();

    for line in lines {
        if let Some(previous) = current.last()
            && (previous.y - line.y) > gap_threshold
        {
            bands.push(std::mem::take(&mut current));
        }
        current.push(line);
    }
    bands.push(current);

    bands
}

/// Recursively splits a band into columns along detected gutters. Lines are
/// re-clustered per column so text on the same height in different columns
/// no longer shares a line.
fn split_band_into_columns(lines: Vec<TextLine>, depth: usize) -> Vec<Vec<TextLine>> {
    if depth >= MAX_COLUMN_SPLIT_DEPTH {
        return vec![lines];
    }

    let Some(gutter_x) = find_column_gutter(&lines) else {
        return vec![lines];
    };

    let spans: Vec<TextSpan> = lines.into_iter().flat_map(|line| line.spans).collect();
    let (left, right): (Vec<TextSpan>, Vec<TextSpan>) = spans
        .into_iter()
        .partition(|span| span_is_left_of(span, gutter_x));

    let mut columns = split_band_into_columns(group_spans_into_lines_owned(left), depth + 1);
    columns.extend(split_band_into_columns(
        group_spans_into_lines_owned(right),
        depth + 1,
    ));
    columns
}

/// Looks for a vertical whitespace gutter separating two independent column
/// flows. Returns the gutter's center x, or `None` when no plausible gutter
/// exists (including when the gap is a table column divider, recognizable by
/// rows aligned across it).
fn find_column_gutter(lines: &[TextLine]) -> Option<f32> {
    if lines.len() < MIN_COLUMN_LINES {
        return None;
    }

    let (min_x, max_x) = lines
        .iter()
        .flat_map(|line| &line.spans)
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(low, high), span| {
            (low.min(span.x), high.max(span.x + span.width))
        });
    let extent = max_x - min_x;
    if !extent.is_finite() || extent <= COLUMN_GUTTER_MIN_WIDTH * 4.0 {
        return None;
    }

    // Coverage histogram over x: how many spans touch each bin.
    let bin_count = ((extent / GUTTER_BIN_WIDTH).ceil() as usize).clamp(1, 2048);
    let bin_width = extent / bin_count as f32;
    let mut coverage = vec![0u32; bin_count];
    for line in lines {
        for span in &line.spans {
            let first = (((span.x - min_x) / bin_width) as usize).min(bin_count - 1);
            let last = (((span.x + span.width - min_x) / bin_width) as usize).min(bin_count - 1);
            for bin in &mut coverage[first..=last] {
                *bin += 1;
            }
        }
    }

    let allowed_crossings = (lines.len() as f32 * MAX_GUTTER_CROSSING_FRACTION) as u32;
    let min_gutter_bins = (COLUMN_GUTTER_MIN_WIDTH / bin_width).ceil() as usize;

    let candidates: Vec<(usize, f32)> = low_coverage_runs(&coverage, allowed_crossings)
        .into_iter()
        .filter(|&(start, end)| end - start >= min_gutter_bins)
        .map(|(start, end)| (end - start, min_x + (start + end) as f32 / 2.0 * bin_width))
        .filter(|&(_, center)| {
            let offset = center - min_x;
            offset > extent * 0.2 && offset < extent * 0.8
        })
        .collect();
    if candidates.is_empty() {
        return None;
    }

    // Table detection is the expensive pass; run it only for the rare bands
    // that actually have a plausible-looking gap.
    let tables = detect_tables(lines);

    candidates
        .into_iter()
        .filter(|&(_, center)| !gutter_is_inside_table(&tables, center))
        .filter(|&(_, center)| column_split_is_plausible(lines, center))
        .max_by_key(|&(run_len, _)| run_len)
        .map(|(_, center)| center)
}

/// Maximal runs of bins whose coverage does not exceed `allowed_crossings`,
/// as half-open `(start, end)` bin ranges.
fn low_coverage_runs(coverage: &[u32], allowed_crossings: u32) -> Vec<(usize, usize)> {
    let mut runs = Vec::new();
    let mut run_start: Option<usize> = None;

    for (bin, &count) in coverage.iter().enumerate() {
        if count <= allowed_crossings {
            run_start.get_or_insert(bin);
        } else if let Some(start) = run_start.take() {
            runs.push((start, bin));
        }
    }
    if let Some(start) = run_start {
        runs.push((start, coverage.len()));
    }

    runs
}

/// A whitespace run between the column anchors of a detected table is the
/// table's own column divider, not a page-column gutter.
fn gutter_is_inside_table(tables: &[DetectedTable], x: f32) -> bool {
    tables.iter().any(|table| {
        table
            .columns
            .first()
            .zip(table.columns.last())
            .is_some_and(|(&first, &last)| x > first && x < last)
    })
}

/// A gutter divides true columns only when each side is an independent flow:
/// both sides need several lines of their own, and lines spanning both sides
/// (table rows, or wrapped cells sharing a row start) must stay a minority
/// relative to either side.
fn column_split_is_plausible(lines: &[TextLine], gutter_x: f32) -> bool {
    let mut left_only = 0usize;
    let mut right_only = 0usize;
    let mut both = 0usize;

    for line in lines {
        let mut has_left = false;
        let mut has_right = false;
        for span in &line.spans {
            if span_is_left_of(span, gutter_x) {
                has_left = true;
            } else {
                has_right = true;
            }
        }
        match (has_left, has_right) {
            (true, true) => both += 1,
            (true, false) => left_only += 1,
            (false, true) => right_only += 1,
            (false, false) => {}
        }
    }

    left_only >= MIN_COLUMN_LINES
        && right_only >= MIN_COLUMN_LINES
        && both <= left_only.min(right_only)
}

// ── Step 4: Detect tables ──────────────────────────────────────────

fn detect_tables(lines: &[TextLine]) -> Vec<DetectedTable> {
    let mut tables = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        match detect_table_at(lines, i) {
            Some(table) => {
                i = table.end_line;
                tables.push(table);
            }
            None => i += 1,
        }
    }

    tables
}

/// Tries to grow a table starting at `start`. A table is a run of "row"
/// lines (several spans, mostly aligned to the accumulated column anchors)
/// interleaved with "continuation" lines — wrapped cell text whose spans
/// each stay inside a single column's territory. Continuations attach to the
/// logical row above them.
fn detect_table_at(lines: &[TextLine], start: usize) -> Option<DetectedTable> {
    if lines[start].spans.len() < MIN_TABLE_COLUMNS {
        return None;
    }

    let line_xs = |line: &TextLine| -> Vec<f32> { line.spans.iter().map(|s| s.x).collect() };

    // Column anchors are the union of x-positions across rows that align,
    // kept sorted so the continuation check can search them directly.
    let first_xs = line_xs(&lines[start]);
    let mut anchors = first_xs.clone();
    anchors.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut row_xs: Vec<Vec<f32>> = vec![first_xs];
    let mut end = start + 1;

    while end < lines.len() {
        let line = &lines[end];

        if line.spans.len() >= MIN_TABLE_COLUMNS {
            let candidate = line_xs(line);
            let aligned = count_aligned(&anchors, &candidate);

            // A candidate is a row if a majority of its columns hit anchors.
            if aligned >= MIN_TABLE_COLUMNS && aligned * 2 >= candidate.len() {
                for &cx in &candidate {
                    if !aligned_to_any(&anchors, cx) {
                        let position = anchors.partition_point(|&a| a < cx);
                        anchors.insert(position, cx);
                    }
                }
                row_xs.push(candidate);
                end += 1;
                continue;
            }
        }

        if is_continuation_line(line, &anchors) {
            end += 1;
        } else {
            break;
        }
    }

    if row_xs.len() < MIN_TABLE_ROWS {
        return None;
    }

    // Keep only anchors that have hits in at least half the rows (drops noise).
    let final_anchors = filter_anchors(&anchors, &row_xs);
    if final_anchors.len() < MIN_TABLE_COLUMNS {
        return None;
    }

    let rows = cluster_lines_into_rows(lines, start, end, &final_anchors);

    Some(DetectedTable {
        start_line: start,
        end_line: end,
        columns: final_anchors,
        rows,
    })
}

/// True when every span of the line sits inside a single column's territory:
/// aligned to an anchor and not reaching into the next column. Full-width
/// paragraph text following a table fails this and ends the table.
/// `sorted_anchors` must be in ascending order.
fn is_continuation_line(line: &TextLine, sorted_anchors: &[f32]) -> bool {
    !line.spans.is_empty()
        && line
            .spans
            .iter()
            .all(|span| span_stays_in_column(span, sorted_anchors))
}

fn span_stays_in_column(span: &TextSpan, sorted_anchors: &[f32]) -> bool {
    let column = nearest_column(span.x, sorted_anchors);
    if !is_aligned(sorted_anchors[column], span.x) {
        return false;
    }

    match sorted_anchors.get(column + 1) {
        Some(&next_anchor) => span.x + span.width < next_anchor,
        None => true,
    }
}

/// The single alignment rule shared by table growth, anchor filtering, row
/// clustering, and the continuation check.
fn is_aligned(anchor: f32, x: f32) -> bool {
    (anchor - x).abs() < COLUMN_ANCHOR_TOLERANCE
}

fn aligned_to_any(anchors: &[f32], x: f32) -> bool {
    anchors.iter().any(|&anchor| is_aligned(anchor, x))
}

/// Groups the table's lines into logical rows: a line with content in the
/// first column starts a new row, every other line continues the row above
/// (wrapped cell text).
fn cluster_lines_into_rows(
    lines: &[TextLine],
    start: usize,
    end: usize,
    columns: &[f32],
) -> Vec<Vec<usize>> {
    let first_anchor = columns[0];
    let mut rows: Vec<Vec<usize>> = Vec::new();

    for (offset, line) in lines[start..end].iter().enumerate() {
        let index = start + offset;
        // A span opens a new row only when the first column is its nearest
        // anchor — plain distance would also match spans of a narrowly
        // indented second column.
        let starts_row = line
            .spans
            .iter()
            .any(|span| nearest_column(span.x, columns) == 0 && is_aligned(first_anchor, span.x));

        match rows.last_mut() {
            Some(row) if !starts_row => row.push(index),
            _ => rows.push(vec![index]),
        }
    }

    rows
}

fn count_aligned(anchors: &[f32], candidate: &[f32]) -> usize {
    candidate
        .iter()
        .filter(|&&cx| aligned_to_any(anchors, cx))
        .count()
}

fn filter_anchors(anchors: &[f32], rows: &[Vec<f32>]) -> Vec<f32> {
    let threshold = (rows.len() / 2).max(1);
    anchors
        .iter()
        .filter(|&&anchor| {
            rows.iter()
                .filter(|row| row.iter().any(|&cx| is_aligned(anchor, cx)))
                .count()
                >= threshold
        })
        .copied()
        .collect()
}

// ── Step 5: Classify lines ─────────────────────────────────────────

fn classify_line(line: &TextLine, body_size: f32) -> LineKind {
    if line.spans.is_empty() {
        return LineKind::Empty;
    }

    let line_text = line_to_text(line);
    let trimmed = line_text.trim();
    if trimmed.is_empty() {
        return LineKind::Empty;
    }

    if is_list_item_start(trimmed) {
        return LineKind::ListItem;
    }

    let max_size = line
        .spans
        .iter()
        .map(|s| s.font_size)
        .fold(0.0_f32, f32::max);

    if max_size > body_size * H1_SIZE_RATIO {
        return LineKind::Heading(1);
    }
    if max_size > body_size * H2_SIZE_RATIO {
        return LineKind::Heading(2);
    }

    LineKind::Paragraph
}

fn is_list_item_start(text: &str) -> bool {
    let mut chars = text.chars();
    let first = match chars.next() {
        Some(c) => c,
        None => return false,
    };
    matches!(first, '•' | '●' | '◦' | '▪' | '■' | '–' | '—' | '·')
        || (first == '-' && chars.next() == Some(' '))
}

// ── Step 6: Group lines into blocks ────────────────────────────────

fn group_into_blocks(lines: &[TextLine], tables: &[DetectedTable], body_size: f32) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        if let Some((idx, table)) = tables
            .iter()
            .enumerate()
            .find(|(_, t)| i >= t.start_line && i < t.end_line)
        {
            blocks.push(Block::Table { index: idx });
            i = table.end_line;
            continue;
        }

        let kind = classify_line(&lines[i], body_size);
        if matches!(kind, LineKind::Empty) {
            i += 1;
            continue;
        }

        // Lists need special handling: collect items + continuation lines.
        if matches!(kind, LineKind::ListItem) {
            let mut items: Vec<Vec<usize>> = vec![vec![i]];
            let mut j = i + 1;
            while j < lines.len() {
                if tables.iter().any(|t| j >= t.start_line && j < t.end_line) {
                    break;
                }
                if !is_vertically_close(&lines[j - 1], &lines[j]) {
                    break;
                }
                let next_kind = classify_line(&lines[j], body_size);
                match next_kind {
                    LineKind::ListItem => {
                        items.push(vec![j]);
                        j += 1;
                    }
                    LineKind::Paragraph => {
                        // Continuation of the current item.
                        items.last_mut().unwrap().push(j);
                        j += 1;
                    }
                    _ => break,
                }
            }
            blocks.push(Block::List { items });
            i = j;
            continue;
        }

        let mut indices = vec![i];
        let mut j = i + 1;

        while j < lines.len() {
            if tables.iter().any(|t| j >= t.start_line && j < t.end_line) {
                break;
            }

            let next_kind = classify_line(&lines[j], body_size);
            if !same_block_kind(&kind, &next_kind) {
                break;
            }

            if !is_vertically_close(&lines[j - 1], &lines[j]) {
                break;
            }

            if matches!(kind, LineKind::Paragraph) && !same_text_style(&lines[j - 1], &lines[j]) {
                break;
            }

            indices.push(j);
            j += 1;
        }

        let block = match &kind {
            LineKind::Heading(level) => Block::Heading {
                level: *level,
                line_indices: indices,
            },
            LineKind::Paragraph => Block::Paragraph {
                line_indices: indices,
            },
            LineKind::ListItem => unreachable!("handled above"),
            LineKind::Empty => unreachable!(),
        };
        blocks.push(block);

        i = j;
    }

    blocks
}

fn same_block_kind(a: &LineKind, b: &LineKind) -> bool {
    matches!(
        (a, b),
        (LineKind::Heading(la), LineKind::Heading(lb)) if la == lb
    ) || matches!(
        (a, b),
        (LineKind::Paragraph, LineKind::Paragraph) | (LineKind::ListItem, LineKind::ListItem)
    )
}

fn is_vertically_close(prev: &TextLine, curr: &TextLine) -> bool {
    let gap = (prev.y - curr.y).abs();
    let line_height = average_line_height(prev).max(average_line_height(curr));
    if line_height <= 0.0 {
        return gap < 20.0;
    }
    gap < line_height * PARAGRAPH_GAP_FACTOR
}

fn average_line_height(line: &TextLine) -> f32 {
    if line.spans.is_empty() {
        return 0.0;
    }
    let sum: f32 = line.spans.iter().map(|s| s.height).sum();
    sum / line.spans.len() as f32
}

fn same_text_style(a: &TextLine, b: &TextLine) -> bool {
    let style_a = dominant_style(a);
    let style_b = dominant_style(b);
    (style_a.0 - style_b.0).abs() < 1.0 && style_a.1 == style_b.1 && style_a.2 == style_b.2
}

/// Returns (font_size, bold, italic) for the dominant span on a line.
fn dominant_style(line: &TextLine) -> (f32, bool, bool) {
    line.spans
        .iter()
        .max_by(|a, b| a.text.len().cmp(&b.text.len()))
        .map(|s| (s.font_size, s.bold, s.italic))
        .unwrap_or((0.0, false, false))
}

// ── Step 7: Render markdown from blocks ────────────────────────────

fn lines_to_markdown(lines: &[TextLine], body_size: f32) -> String {
    if lines.is_empty() {
        return String::new();
    }

    let tables = detect_tables(lines);
    let blocks = group_into_blocks(lines, &tables, body_size);

    let mut md = String::new();
    for block in &blocks {
        render_block(&mut md, block, lines, &tables);
    }

    while md.contains("\n\n\n") {
        md = md.replace("\n\n\n", "\n\n");
    }
    md.trim().to_string()
}

fn render_block(md: &mut String, block: &Block, lines: &[TextLine], tables: &[DetectedTable]) {
    match block {
        Block::Heading {
            level,
            line_indices,
        } => {
            md.push_str(&"#".repeat(*level as usize));
            md.push(' ');
            for (i, &idx) in line_indices.iter().enumerate() {
                if i > 0 {
                    md.push(' ');
                }
                escape_markdown_into(md, line_to_text(&lines[idx]).trim());
            }
            md.push_str("\n\n");
        }
        Block::Paragraph { line_indices } => {
            render_spans_with_inline_formatting(md, &paragraph_spans(lines, line_indices));
            md.push_str("\n\n");
        }
        Block::List { items } => {
            for item_lines in items {
                let mut text = String::new();
                for (i, &idx) in item_lines.iter().enumerate() {
                    if i > 0 {
                        text.push(' ');
                    }
                    text.push_str(&line_to_text(&lines[idx]));
                }
                let cleaned = strip_list_marker(text.trim());
                md.push_str("- ");
                escape_markdown_into(md, &cleaned);
                md.push('\n');
            }
            md.push('\n');
        }
        Block::Table { index } => {
            render_table_markdown(md, lines, &tables[*index]);
        }
    }
}

fn strip_list_marker(text: &str) -> String {
    let mut chars = text.chars();
    let first = chars.next();
    match first {
        Some('•') | Some('●') | Some('◦') | Some('▪') | Some('■') | Some('–') | Some('—')
        | Some('·') => chars.as_str().trim_start().to_string(),
        Some('-') => {
            let rest = chars.as_str();
            if rest.starts_with(' ') {
                rest.trim_start().to_string()
            } else {
                text.to_string()
            }
        }
        _ => text.to_string(),
    }
}

fn estimate_body_font_size(lines: &[TextLine]) -> f32 {
    let mut size_counts: Vec<(f32, usize)> = Vec::new();

    for line in lines {
        for span in &line.spans {
            let rounded = (span.font_size * 2.0).round() / 2.0;
            if let Some(entry) = size_counts
                .iter_mut()
                .find(|(s, _)| (*s - rounded).abs() < 0.1)
            {
                entry.1 += span.text.len();
            } else {
                size_counts.push((rounded, span.text.len()));
            }
        }
    }

    size_counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(size, _)| size)
        .unwrap_or(12.0)
}

fn line_to_text(line: &TextLine) -> String {
    let mut text = String::new();
    for (i, span) in line.spans.iter().enumerate() {
        if i > 0 && needs_space_between(&line.spans[i - 1], span) {
            text.push(' ');
        }
        text.push_str(&span.text);
    }
    text
}

/// A span prepared for inline rendering:
/// `(span, needs_leading_space, strip_trailing_hyphen)`.
type InlineSpan<'a> = (&'a TextSpan, bool, bool);

/// Flattens paragraph lines into render-ready spans: word spaces are decided
/// from horizontal gaps, line breaks become spaces, and a hyphen at a line
/// break followed by a lowercase continuation is joined into one word.
fn paragraph_spans<'a>(lines: &'a [TextLine], line_indices: &[usize]) -> Vec<InlineSpan<'a>> {
    let mut spans: Vec<InlineSpan<'a>> = Vec::new();

    for (line_pos, &idx) in line_indices.iter().enumerate() {
        let line = &lines[idx];
        for (span_pos, span) in line.spans.iter().enumerate() {
            let mut leading_space = if span_pos > 0 {
                needs_space_between(&line.spans[span_pos - 1], span)
            } else {
                line_pos > 0
            };

            if span_pos == 0
                && let Some((previous, _, strip_trailing_hyphen)) = spans.last_mut()
                && ends_with_soft_hyphen(&previous.text)
                && starts_lowercase(&span.text)
            {
                *strip_trailing_hyphen = true;
                leading_space = false;
            }

            spans.push((span, leading_space, false));
        }
    }

    spans
}

/// A trailing hyphen on a multi-character span is treated as line-break
/// hyphenation (a lone "-" is more likely a bullet or minus sign).
fn ends_with_soft_hyphen(text: &str) -> bool {
    text.len() >= 2 && text.ends_with('-')
}

fn starts_lowercase(text: &str) -> bool {
    text.chars().next().is_some_and(char::is_lowercase)
}

/// Appends `text` to `md`, escaping characters that would otherwise be
/// interpreted as markdown syntax (`|` included, so table rows stay intact).
fn escape_markdown_into(md: &mut String, text: &str) {
    for ch in text.chars() {
        if matches!(ch, '\\' | '`' | '*' | '_' | '[' | ']' | '|') {
            md.push('\\');
        }
        md.push(ch);
    }
}

/// Renders a sequence of spans with inline markdown emphasis.
/// Adjacent spans with the same emphasis state share the same opening/closing
/// markers — so `bold A` followed by `bold B` produces `**A B**`, not `**A** **B**`.
fn render_spans_with_inline_formatting(md: &mut String, spans: &[InlineSpan<'_>]) {
    let mut current_state: Option<(bool, bool)> = None;
    let at_line_start = md.is_empty() || md.ends_with('\n');

    for (index, (span, leading_space, strip_trailing_hyphen)) in spans.iter().enumerate() {
        let next_state = (span.bold, span.italic);

        if Some(next_state) != current_state {
            if let Some(state) = current_state {
                push_emphasis_close(md, state);
            }
            if *leading_space && !md.is_empty() {
                let last = md.chars().last();
                if !matches!(last, Some(' ') | Some('\n')) {
                    md.push(' ');
                }
            }
            push_emphasis_open(md, next_state);
            current_state = Some(next_state);
        } else if *leading_space {
            md.push(' ');
        }

        let text = if *strip_trailing_hyphen {
            // Only set for spans ending in an ASCII '-', so the slice is safe.
            &span.text[..span.text.len() - 1]
        } else {
            span.text.as_str()
        };

        // An unstyled paragraph starting with '#', '>', "- ", or "1. " would
        // otherwise render as a heading, quote, or list.
        if index == 0
            && next_state == (false, false)
            && at_line_start
            && let Some(marker_offset) = block_marker_offset(text)
        {
            escape_markdown_into(md, &text[..marker_offset]);
            md.push('\\');
            escape_markdown_into(md, &text[marker_offset..]);
        } else {
            escape_markdown_into(md, text);
        }
    }

    if let Some(state) = current_state {
        push_emphasis_close(md, state);
    }
}

/// Detects a leading markdown block marker that would restructure the line
/// ('#'/'>' anywhere, '-'/'+' or "1."/"1)" followed by a space or the end).
/// Returns the byte offset of the character that needs escaping.
fn block_marker_offset(text: &str) -> Option<usize> {
    let mut chars = text.chars();
    match chars.next()? {
        '#' | '>' => Some(0),
        '-' | '+' if matches!(chars.next(), None | Some(' ')) => Some(0),
        first if first.is_ascii_digit() => {
            // Byte offset: safe because ASCII digits are single-byte.
            let digits = text
                .bytes()
                .take_while(|byte| byte.is_ascii_digit())
                .count();
            let mut rest = text[digits..].chars();
            match rest.next() {
                Some('.' | ')') if matches!(rest.next(), None | Some(' ')) => Some(digits),
                _ => None,
            }
        }
        _ => None,
    }
}

fn push_emphasis_open(md: &mut String, state: (bool, bool)) {
    match state {
        (true, true) => md.push_str("***"),
        (true, false) => md.push_str("**"),
        (false, true) => md.push('*'),
        (false, false) => {}
    }
}

fn push_emphasis_close(md: &mut String, state: (bool, bool)) {
    push_emphasis_open(md, state);
}

fn render_table_markdown(md: &mut String, lines: &[TextLine], table: &DetectedTable) {
    if table.rows.is_empty() {
        return;
    }

    let columns = &table.columns;
    let num_cols = columns.len();
    if num_cols == 0 {
        return;
    }

    md.push('\n');

    for (row_index, row) in table.rows.iter().enumerate() {
        let mut cells: Vec<String> = vec![String::new(); num_cols];
        // (previous span, its line index) per cell, to decide separators.
        let mut cell_state: Vec<Option<(&TextSpan, usize)>> = vec![None; num_cols];

        for &line_index in row {
            for span in &lines[line_index].spans {
                let col_idx = nearest_column(span.x, columns);
                let wants_space = match cell_state[col_idx] {
                    // Wrapped text from a later line always joins with a space;
                    // same-line neighbors join on the geometric gap rule.
                    Some((previous, previous_line)) => {
                        previous_line != line_index || needs_space_between(previous, span)
                    }
                    None => false,
                };
                if wants_space && !cells[col_idx].is_empty() {
                    cells[col_idx].push(' ');
                }
                escape_markdown_into(&mut cells[col_idx], &span.text);
                cell_state[col_idx] = Some((span, line_index));
            }
        }

        md.push('|');
        for cell in &cells {
            md.push(' ');
            md.push_str(cell.trim());
            md.push_str(" |");
        }
        md.push('\n');

        // Markdown tables require a separator after the first row.
        if row_index == 0 {
            md.push('|');
            for _ in 0..num_cols {
                md.push_str(" --- |");
            }
            md.push('\n');
        }
    }

    md.push('\n');
}

fn nearest_column(x: f32, columns: &[f32]) -> usize {
    columns
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            (x - *a)
                .abs()
                .partial_cmp(&(x - *b).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(i, _)| i)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw_char(ch: char, x: f32, y: f32, width: f32, font_size: f32) -> RawChar {
        RawChar {
            ch,
            x,
            y,
            width,
            height: font_size,
            font_size,
            font_name: "Helvetica".into(),
            bold: false,
            italic: false,
            space_before: false,
        }
    }

    fn span(text: &str, x: f32, y: f32, width: f32, font_size: f32) -> TextSpan {
        TextSpan {
            text: text.to_string(),
            font_size,
            font_name: "Helvetica".to_string(),
            bold: false,
            italic: false,
            x,
            y,
            width,
            height: font_size,
        }
    }

    fn styled_span(text: &str, x: f32, y: f32, font_size: f32, bold: bool) -> TextSpan {
        TextSpan {
            bold,
            ..span(text, x, y, text.len() as f32 * font_size * 0.5, font_size)
        }
    }

    fn text_line(y: f32, spans: Vec<TextSpan>) -> TextLine {
        TextLine { y, spans }
    }

    // ── font weights ───────────────────────────────────────────────

    #[test]
    fn font_weight_numeric_covers_all_variants() {
        let cases = [
            (PdfFontWeight::Weight100, 100),
            (PdfFontWeight::Weight200, 200),
            (PdfFontWeight::Weight300, 300),
            (PdfFontWeight::Weight400Normal, 400),
            (PdfFontWeight::Weight500, 500),
            (PdfFontWeight::Weight600, 600),
            (PdfFontWeight::Weight700Bold, 700),
            (PdfFontWeight::Weight800, 800),
            (PdfFontWeight::Weight900, 900),
            (PdfFontWeight::Custom(650), 650),
        ];
        for (weight, expected) in cases {
            assert_eq!(font_weight_numeric(&weight), expected);
        }
    }

    // ── span grouping ──────────────────────────────────────────────

    #[test]
    fn spans_merge_adjacent_chars_with_same_style() {
        let chars = [
            raw_char('H', 10.0, 700.0, 6.0, 12.0),
            raw_char('i', 16.0, 700.0, 3.0, 12.0),
        ];
        let spans = group_chars_into_spans(&chars);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "Hi");
        assert!((spans[0].width - 9.0).abs() < 0.01);
    }

    #[test]
    fn spans_insert_space_when_space_was_seen() {
        let mut second = raw_char('b', 20.0, 700.0, 6.0, 12.0);
        second.space_before = true;
        let chars = [raw_char('a', 10.0, 700.0, 6.0, 12.0), second];
        let spans = group_chars_into_spans(&chars);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "a b");
    }

    #[test]
    fn spans_split_on_bold_change() {
        let mut second = raw_char('b', 16.0, 700.0, 6.0, 12.0);
        second.bold = true;
        let chars = [raw_char('a', 10.0, 700.0, 6.0, 12.0), second];
        let spans = group_chars_into_spans(&chars);
        assert_eq!(spans.len(), 2);
        assert!(!spans[0].bold);
        assert!(spans[1].bold);
    }

    #[test]
    fn spans_split_on_font_size_change() {
        let chars = [
            raw_char('a', 10.0, 700.0, 6.0, 12.0),
            raw_char('b', 16.0, 700.0, 6.0, 14.0),
        ];
        assert_eq!(group_chars_into_spans(&chars).len(), 2);
    }

    #[test]
    fn spans_split_on_wide_horizontal_gap() {
        let chars = [
            raw_char('a', 10.0, 700.0, 6.0, 12.0),
            raw_char('b', 60.0, 700.0, 6.0, 12.0),
        ];
        assert_eq!(group_chars_into_spans(&chars).len(), 2);
    }

    #[test]
    fn spans_split_on_backward_jump() {
        let chars = [
            raw_char('a', 100.0, 700.0, 6.0, 12.0),
            raw_char('b', 10.0, 700.0, 6.0, 12.0),
        ];
        assert_eq!(group_chars_into_spans(&chars).len(), 2);
    }

    #[test]
    fn spans_split_on_line_change() {
        let chars = [
            raw_char('a', 10.0, 700.0, 6.0, 12.0),
            raw_char('b', 16.0, 680.0, 6.0, 12.0),
        ];
        assert_eq!(group_chars_into_spans(&chars).len(), 2);
    }

    #[test]
    fn spans_empty_input_yields_no_spans() {
        assert!(group_chars_into_spans(&[]).is_empty());
    }

    // ── line grouping ──────────────────────────────────────────────

    #[test]
    fn lines_group_same_y_and_sort_spans_by_x() {
        let spans = [
            span("right", 300.0, 700.0, 30.0, 12.0),
            span("left", 50.0, 700.5, 20.0, 12.0),
            span("below", 50.0, 650.0, 25.0, 12.0),
        ];
        let lines = group_spans_into_lines(&spans);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].spans[0].text, "left");
        assert_eq!(lines[0].spans[1].text, "right");
        assert_eq!(lines[1].spans[0].text, "below");
    }

    #[test]
    fn lines_split_beyond_y_tolerance() {
        let spans = [
            span("a", 50.0, 700.0, 6.0, 12.0),
            span("b", 60.0, 695.0, 6.0, 12.0),
        ];
        assert_eq!(group_spans_into_lines(&spans).len(), 2);
    }

    #[test]
    fn large_fonts_tolerate_proportional_baseline_jitter() {
        // 3pt of jitter splits 12pt text but stays on one line at 24pt.
        let small = [
            span("a", 50.0, 700.0, 6.0, 12.0),
            span("b", 60.0, 696.5, 6.0, 12.0),
        ];
        let large = [
            span("a", 50.0, 700.0, 12.0, 24.0),
            span("b", 70.0, 697.0, 12.0, 24.0),
        ];
        assert_eq!(group_spans_into_lines(&small).len(), 2);
        assert_eq!(group_spans_into_lines(&large).len(), 1);
    }

    #[test]
    fn lines_are_ordered_top_to_bottom_regardless_of_input_order() {
        // Content-stream order: footer first, heading last.
        let spans = [
            span("footer", 50.0, 40.0, 40.0, 12.0),
            span("body", 50.0, 400.0, 30.0, 12.0),
            span("heading", 50.0, 780.0, 50.0, 12.0),
        ];
        let lines = group_spans_into_lines(&spans);
        let order: Vec<&str> = lines
            .iter()
            .map(|line| line.spans[0].text.as_str())
            .collect();
        assert_eq!(order, vec!["heading", "body", "footer"]);
    }

    #[test]
    fn spans_rejoin_their_line_even_when_emitted_later() {
        // A span for an already-seen y arrives after other lines.
        let spans = [
            span("left", 50.0, 700.0, 20.0, 12.0),
            span("below", 50.0, 650.0, 25.0, 12.0),
            span("right", 300.0, 700.0, 30.0, 12.0),
        ];
        let lines = group_spans_into_lines(&spans);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].spans.len(), 2);
        assert_eq!(lines[0].spans[0].text, "left");
        assert_eq!(lines[0].spans[1].text, "right");
    }

    // ── table detection ────────────────────────────────────────────

    fn table_lines() -> Vec<TextLine> {
        vec![
            text_line(
                700.0,
                vec![
                    span("Name", 50.0, 700.0, 30.0, 12.0),
                    span("Value", 300.0, 700.0, 30.0, 12.0),
                ],
            ),
            text_line(
                680.0,
                vec![
                    span("Alpha", 50.0, 680.0, 30.0, 12.0),
                    span("1", 300.0, 680.0, 8.0, 12.0),
                ],
            ),
            text_line(
                660.0,
                vec![
                    span("Beta", 50.0, 660.0, 28.0, 12.0),
                    span("2", 300.0, 660.0, 8.0, 12.0),
                ],
            ),
        ]
    }

    #[test]
    fn detect_tables_finds_aligned_columns() {
        let tables = detect_tables(&table_lines());
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].start_line, 0);
        assert_eq!(tables[0].end_line, 3);
        assert_eq!(tables[0].columns.len(), 2);
        assert_eq!(tables[0].rows, vec![vec![0], vec![1], vec![2]]);
    }

    fn wrapped_table_lines() -> Vec<TextLine> {
        vec![
            text_line(
                700.0,
                vec![
                    span("A1", 60.0, 700.0, 15.0, 12.0),
                    span("Agriculture", 150.0, 700.0, 60.0, 12.0),
                    span("5 acres", 300.0, 700.0, 40.0, 12.0),
                ],
            ),
            text_line(
                685.0,
                vec![span("One-Family Dwellings,", 150.0, 685.0, 100.0, 12.0)],
            ),
            text_line(
                670.0,
                vec![span("Parks and Playgrounds", 150.0, 670.0, 100.0, 12.0)],
            ),
            text_line(
                655.0,
                vec![
                    span("A2", 60.0, 655.0, 15.0, 12.0),
                    span("More farming", 150.0, 655.0, 70.0, 12.0),
                    span("2 acres", 300.0, 655.0, 40.0, 12.0),
                ],
            ),
            text_line(
                640.0,
                vec![span("with extra uses", 150.0, 640.0, 80.0, 12.0)],
            ),
        ]
    }

    #[test]
    fn detect_tables_absorbs_wrapped_cell_lines() {
        let lines = wrapped_table_lines();
        let tables = detect_tables(&lines);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].start_line, 0);
        assert_eq!(tables[0].end_line, 5);
        assert_eq!(tables[0].rows, vec![vec![0, 1, 2], vec![3, 4]]);
    }

    #[test]
    fn markdown_merges_wrapped_cells_into_their_row() {
        let lines = wrapped_table_lines();
        let md = lines_to_markdown(&lines, estimate_body_font_size(&lines));
        assert!(
            md.contains(
                "| A1 | Agriculture One-Family Dwellings, Parks and Playgrounds | 5 acres |"
            ),
            "row 1 must merge wrapped text: {md}"
        );
        assert!(
            md.contains("| A2 | More farming with extra uses | 2 acres |"),
            "row 2 must merge wrapped text: {md}"
        );
    }

    #[test]
    fn wide_text_after_table_ends_the_table() {
        let mut lines = wrapped_table_lines();
        lines.push(text_line(
            625.0,
            vec![span(
                "This general note runs across the whole width of the page after the table",
                60.0,
                625.0,
                320.0,
                12.0,
            )],
        ));

        let tables = detect_tables(&lines);
        assert_eq!(tables.len(), 1);
        assert_eq!(
            tables[0].end_line, 5,
            "wide paragraph must not be swallowed into the table"
        );

        let md = lines_to_markdown(&lines, estimate_body_font_size(&lines));
        assert!(
            md.contains("This general note runs across"),
            "trailing paragraph must render: {md}"
        );
    }

    #[test]
    fn detect_tables_ignores_single_span_lines() {
        let lines = vec![
            text_line(700.0, vec![span("prose one", 50.0, 700.0, 60.0, 12.0)]),
            text_line(680.0, vec![span("prose two", 50.0, 680.0, 60.0, 12.0)]),
        ];
        assert!(detect_tables(&lines).is_empty());
    }

    #[test]
    fn detect_tables_ignores_misaligned_columns() {
        let lines = vec![
            text_line(
                700.0,
                vec![
                    span("a", 50.0, 700.0, 6.0, 12.0),
                    span("b", 200.0, 700.0, 6.0, 12.0),
                ],
            ),
            text_line(
                680.0,
                vec![
                    span("c", 100.0, 680.0, 6.0, 12.0),
                    span("d", 260.0, 680.0, 6.0, 12.0),
                ],
            ),
        ];
        assert!(detect_tables(&lines).is_empty());
    }

    // ── line classification ────────────────────────────────────────

    #[test]
    fn classify_line_by_font_size_ratio() {
        let body = 12.0;
        let heading1 = text_line(700.0, vec![span("Title", 50.0, 700.0, 40.0, 24.0)]);
        let heading2 = text_line(700.0, vec![span("Section", 50.0, 700.0, 40.0, 16.0)]);
        let paragraph = text_line(700.0, vec![span("Body", 50.0, 700.0, 30.0, 12.0)]);
        assert_eq!(classify_line(&heading1, body), LineKind::Heading(1));
        assert_eq!(classify_line(&heading2, body), LineKind::Heading(2));
        assert_eq!(classify_line(&paragraph, body), LineKind::Paragraph);
    }

    #[test]
    fn classify_line_detects_list_items() {
        for marker in ["• item", "● item", "◦ item", "- item", "– item", "· item"] {
            let line = text_line(700.0, vec![span(marker, 50.0, 700.0, 40.0, 12.0)]);
            assert_eq!(classify_line(&line, 12.0), LineKind::ListItem, "{marker:?}");
        }
    }

    #[test]
    fn classify_line_dash_without_space_is_paragraph() {
        let line = text_line(700.0, vec![span("-item", 50.0, 700.0, 40.0, 12.0)]);
        assert_eq!(classify_line(&line, 12.0), LineKind::Paragraph);
    }

    #[test]
    fn classify_line_empty_variants() {
        let no_spans = text_line(700.0, Vec::new());
        let blank = text_line(700.0, vec![span("   ", 50.0, 700.0, 10.0, 12.0)]);
        assert_eq!(classify_line(&no_spans, 12.0), LineKind::Empty);
        assert_eq!(classify_line(&blank, 12.0), LineKind::Empty);
    }

    // ── list markers ───────────────────────────────────────────────

    #[test]
    fn strip_list_marker_variants() {
        assert_eq!(strip_list_marker("• alpha"), "alpha");
        assert_eq!(strip_list_marker("– beta"), "beta");
        assert_eq!(strip_list_marker("- gamma"), "gamma");
        assert_eq!(strip_list_marker("-nodash"), "-nodash");
        assert_eq!(strip_list_marker("plain"), "plain");
    }

    // ── body font size estimation ──────────────────────────────────

    #[test]
    fn estimate_body_font_size_picks_dominant_size() {
        let lines = vec![
            text_line(700.0, vec![span("Big", 50.0, 700.0, 30.0, 24.0)]),
            text_line(
                680.0,
                vec![span(
                    "much longer body text dominates by character count",
                    50.0,
                    680.0,
                    300.0,
                    12.0,
                )],
            ),
        ];
        assert_eq!(estimate_body_font_size(&lines), 12.0);
    }

    #[test]
    fn estimate_body_font_size_defaults_when_empty() {
        assert_eq!(estimate_body_font_size(&[]), 12.0);
    }

    // ── vertical proximity and style ───────────────────────────────

    #[test]
    fn vertically_close_uses_line_height_factor() {
        let a = text_line(700.0, vec![span("a", 50.0, 700.0, 6.0, 12.0)]);
        let near = text_line(686.0, vec![span("b", 50.0, 686.0, 6.0, 12.0)]);
        let far = text_line(660.0, vec![span("c", 50.0, 660.0, 6.0, 12.0)]);
        assert!(is_vertically_close(&a, &near));
        assert!(!is_vertically_close(&a, &far));
    }

    #[test]
    fn vertically_close_falls_back_for_zero_height() {
        let mut a = text_line(700.0, vec![span("a", 50.0, 700.0, 6.0, 12.0)]);
        let mut b = text_line(690.0, vec![span("b", 50.0, 690.0, 6.0, 12.0)]);
        a.spans[0].height = 0.0;
        b.spans[0].height = 0.0;
        assert!(is_vertically_close(&a, &b));
    }

    #[test]
    fn dominant_style_prefers_longest_span() {
        let line = text_line(
            700.0,
            vec![
                styled_span("hi", 50.0, 700.0, 12.0, true),
                styled_span("a much longer regular span", 80.0, 700.0, 12.0, false),
            ],
        );
        assert_eq!(dominant_style(&line), (12.0, false, false));
    }

    // ── inline formatting ──────────────────────────────────────────

    #[test]
    fn inline_formatting_merges_adjacent_bold_spans() {
        let a = styled_span("A", 50.0, 700.0, 12.0, true);
        let b = styled_span("B", 60.0, 700.0, 12.0, true);
        let c = styled_span("c", 70.0, 700.0, 12.0, false);
        let mut md = String::new();
        render_spans_with_inline_formatting(
            &mut md,
            &[(&a, false, false), (&b, true, false), (&c, true, false)],
        );
        assert_eq!(md, "**A B** c");
    }

    #[test]
    fn inline_formatting_bold_italic_combined() {
        let mut styled = styled_span("x", 50.0, 700.0, 12.0, true);
        styled.italic = true;
        let mut md = String::new();
        render_spans_with_inline_formatting(&mut md, &[(&styled, false, false)]);
        assert_eq!(md, "***x***");
    }

    #[test]
    fn inline_formatting_italic_only() {
        let mut styled = styled_span("x", 50.0, 700.0, 12.0, false);
        styled.italic = true;
        let mut md = String::new();
        render_spans_with_inline_formatting(&mut md, &[(&styled, false, false)]);
        assert_eq!(md, "*x*");
    }

    #[test]
    fn inline_formatting_escapes_markdown_characters() {
        let literal = styled_span("2 * 3 = [6]_note", 50.0, 700.0, 12.0, false);
        let mut md = String::new();
        render_spans_with_inline_formatting(&mut md, &[(&literal, false, false)]);
        assert_eq!(md, "2 \\* 3 = \\[6\\]\\_note");
    }

    // ── escaping ───────────────────────────────────────────────────

    #[test]
    fn escape_markdown_covers_syntax_characters() {
        assert_eq!(escaped(r"\ ` * _ [ ] |"), r"\\ \` \* \_ \[ \] \|");
        assert_eq!(escaped("plain text 1-2"), "plain text 1-2");
    }

    fn escaped(text: &str) -> String {
        let mut md = String::new();
        escape_markdown_into(&mut md, text);
        md
    }

    // ── span spacing and hyphenation ───────────────────────────────

    #[test]
    fn tight_span_gaps_do_not_insert_spaces() {
        // A style change mid-word must not become "Re quired".
        let re = styled_span("Re", 50.0, 700.0, 12.0, true);
        let mut quired = styled_span("quired", 62.5, 700.0, 12.0, false);
        quired.width = 36.0;
        let line = text_line(700.0, vec![re, quired]);
        assert_eq!(line_to_text(&line), "Required");
    }

    #[test]
    fn wide_span_gaps_insert_spaces() {
        let left = span("left", 50.0, 700.0, 24.0, 12.0);
        let right = span("right", 90.0, 700.0, 30.0, 12.0);
        let line = text_line(700.0, vec![left, right]);
        assert_eq!(line_to_text(&line), "left right");
    }

    #[test]
    fn paragraph_spans_repair_line_break_hyphenation() {
        let lines = vec![
            text_line(700.0, vec![span("exam-", 50.0, 700.0, 30.0, 12.0)]),
            text_line(685.0, vec![span("ple continues", 50.0, 685.0, 80.0, 12.0)]),
        ];
        let spans = paragraph_spans(&lines, &[0, 1]);
        assert!(spans[0].2, "trailing hyphen must be marked for stripping");
        assert!(!spans[1].1, "no space after repaired hyphen");

        let mut md = String::new();
        render_spans_with_inline_formatting(&mut md, &spans);
        assert_eq!(md, "example continues");
    }

    #[test]
    fn paragraph_spans_keep_hyphen_before_capitalized_words() {
        let lines = vec![
            text_line(700.0, vec![span("Jean-", 50.0, 700.0, 30.0, 12.0)]),
            text_line(685.0, vec![span("Paul spoke", 50.0, 685.0, 60.0, 12.0)]),
        ];
        let spans = paragraph_spans(&lines, &[0, 1]);
        assert!(!spans[0].2, "compound-word hyphen must be kept");
        assert!(spans[1].1, "line break still becomes a space");

        let mut md = String::new();
        render_spans_with_inline_formatting(&mut md, &spans);
        assert_eq!(md, "Jean- Paul spoke");
    }

    // ── font-name style fallback ───────────────────────────────────

    #[test]
    fn bold_and_italic_detected_from_font_names() {
        assert_eq!(font_name_style("Helvetica-Bold"), (true, false));
        assert_eq!(font_name_style("Arial Black"), (true, false));
        assert_eq!(font_name_style("SourceSans-Heavy"), (true, false));
        assert_eq!(font_name_style("Times-Italic"), (false, true));
        assert_eq!(font_name_style("Helvetica-Oblique"), (false, true));
        assert_eq!(font_name_style("Georgia-BoldItalic"), (true, true));
        assert_eq!(font_name_style("Times-Roman"), (false, false));
    }

    // ── rotation mapping ───────────────────────────────────────────

    #[test]
    fn rotate_to_visual_maps_all_rotations() {
        let (left, top, width, height) = (100.0, 400.0, 50.0, 12.0);
        let (page_width, page_height) = (595.0, 842.0);
        let rotate = |rotation| {
            rotate_to_visual(left, top, width, height, rotation, page_width, page_height)
        };

        assert_eq!(
            rotate(PdfPageRenderRotation::None),
            (100.0, 400.0, 50.0, 12.0)
        );
        assert_eq!(
            rotate(PdfPageRenderRotation::Degrees90),
            (388.0, 495.0, 12.0, 50.0)
        );
        assert_eq!(
            rotate(PdfPageRenderRotation::Degrees180),
            (445.0, 454.0, 50.0, 12.0)
        );
        assert_eq!(
            rotate(PdfPageRenderRotation::Degrees270),
            (442.0, 150.0, 12.0, 50.0)
        );
    }

    // ── line-start block markers ───────────────────────────────────

    #[test]
    fn block_marker_offset_detects_line_start_hazards() {
        assert_eq!(block_marker_offset("# heading"), Some(0));
        assert_eq!(block_marker_offset("> quote"), Some(0));
        assert_eq!(block_marker_offset("- item"), Some(0));
        assert_eq!(block_marker_offset("+ item"), Some(0));
        assert_eq!(block_marker_offset("12. entry"), Some(2));
        assert_eq!(block_marker_offset("12) entry"), Some(2));
        assert_eq!(block_marker_offset("1."), Some(1));
        assert_eq!(block_marker_offset("-item"), None);
        assert_eq!(block_marker_offset("12.5 units"), None);
        assert_eq!(block_marker_offset("word"), None);
        assert_eq!(block_marker_offset(""), None);
    }

    #[test]
    fn paragraph_starting_with_marker_is_escaped() {
        let plain = span("# not a heading", 50.0, 700.0, 80.0, 12.0);
        let mut md = String::new();
        render_spans_with_inline_formatting(&mut md, &[(&plain, false, false)]);
        assert_eq!(md, "\\# not a heading");

        let numbered = span("1. not a list", 50.0, 700.0, 70.0, 12.0);
        let mut md = String::new();
        render_spans_with_inline_formatting(&mut md, &[(&numbered, false, false)]);
        assert_eq!(md, "1\\. not a list");
    }

    #[test]
    fn styled_paragraph_start_needs_no_marker_escape() {
        let bold = styled_span("#tag", 50.0, 700.0, 12.0, true);
        let mut md = String::new();
        render_spans_with_inline_formatting(&mut md, &[(&bold, false, false)]);
        assert_eq!(md, "**#tag**");
    }

    // ── bands and columns ──────────────────────────────────────────

    fn column_fixture(aligned_rows: bool) -> Vec<TextSpan> {
        let mut spans = Vec::new();
        for row in 0..5 {
            let left_y = 700.0 - row as f32 * 15.0;
            let right_y = if aligned_rows { left_y } else { left_y - 8.0 };
            spans.push(span(&format!("alpha {row}"), 60.0, left_y, 60.0, 12.0));
            spans.push(span(&format!("beta {row}"), 320.0, right_y, 60.0, 12.0));
        }
        spans
    }

    #[test]
    fn bands_split_at_large_vertical_gaps() {
        let lines = group_spans_into_lines(&[
            span("top", 50.0, 800.0, 20.0, 12.0),
            span("middle", 50.0, 760.0, 30.0, 12.0),
            span("bottom", 50.0, 745.0, 30.0, 12.0),
        ]);
        let bands = split_into_bands(lines);
        assert_eq!(bands.len(), 2);
        assert_eq!(bands[0].len(), 1);
        assert_eq!(bands[1].len(), 2);
    }

    #[test]
    fn two_column_layout_reads_left_column_first() {
        let md = spans_to_markdown(&column_fixture(false));
        let last_alpha = md.rfind("alpha 4").expect("alpha 4 present");
        let first_beta = md.find("beta 0").expect("beta 0 present");
        assert!(
            last_alpha < first_beta,
            "columns must render sequentially: {md}"
        );
    }

    #[test]
    fn aligned_rows_are_a_table_not_columns() {
        let spans = column_fixture(true);
        let lines = group_spans_into_lines(&spans);
        assert!(find_column_gutter(&lines).is_none());

        let md = spans_to_markdown(&spans);
        assert!(
            md.contains("| --- |"),
            "aligned rows must render as a table: {md}"
        );
    }

    #[test]
    fn full_width_title_band_precedes_columns() {
        let mut spans = vec![span("Grand Title Line", 60.0, 800.0, 320.0, 12.0)];
        spans.extend(column_fixture(false));

        let md = spans_to_markdown(&spans);
        let title = md.find("Grand Title Line").expect("title present");
        let first_alpha = md.find("alpha 0").expect("alpha 0 present");
        let last_alpha = md.rfind("alpha 4").expect("alpha 4 present");
        let first_beta = md.find("beta 0").expect("beta 0 present");
        assert!(
            title < first_alpha && last_alpha < first_beta,
            "expected title, then left column, then right column: {md}"
        );
    }

    #[test]
    fn single_column_text_is_not_split() {
        let spans: Vec<TextSpan> = (0..6)
            .map(|row| {
                span(
                    &format!("full width paragraph line {row}"),
                    60.0,
                    700.0 - row as f32 * 15.0,
                    380.0,
                    12.0,
                )
            })
            .collect();
        let lines = group_spans_into_lines(&spans);
        assert!(find_column_gutter(&lines).is_none());
    }

    #[test]
    fn raw_chars_to_text_restores_word_spaces() {
        let mut chars = vec![
            raw_char('H', 10.0, 700.0, 6.0, 12.0),
            raw_char('i', 16.0, 700.0, 3.0, 12.0),
            raw_char('y', 25.0, 700.0, 6.0, 12.0),
        ];
        chars[2].space_before = true;
        assert_eq!(raw_chars_to_text(&chars), "Hi y");
    }

    // ── markdown rendering ─────────────────────────────────────────

    #[test]
    fn markdown_renders_heading_paragraph_and_list() {
        let lines = vec![
            text_line(800.0, vec![span("Title", 50.0, 800.0, 60.0, 24.0)]),
            text_line(
                760.0,
                vec![span(
                    "First body line with plenty of characters here",
                    50.0,
                    760.0,
                    280.0,
                    12.0,
                )],
            ),
            text_line(
                745.0,
                vec![span(
                    "second body line continues the same paragraph",
                    50.0,
                    745.0,
                    280.0,
                    12.0,
                )],
            ),
            text_line(700.0, vec![span("• One", 50.0, 700.0, 40.0, 12.0)]),
            text_line(685.0, vec![span("• Two", 50.0, 685.0, 40.0, 12.0)]),
        ];

        let md = lines_to_markdown(&lines, estimate_body_font_size(&lines));
        assert!(md.contains("# Title"), "missing heading in: {md}");
        assert!(
            md.contains("First body line with plenty of characters here second body line"),
            "paragraph lines must join: {md}"
        );
        assert!(md.contains("- One\n- Two"), "missing list in: {md}");
    }

    #[test]
    fn markdown_renders_table_with_separator() {
        let lines = table_lines();
        let md = lines_to_markdown(&lines, estimate_body_font_size(&lines));
        assert!(md.contains("| Name | Value |"), "missing header row: {md}");
        assert!(md.contains("| --- | --- |"), "missing separator: {md}");
        assert!(md.contains("| Alpha | 1 |"), "missing data row: {md}");
        assert!(md.contains("| Beta | 2 |"), "missing data row: {md}");
    }

    #[test]
    fn markdown_of_empty_lines_is_empty() {
        assert_eq!(lines_to_markdown(&[], 12.0), "");
    }

    #[test]
    fn markdown_has_no_triple_newlines_or_outer_whitespace() {
        let lines = vec![
            text_line(800.0, vec![span("Title", 50.0, 800.0, 60.0, 24.0)]),
            text_line(
                600.0,
                vec![span(
                    "body text that is long enough to dominate size counting",
                    50.0,
                    600.0,
                    300.0,
                    12.0,
                )],
            ),
        ];
        let md = lines_to_markdown(&lines, estimate_body_font_size(&lines));
        assert!(!md.contains("\n\n\n"));
        assert_eq!(md, md.trim());
    }

    // ── column mapping ─────────────────────────────────────────────

    #[test]
    fn nearest_column_picks_closest_anchor() {
        let columns = [50.0, 300.0, 500.0];
        assert_eq!(nearest_column(60.0, &columns), 0);
        assert_eq!(nearest_column(290.0, &columns), 1);
        assert_eq!(nearest_column(1000.0, &columns), 2);
        assert_eq!(nearest_column(10.0, &[]), 0);
    }
}
