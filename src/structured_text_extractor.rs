use crate::document::open_document;
use crate::error::{PdfCutterError, Result};
use crate::extractor::ExtractionWarning;
use crate::page_selection::PageSelection;
use pdfium_render::prelude::{PdfFontWeight, PdfPage, PdfPages, Pdfium, PdfiumError};
use serde::Serialize;
use std::num::NonZeroU16;
use std::path::Path;

// ── Tolerance constants ────────────────────────────────────────────

/// Characters within this vertical distance (in points) are on the same line.
const LINE_Y_TOLERANCE: f32 = 2.0;

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
    font_name: String,
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
    start_line: usize,
    end_line: usize,
    columns: Vec<f32>,
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
    let total_pages = pages.len();

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
    let page_index = page_number.get() - 1;
    let page = pages
        .get(page_index)
        .map_err(|error| ExtractionWarning::LoadPage {
            page_number: page_number.get(),
            reason: error.to_string(),
        })?;

    let raw_chars = extract_raw_chars(&page).map_err(|error| ExtractionWarning::LoadPage {
        page_number: page_number.get(),
        reason: error.to_string(),
    })?;

    let spans = group_chars_into_spans(&raw_chars);
    let lines = group_spans_into_lines(&spans);
    let markdown = lines_to_markdown(&lines);

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
    let mut chars = Vec::new();
    let mut pending_space = false;

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

        let font_size = char_ref.scaled_font_size().value;
        if font_size <= 0.0 {
            continue;
        }

        let font_name = char_ref.font_name();
        let bold = char_ref.font_is_bold_reenforced()
            || char_ref
                .font_weight()
                .is_some_and(|w| font_weight_numeric(&w) >= 600);
        let italic = char_ref.font_is_italic();

        let left = bounds.left().value;
        let right = bounds.right().value;
        let top = bounds.top().value;
        let bottom = bounds.bottom().value;
        chars.push(RawChar {
            ch,
            x: left,
            y: top,
            width: (right - left).abs(),
            height: (top - bottom).abs(),
            font_size,
            font_name,
            bold,
            italic,
            space_before: pending_space,
        });
        pending_space = false;
    }

    Ok(chars)
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
            same_y(prev.y, raw.y) && same_font(prev, raw) && horizontal_gap_ok(prev, raw)
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
                font_name: raw.font_name.clone(),
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

fn same_y(y1: f32, y2: f32) -> bool {
    (y1 - y2).abs() < LINE_Y_TOLERANCE
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

fn group_spans_into_lines(spans: &[TextSpan]) -> Vec<TextLine> {
    if spans.is_empty() {
        return Vec::new();
    }

    let mut lines: Vec<TextLine> = Vec::new();

    for span in spans {
        let belongs_to_last = lines.last().is_some_and(|line| same_y(line.y, span.y));

        if belongs_to_last {
            lines.last_mut().unwrap().spans.push(span.clone());
        } else {
            lines.push(TextLine {
                y: span.y,
                spans: vec![span.clone()],
            });
        }
    }

    for line in &mut lines {
        line.spans
            .sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal));
    }

    lines
}

// ── Step 4: Detect tables ──────────────────────────────────────────

fn detect_tables(lines: &[TextLine]) -> Vec<DetectedTable> {
    let line_xs: Vec<Vec<f32>> = lines
        .iter()
        .map(|l| l.spans.iter().map(|s| s.x).collect())
        .collect();

    let mut tables = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        if line_xs[i].len() < MIN_TABLE_COLUMNS {
            i += 1;
            continue;
        }

        // Collect column anchors as the union of x-positions across rows that align.
        let mut anchors = line_xs[i].clone();
        let mut end = i + 1;

        while end < lines.len() {
            let candidate = &line_xs[end];
            if candidate.len() < MIN_TABLE_COLUMNS {
                break;
            }

            let aligned = count_aligned(&anchors, candidate);
            // A candidate is part of the table if a majority of its cols hit anchors.
            if aligned >= MIN_TABLE_COLUMNS && aligned * 2 >= candidate.len() {
                // Extend anchors with any new column positions.
                for &cx in candidate {
                    if !anchors
                        .iter()
                        .any(|&a| (a - cx).abs() < COLUMN_ANCHOR_TOLERANCE)
                    {
                        anchors.push(cx);
                    }
                }
                end += 1;
            } else {
                break;
            }
        }

        if end - i >= MIN_TABLE_ROWS {
            anchors.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            // Keep only anchors that have hits in at least half the rows (drops noise).
            let final_anchors = filter_anchors(&anchors, &line_xs[i..end]);
            if final_anchors.len() >= MIN_TABLE_COLUMNS {
                tables.push(DetectedTable {
                    start_line: i,
                    end_line: end,
                    columns: final_anchors,
                });
                i = end;
                continue;
            }
        }

        i += 1;
    }

    tables
}

fn count_aligned(anchors: &[f32], candidate: &[f32]) -> usize {
    candidate
        .iter()
        .filter(|&&cx| {
            anchors
                .iter()
                .any(|&a| (a - cx).abs() < COLUMN_ANCHOR_TOLERANCE)
        })
        .count()
}

fn filter_anchors(anchors: &[f32], rows: &[Vec<f32>]) -> Vec<f32> {
    let threshold = (rows.len() / 2).max(1);
    anchors
        .iter()
        .filter(|&&a| {
            rows.iter()
                .filter(|row| {
                    row.iter()
                        .any(|&cx| (cx - a).abs() < COLUMN_ANCHOR_TOLERANCE)
                })
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

fn lines_to_markdown(lines: &[TextLine]) -> String {
    if lines.is_empty() {
        return String::new();
    }

    let tables = detect_tables(lines);
    let body_size = estimate_body_font_size(lines);
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
                md.push_str(line_to_text(&lines[idx]).trim());
            }
            md.push_str("\n\n");
        }
        Block::Paragraph { line_indices } => {
            let mut spans = Vec::new();
            for (line_pos, &idx) in line_indices.iter().enumerate() {
                for (span_pos, span) in lines[idx].spans.iter().enumerate() {
                    let leading_space = !(line_pos == 0 && span_pos == 0);
                    spans.push((span, leading_space));
                }
            }
            render_spans_with_inline_formatting(md, &spans);
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
                md.push_str(&cleaned);
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
        if i > 0 {
            text.push(' ');
        }
        text.push_str(&span.text);
    }
    text
}

/// Renders a sequence of spans with inline markdown emphasis.
/// Adjacent spans with the same emphasis state share the same opening/closing
/// markers — so `bold A` followed by `bold B` produces `**A B**`, not `**A** **B**`.
fn render_spans_with_inline_formatting(md: &mut String, spans: &[(&TextSpan, bool)]) {
    // (span, needs_leading_space)
    let mut current_state: Option<(bool, bool)> = None;

    for (span, leading_space) in spans {
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

        md.push_str(&span.text);
    }

    if let Some(state) = current_state {
        push_emphasis_close(md, state);
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
    if table.start_line >= table.end_line {
        return;
    }

    let columns = &table.columns;
    let num_cols = columns.len();
    if num_cols == 0 {
        return;
    }

    md.push('\n');

    for (line_idx, line) in lines
        .iter()
        .enumerate()
        .take(table.end_line)
        .skip(table.start_line)
    {
        let mut cells: Vec<String> = vec![String::new(); num_cols];
        for span in &line.spans {
            let col_idx = nearest_column(span.x, columns);
            if !cells[col_idx].is_empty() {
                cells[col_idx].push(' ');
            }
            cells[col_idx].push_str(&span.text);
        }

        md.push('|');
        for cell in &cells {
            md.push(' ');
            md.push_str(cell.trim());
            md.push_str(" |");
        }
        md.push('\n');

        if line_idx == table.start_line {
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
            font_name: "Helvetica".to_string(),
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
            span("b", 60.0, 697.0, 6.0, 12.0),
        ];
        assert_eq!(group_spans_into_lines(&spans).len(), 2);
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
        render_spans_with_inline_formatting(&mut md, &[(&a, false), (&b, true), (&c, true)]);
        assert_eq!(md, "**A B** c");
    }

    #[test]
    fn inline_formatting_bold_italic_combined() {
        let mut styled = styled_span("x", 50.0, 700.0, 12.0, true);
        styled.italic = true;
        let mut md = String::new();
        render_spans_with_inline_formatting(&mut md, &[(&styled, false)]);
        assert_eq!(md, "***x***");
    }

    #[test]
    fn inline_formatting_italic_only() {
        let mut styled = styled_span("x", 50.0, 700.0, 12.0, false);
        styled.italic = true;
        let mut md = String::new();
        render_spans_with_inline_formatting(&mut md, &[(&styled, false)]);
        assert_eq!(md, "*x*");
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

        let md = lines_to_markdown(&lines);
        assert!(md.contains("# Title"), "missing heading in: {md}");
        assert!(
            md.contains("First body line with plenty of characters here second body line"),
            "paragraph lines must join: {md}"
        );
        assert!(md.contains("- One\n- Two"), "missing list in: {md}");
    }

    #[test]
    fn markdown_renders_table_with_separator() {
        let md = lines_to_markdown(&table_lines());
        assert!(md.contains("| Name | Value |"), "missing header row: {md}");
        assert!(md.contains("| --- | --- |"), "missing separator: {md}");
        assert!(md.contains("| Alpha | 1 |"), "missing data row: {md}");
        assert!(md.contains("| Beta | 2 |"), "missing data row: {md}");
    }

    #[test]
    fn markdown_of_empty_lines_is_empty() {
        assert_eq!(lines_to_markdown(&[]), "");
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
        let md = lines_to_markdown(&lines);
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
