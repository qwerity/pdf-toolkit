use pdf_core::error::{PdfError, Result};
use pdf_core::page::{open_document, page_count_u16, page_index};
use pdf_core::page_selection::PageSelection;
use pdf_core::text_quality::{is_probably_unreadable_text, sanitize_extracted_text};
use pdf_core::warning::ExtractionWarning;
use pdfium_render::prelude::{
    PdfFontWeight, PdfPage, PdfPageAnnotationCommon, PdfPageAnnotationType, PdfPageRenderRotation,
    PdfPageText, PdfPages, Pdfium,
};
use serde::Serialize;
use std::num::NonZeroU16;
use std::path::{Path, PathBuf};
use std::rc::Rc;

mod graphics;
mod ocr;
mod regions;

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

/// A character box taller than this many times its type size comes from a
/// font whose ascent or descent is wrong; its position is read from the
/// baseline instead (see [`plausible_box`]).
const MAX_BOX_TO_SIZE: f32 = 2.5;

/// Ascent and descent, as fractions of the type size, of the box a character
/// is given when its font's own box is implausible: a typical text face's.
const TYPICAL_ASCENT: f32 = 0.9;
const TYPICAL_DESCENT: f32 = 0.25;

/// Horizontal gap (× font size) beyond which we start a new span on the same line.
/// A table's column gap can be narrower than two ems: at 1.8 the AIM Norman
/// plan's compatibility matrix merged each row label with its first value
/// ("UM 4") and its whole header into one span, so no column lined up. Word
/// spaces, even stretched by justification, stay well under one em, and
/// `line_to_text` rejoins spans with a space, so prose reads the same.
const SPAN_GAP_TOLERANCE_FACTOR: f32 = 1.0;

/// How characters join into spans: the largest gap (× font size) and the
/// largest overlap (× font size) between one glyph and the next.
#[derive(Clone, Copy)]
struct SpanRules {
    gap_factor: f32,
    overlap_factor: f32,
}

/// The rules the markdown is read with.
const READING_SPANS: SpanRules = SpanRules {
    gap_factor: SPAN_GAP_TOLERANCE_FACTOR,
    // A ligature's box ("ff" in "traffic") overlaps the next glyph by half an
    // em; a full em back is a new line or overprinting, not the next letter.
    overlap_factor: 1.0,
};

/// The 0.17 grouping, kept for two outputs whose values callers already rely
/// on: the `spans` array of `structured` (a contract field), and the
/// `complex_table` routing flag, which the reviewed dossier annotations pin
/// page by page. Moving either is a decision of its own, not a side effect of
/// reading the markdown better.
const LEGACY_SPANS: SpanRules = SpanRules {
    gap_factor: 1.8,
    overlap_factor: 0.5,
};

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

/// Minimum number of table columns for wrapped rows to make the extracted
/// structure approximate. Two-column tables with wrapped cells (definition
/// lists, footnotes) reconstruct reliably; wider ones do not.
const COMPLEX_TABLE_MIN_COLUMNS: usize = 3;

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
    /// The markdown was read by OCR from the rendered page (a scan), not from
    /// its text layer. `spans` stay the text layer either way.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub ocr: bool,
}

/// Which pages structured extraction reads by OCR.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OcrPages {
    /// Scanned pages only: one image over most of the page and no readable
    /// text of its own.
    Scans,
    /// Every page, text layer or not.
    All,
}

/// OCR for structured extraction: the pages it reads, and the engine,
/// loaded from the models on the first page that needs it, so a run over
/// digital pages never pays for it.
pub struct OcrOptions {
    pub pages: OcrPages,
    models: PathBuf,
    engine: Option<pdf_ocr::Ocr>,
}

impl OcrOptions {
    /// OCR with the models in `models`.
    ///
    /// # Errors
    /// When `models` does not hold the model files.
    pub fn new(models: &Path, pages: OcrPages) -> anyhow::Result<Self> {
        pdf_ocr::check_models(models)?;
        Ok(Self {
            pages,
            models: models.to_path_buf(),
            engine: None,
        })
    }

    fn engine(&mut self) -> anyhow::Result<&mut pdf_ocr::Ocr> {
        if self.engine.is_none() {
            self.engine = Some(pdf_ocr::Ocr::load(&self.models)?);
        }
        Ok(self.engine.as_mut().expect("invariant: loaded just above"))
    }
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
    /// Left edge and width of each non-space glyph, so a table's lines can be
    /// re-split at gaps narrower than the ones that end a span. Not output.
    #[serde(skip)]
    pub glyphs: Vec<(f32, f32)>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum StructuredOutputFormat {
    Full,
    MarkdownOnly,
}

// ── Internal intermediate types ────────────────────────────────────

#[derive(Clone)]
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
    /// Set on its side: the baseline runs up or down the page, as in a
    /// rotated column heading ("Urban General" over a narrow use column).
    vertical: bool,
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
    columns: Vec<f32>,
    /// Logical rows as groups of contiguous line indices: wrapped cell text
    /// lines are grouped with the row line they continue. Never empty.
    rows: Vec<Vec<usize>>,
}

impl DetectedTable {
    /// First line of the table.
    fn start_line(&self) -> usize {
        self.rows[0][0]
    }

    /// One past the table's last line.
    fn end_line(&self) -> usize {
        self.rows
            .last()
            .and_then(|row| row.last())
            .map_or(0, |&line| line + 1)
    }

    fn contains_line(&self, line: usize) -> bool {
        line >= self.start_line() && line < self.end_line()
    }
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
    extract_structured_text_with(pdfium, pdf_path, selection, output_format, None)
}

/// [`extract_structured_text`], reading the pages `ocr` names by OCR.
///
/// # Errors
/// As [`extract_structured_text`]; an OCR failure on a page is a warning.
pub fn extract_structured_text_with(
    pdfium: &Pdfium,
    pdf_path: &Path,
    selection: PageSelection,
    output_format: StructuredOutputFormat,
    mut ocr: Option<&mut OcrOptions>,
) -> Result<StructuredExtractionReport> {
    let document = open_document(pdfium, pdf_path)?;
    let pages = document.pages();
    let total_pages = page_count_u16(pages)?;

    if total_pages == 0 {
        return Err(PdfError::EmptyDocument);
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
    // A table continued from the page before, when that page was read too.
    let mut carry: Option<regions::TableTail> = None;
    let mut previous_page: Option<u16> = None;
    for &page_number in &page_numbers {
        if previous_page.map(|p| p + 1) != Some(page_number.get()) {
            carry = None;
        }
        previous_page = Some(page_number.get());
        let page_ocr = ocr.as_deref_mut();
        match extract_page_structured(pages, page_number, output_format, &mut carry, page_ocr) {
            Ok((page, warning)) => {
                structured_pages.push(page);
                warnings.extend(warning);
            }
            Err(warning) => {
                carry = None;
                warnings.push(warning);
            }
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
    carry: &mut Option<regions::TableTail>,
    ocr: Option<&mut OcrOptions>,
) -> std::result::Result<(StructuredPage, Option<ExtractionWarning>), ExtractionWarning> {
    let page = pages
        .get(page_index(page_number))
        .map_err(|error| ExtractionWarning::LoadPage {
            page_number: page_number.get(),
            reason: error.to_string(),
        })?;

    let text = page.text().map_err(|error| ExtractionWarning::LoadPage {
        page_number: page_number.get(),
        reason: error.to_string(),
    })?;

    // Judge the same sanitized page text as the plain-text extractor, so the
    // shared heuristic's thresholds apply to one input distribution.
    let unreadable = is_probably_unreadable_text(&sanitize_extracted_text(&text.all()));
    let reader =
        ocr.filter(|o| o.pages == OcrPages::All || ocr::is_scanned(&page, &text, unreadable));
    if unreadable && reader.is_none() {
        return Err(ExtractionWarning::UnreadableText {
            page_number: page_number.get(),
        });
    }

    let rotation = reading_rotation(&page, &text);
    let mut raw_chars = extract_raw_chars(&page, &text, rotation);
    repair_legacy_armenian(&mut raw_chars);
    let output_spans = match format {
        StructuredOutputFormat::Full => legacy_spans(&raw_chars),
        StructuredOutputFormat::MarkdownOnly => Vec::new(),
    };
    // `spans` stay the text layer; the markdown reads what the page shows.
    raw_chars.extend(note_chars(&page, rotation));
    // Only a scan found on its own is weighed against its text layer; a
    // caller asking for every page by OCR gets OCR.
    let weigh_text_layer =
        reader.as_ref().is_some_and(|o| o.pages == OcrPages::Scans) && !unreadable;
    // The table carried in from the page before, for the text layer to be
    // read against if OCR's reading of the page is weighed against it.
    let before_ocr = weigh_text_layer.then(|| carry.clone());
    let read = reader
        .map(|o| {
            o.engine()
                .and_then(|engine| ocr::ocr_markdown(&page, engine, carry))
        })
        .transpose()
        .map_err(|error| ExtractionWarning::OcrFailed {
            page_number: page_number.get(),
            reason: format!("{error:#}"),
        });
    // A failed OCR falls back to the text layer, unless there is none to read.
    let (markdown, by_ocr, warning) = match read {
        // A scanner's own hidden OCR is sometimes the better reading of the
        // two: of text laid sideways on the page, or set in italics the
        // engine runs together. Whichever reads more like words wins.
        Ok(Some(ocr_markdown)) => match before_ocr {
            Some(mut layer_carry) => {
                let text = page_markdown(&page, raw_chars, rotation, &mut layer_carry);
                if ocr::reads_better(&text, &ocr_markdown) {
                    *carry = layer_carry;
                    (text, false, None)
                } else {
                    (ocr_markdown, true, None)
                }
            }
            None => (ocr_markdown, true, None),
        },
        Err(failed) if unreadable => return Err(failed),
        other => (
            page_markdown(&page, raw_chars, rotation, carry),
            false,
            other.err(),
        ),
    };

    Ok((
        StructuredPage {
            page: page_number.get(),
            spans: output_spans,
            markdown,
            ocr: by_ocr,
        },
        warning,
    ))
}

/// Marks where a table or text box sits in the reading flow until its own
/// markdown replaces it. Private-use code points never occur in extracted
/// text, so the marker cannot collide with page content.
const REGION_OPEN: char = '\u{E000}';
const REGION_CLOSE: char = '\u{E001}';

/// The page's markdown: tables and text boxes read from its drawing, and the
/// rest of its text in visual reading order around them.
///
/// Each region stands in the flow as one placeholder span at its top-left
/// corner, the size of the body text, so bands, columns and paragraphs are
/// decided exactly as before and the region lands where it is printed.
///
/// `carry` holds the previous page's last table, for the top table here to
/// continue, and receives this page's last table.
fn page_markdown(
    page: &PdfPage<'_>,
    chars: Vec<RawChar>,
    rotation: PdfPageRenderRotation,
    carry: &mut Option<regions::TableTail>,
) -> String {
    // A glyph off the page is not printed: a page cut from a two-page spread
    // still carries the other page's text, placed beside it. Dropped here, not
    // at extraction, so `spans` and the routing flag read what they always did.
    // Only on an upright page: a turned page's glyphs are placed by
    // `rotate_to_visual`, whose frame is not the page box, and are all kept;
    // its drawing is in the page's frame, so it is not read either.
    let upright = matches!(rotation, PdfPageRenderRotation::None);
    // pdfium gives the page's size as displayed; a `/Rotate`d page read in
    // its own frame is the other way round.
    let (width, height) = (page.width().value, page.height().value);
    let turned = matches!(
        page.rotation().unwrap_or(PdfPageRenderRotation::None),
        PdfPageRenderRotation::Degrees90 | PdfPageRenderRotation::Degrees270
    );
    let page_box = upright.then_some(if turned {
        (height, width)
    } else {
        (width, height)
    });
    let drawing = if upright {
        graphics::collect(page)
    } else {
        graphics::PageGraphics::default()
    };
    page_markdown_drawn(page, chars, page_box, &drawing, carry)
}

/// Along its baseline, the farthest one character of a sideways run may sit
/// from the next, and across it, how far off its column, in line heights.
const SIDEWAYS_STEP: f32 = 1.5;
const SIDEWAYS_DRIFT: f32 = 0.3;

/// Characters in a column that make a run of sideways text: fewer are a
/// stack of labels or a coincidence.
const SIDEWAYS_MIN_RUN: usize = 3;

/// A turned glyph's type size, as a share of the line height its box spans,
/// and its advance once laid upright, in type sizes.
const SIDEWAYS_TYPE_SHARE: f32 = 0.75;
const SIDEWAYS_ADVANCE: f32 = 0.55;

/// Text drawn a quarter turn round on an upright page, outside any table (a
/// plan's title running up its margin, a street name along a map's road), as
/// upright lines. Read where it lies, each of its characters is a line of its
/// own ("n a l P e v i s n e h e r p m o C" for "Comprehensive Plan" up
/// Harrah's margin). pdfium does not always report the turn: Harrah's banner
/// comes with angle 0 and type size 1. Its geometry does: each character
/// follows the one before it along a column rather than a line, in a box
/// wider than it is tall. Such characters, taken in the order they were drawn
/// (the order they read, whichever way they are turned), form a run, laid out
/// left to right from its top-left corner. A table's turned column headings
/// are read in their cells instead (`regions`), so only characters outside
/// tables come here.
fn upright_sideways(chars: Vec<RawChar>) -> Vec<RawChar> {
    let mut out = Vec::with_capacity(chars.len());
    let mut run: Vec<RawChar> = Vec::new();
    for c in chars {
        if !run.last().is_some_and(|last| follows_down_column(last, &c)) {
            settle_run(&mut run, &mut out);
        }
        run.push(c);
    }
    settle_run(&mut run, &mut out);
    out
}

/// Whether `next` is the character after `last` in a column of sideways text.
fn follows_down_column(last: &RawChar, next: &RawChar) -> bool {
    // A turned glyph's box spans the line across and its advance along.
    let across = last.width;
    let (dx, dy) = (next.x - last.x, next.y - last.y);
    across > last.height
        && next.width > next.height
        && dx.abs() <= across * SIDEWAYS_DRIFT
        && dy != 0.0
        && dy.abs() <= across * SIDEWAYS_STEP
}

/// Moves a run of characters to `out`, laid upright when it is sideways text
/// (long enough, or every character reported turned), as it was otherwise.
fn settle_run(run: &mut Vec<RawChar>, out: &mut Vec<RawChar>) {
    if run.len() < SIDEWAYS_MIN_RUN && !run.iter().all(|c| c.vertical) {
        out.append(run);
        return;
    }
    let left = run.iter().map(|c| c.x).fold(f32::INFINITY, f32::min);
    let top = run.iter().map(|c| c.y).fold(f32::NEG_INFINITY, f32::max);
    let across = run.iter().map(|c| c.width).fold(0.0, f32::max);
    let mut x = left;
    for c in run.drain(..) {
        // A size pdfium could not read from the turned matrix comes back tiny.
        let size = if c.font_size > 2.0 {
            c.font_size
        } else {
            across * SIDEWAYS_TYPE_SHARE
        };
        let advance = size * SIDEWAYS_ADVANCE;
        if c.space_before {
            x += advance;
        }
        out.push(RawChar {
            x,
            y: top,
            width: advance,
            height: size,
            font_size: size,
            vertical: false,
            ..c
        });
        x += advance;
    }
}

/// [`page_markdown`] over a given drawing: the page's own vector drawing, or
/// the rules found in a scan's pixels. `page_box` is the page, `(width,
/// height)`, in the characters' coordinates; a character outside it is
/// dropped.
fn page_markdown_drawn(
    page: &PdfPage<'_>,
    mut chars: Vec<RawChar>,
    page_box: Option<(f32, f32)>,
    drawing: &graphics::PageGraphics,
    carry: &mut Option<regions::TableTail>,
) -> String {
    if let Some((width, height)) = page_box {
        chars.retain(|c| {
            let (cx, cy) = (c.x + c.width / 2.0, c.y - c.height / 2.0);
            (0.0..=width).contains(&cx) && (0.0..=height).contains(&cy)
        });
    }
    for c in &mut chars {
        if c.ch == LINE_END_HYPHEN {
            c.ch = '-';
        }
    }
    let page_area = page.width().value * page.height().value;
    let found = regions::detect(drawing, &chars, page_area);
    let incoming = carry.take();
    if found.is_empty() {
        return spans_to_markdown(&group_chars_into_spans(&upright_sideways(chars)));
    }
    let tables = || {
        found
            .iter()
            .enumerate()
            .filter(|(_, r)| matches!(r.kind, regions::Kind::Table(_)))
    };
    let top = tables()
        .max_by(|a, b| a.1.rect.top.total_cmp(&b.1.rect.top))
        .map(|(i, _)| i);
    let bottom = tables()
        .min_by(|a, b| a.1.rect.bottom.total_cmp(&b.1.rect.bottom))
        .map(|(i, _)| i);

    let body_size =
        estimate_body_font_size(&group_spans_into_lines(&group_chars_into_spans(&chars)));
    let (free, held) = regions::assign(&found, chars);
    let mut spans = group_chars_into_spans(&upright_sideways(free));
    let mut rendered = Vec::with_capacity(found.len());
    for (i, (region, region_chars)) in found.iter().zip(held).enumerate() {
        rendered.push(match &region.kind {
            regions::Kind::Table(grid) => {
                let continued = incoming.as_ref().filter(|_| top == Some(i));
                let table = regions::render_table(grid, &region_chars, &drawing.fills, continued);
                if bottom == Some(i) {
                    *carry = table.tail;
                }
                table.markdown
            }
            regions::Kind::TextBox => {
                spans_to_markdown(&group_chars_into_spans(&upright_sideways(region_chars)))
            }
        });
        spans.push(TextSpan {
            text: format!("{REGION_OPEN}{i}{REGION_CLOSE}"),
            font_size: body_size,
            font_name: String::new(),
            bold: false,
            italic: false,
            x: region.rect.left,
            y: region.rect.top,
            width: region.rect.width(),
            height: body_size,
            glyphs: Vec::new(),
        });
    }

    substitute_regions(&spans_to_markdown(&spans), &rendered)
}

/// Replaces each region placeholder with the region's markdown as a block of
/// its own, whatever paragraph the placeholder was joined into.
fn substitute_regions(markdown: &str, rendered: &[String]) -> String {
    let mut out = String::with_capacity(markdown.len());
    let mut rest = markdown;
    while let Some(open) = rest.find(REGION_OPEN) {
        let Some(close) = rest[open..].find(REGION_CLOSE).map(|c| open + c) else {
            break;
        };
        // A region can stand where a heading would have begun: drop the
        // heading marker it leaves behind rather than print an empty heading.
        let before = rest[..open].trim_end().trim_end_matches('#').trim_end();
        out.push_str(before);
        let index: Option<usize> = rest[open + REGION_OPEN.len_utf8()..close].parse().ok();
        if let Some(md) = index
            .and_then(|i| rendered.get(i))
            .filter(|md| !md.is_empty())
        {
            out.push_str("\n\n");
            out.push_str(md);
        }
        out.push_str("\n\n");
        rest = rest[close + REGION_CLOSE.len_utf8()..].trim_start();
    }
    out.push_str(rest);
    collapse_blank_lines(out.trim()).to_string()
}

// ── Step 1: Extract raw characters ─────────────────────────────────

/// [`pdf_armscii::repair`] over a page's characters rather than over a string.
///
/// The structured extractor never assembles the page into one string -- every
/// span keeps its own coordinates -- so it cannot go through
/// `sanitize_extracted_text`, which is where the other three text paths get
/// this. The decision is still the page's: a single character carries no
/// evidence at all about which alphabet it belongs to.
fn repair_legacy_armenian(chars: &mut [RawChar]) {
    if !pdf_armscii::Tally::of(chars.iter().map(|c| c.ch)).has_legacy_text() {
        return;
    }
    for raw in chars {
        if let Some(letter) = pdf_armscii::letter(raw.ch) {
            raw.ch = letter;
        }
    }
}

/// Share of a page's characters that must be drawn at one quarter turn for
/// the page to be read in that turn's frame.
const UPRIGHT_FRAME_SHARE: f32 = 0.6;

/// Characters a page needs before their angles can choose its frame.
const MIN_FRAME_CHARS: usize = 20;

/// The turn that sets a page's text upright. Usually its `/Rotate`: a
/// drawing sheet stored landscape and turned for display has its text drawn
/// a quarter turn round, and `/Rotate` sets it straight. But `/Rotate` can
/// disagree with the text: Tucson's Saguaro Trails PAD p.414 is a landscape
/// brochure whose text runs along the page while `/Rotate` stands it on end,
/// so read as displayed it is one letter per line. The frame most of the
/// page's characters are upright in is the one it is read in.
fn reading_rotation(page: &PdfPage<'_>, text: &PdfPageText<'_>) -> PdfPageRenderRotation {
    let rotation = page.rotation().unwrap_or(PdfPageRenderRotation::None);
    let mut turns = [0usize; 4];
    for c in text.chars().iter() {
        if !c
            .unicode_char()
            .is_some_and(|ch| !ch.is_whitespace() && !ch.is_control())
        {
            continue;
        }
        let angle = c.angle_degrees().unwrap_or(0.0).rem_euclid(360.0);
        let quarter = (angle / 90.0).round();
        // Only a character close to a quarter turn votes.
        if (angle - quarter * 90.0).abs() < 30.0 {
            turns[quarter as usize % 4] += 1;
        }
    }
    let total: usize = turns.iter().sum();
    let (turn, &count) = turns
        .iter()
        .enumerate()
        .max_by_key(|&(_, &n)| n)
        .expect("invariant: four turns");
    if total < MIN_FRAME_CHARS || (count as f32) < total as f32 * UPRIGHT_FRAME_SHARE {
        return rotation;
    }
    // pdfium measures a character's angle counterclockwise and `/Rotate`
    // turns clockwise: text drawn at 270° stands upright under `/Rotate 90`.
    match turn {
        1 => PdfPageRenderRotation::Degrees270,
        2 => PdfPageRenderRotation::Degrees180,
        3 => PdfPageRenderRotation::Degrees90,
        _ => PdfPageRenderRotation::None,
    }
}

/// Share of a note's box height one line of its text is taken to fill.
const NOTE_TYPE_HEIGHT: f32 = 0.8;

/// Largest type size a note's text is given when its box is tall.
const NOTE_MAX_TYPE_SIZE: f32 = 12.0;

/// The text of the page's free-text notes, as characters laid in each note's
/// box. A note typed onto a page after it was made ("Adopted as Regulation,
/// October 12, 2009" on the cover of Juneau's historic district standards)
/// is printed on the page but is no part of its text layer, so it is read
/// from the annotation: each line of its contents across the box, top down.
fn note_chars(page: &PdfPage<'_>, rotation: PdfPageRenderRotation) -> Vec<RawChar> {
    let (page_width, page_height) = (page.width().value, page.height().value);
    let mut chars = Vec::new();
    for note in page.annotations().iter() {
        if note.annotation_type() != PdfPageAnnotationType::FreeText || note.is_hidden() {
            continue;
        }
        let (Some(contents), Ok(bounds)) = (note.contents(), note.bounds()) else {
            continue;
        };
        let lines: Vec<&str> = contents
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        let (left, right) = (bounds.left().value, bounds.right().value);
        let (top, bottom) = (bounds.top().value, bounds.bottom().value);
        if lines.is_empty() || right <= left || top <= bottom {
            continue;
        }
        let line_height = (top - bottom) / lines.len() as f32;
        let font_size = (line_height * NOTE_TYPE_HEIGHT).min(NOTE_MAX_TYPE_SIZE);
        for (i, line) in lines.iter().enumerate() {
            let pitch = ((right - left) / line.chars().count() as f32).min(font_size * 0.6);
            let line_top = top - i as f32 * line_height;
            for mut c in spread_chars(line, left, line_top, pitch, font_size, font_size) {
                (c.x, c.y, c.width, c.height) = rotate_to_visual(
                    c.x,
                    c.y,
                    c.width,
                    c.height,
                    rotation,
                    page_width,
                    page_height,
                );
                chars.push(c);
            }
        }
    }
    chars
}

/// A line of text laid across a box as characters, `pitch` apart from
/// `left`, their tops at `top`: for text that comes with a box and no glyph
/// positions (an OCR line, a note typed onto the page).
fn spread_chars(
    text: &str,
    left: f32,
    top: f32,
    pitch: f32,
    height: f32,
    font_size: f32,
) -> Vec<RawChar> {
    let font_name: Rc<str> = Rc::from("");
    let mut space = false;
    let mut chars = Vec::new();
    for (i, ch) in text.chars().enumerate() {
        if ch.is_whitespace() {
            space = true;
            continue;
        }
        chars.push(RawChar {
            ch,
            x: left + i as f32 * pitch,
            y: top,
            width: pitch * 0.9,
            height,
            font_size,
            font_name: Rc::clone(&font_name),
            bold: false,
            italic: false,
            space_before: std::mem::take(&mut space),
            vertical: false,
        });
    }
    chars
}

fn extract_raw_chars(
    page: &PdfPage<'_>,
    text: &PdfPageText<'_>,
    rotation: PdfPageRenderRotation,
) -> Vec<RawChar> {
    let page_width = page.width().value;
    let page_height = page.height().value;
    let mut chars = Vec::new();
    let mut pending_space = false;
    // Pages rarely use more than a handful of fonts; a linearly probed list
    // keeps style lookups cached even when runs alternate between fonts.
    let mut font_cache: Vec<(Rc<str>, bool, bool)> = Vec::new();

    for char_ref in text.chars().iter() {
        let ch = match char_ref.unicode_char() {
            Some(c) => c,
            None => continue,
        };

        if ch.is_control() && !matches!(ch, '\n' | '\r' | '\t' | LINE_END_HYPHEN) {
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

        // Weight, flags, and the name-derived style are font properties, so
        // they are computed once per font and reused. Only the
        // bold-reenforced check remains per character (it reflects the text
        // render mode, not the font).
        let raw_font_name = char_ref.font_name();
        let (font_name, font_bold, font_italic) =
            match font_cache.iter().find(|(name, ..)| **name == raw_font_name) {
                Some((name, font_bold, font_italic)) => (Rc::clone(name), *font_bold, *font_italic),
                None => {
                    let name: Rc<str> = raw_font_name.into();
                    let (name_bold, name_italic) = font_name_style(&name);
                    // A declared weight is a hundred (100 to 900); any other is
                    // pdfium's estimate from the stems where none is declared (a
                    // page printed by a browser: plain Georgia, stem 133, comes out
                    // 665). A name that names its face beats an estimate. Stems of
                    // 120 and 140 estimate exactly 600 and 700 and pass as declared.
                    let font_bold = name_bold
                        || char_ref.font_weight().is_some_and(|w| {
                            let w = font_weight_numeric(&w);
                            w >= 600 && (w.is_multiple_of(100) || !names_its_face(&name))
                        });
                    let font_italic = name_italic || char_ref.font_is_italic();
                    font_cache.push((Rc::clone(&name), font_bold, font_italic));
                    (name, font_bold, font_italic)
                }
            };

        let bold = font_bold || char_ref.font_is_bold_reenforced();

        let left = bounds.left().value;
        let right = bounds.right().value;
        let angle = char_ref.angle_degrees().unwrap_or(0.0);
        let (top, bottom) = plausible_box(
            bounds.top().value,
            bounds.bottom().value,
            font_size,
            angle,
            || char_ref.origin_y().ok().map(|y| y.value),
        );
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
            vertical: is_vertical(angle, rotation),
        });
        // The next character keeps the space that preceded a line-end
        // hyphen, as it did when the hyphen was dropped.
        if ch != LINE_END_HYPHEN {
            pending_space = false;
        }
    }

    chars
}

/// Words a font's name gives its style with rather than its face.
const FONT_STYLE_WORDS: [&str; 8] = [
    "bold", "black", "heavy", "italic", "oblique", "regular", "roman", "mt",
];

/// Whether a font's name names its face ("Georgia", "FrutigerLTStd"), rather
/// than numbering it as a subset or a resource does ("F1", "T1_0", "TT2"):
/// four letters or more once its subset prefix and style words are gone.
fn names_its_face(name: &str) -> bool {
    let name = name.split_once('+').map_or(name, |(_, rest)| rest);
    let mut face = name.to_ascii_lowercase();
    for word in FONT_STYLE_WORDS {
        face = face.replace(word, "");
    }
    face.chars().filter(char::is_ascii_alphabetic).count() >= 4
}

/// The top and bottom of a character's box. A font that declares an ascent or
/// descent several times its size (Juneau's 2013 Comprehensive Plan sets 10.5
/// pt body text in a 58 pt box) moves every line of it up or down the page, so
/// a paragraph's first line lands on its heading and the two are read as one
/// line. The baseline is exact whatever the font says, so an implausible box
/// is rebuilt around it; `baseline` is only asked for then. Text drawn at an
/// angle keeps its box: its baseline is not a page height.
fn plausible_box(
    top: f32,
    bottom: f32,
    font_size: f32,
    angle: f32,
    baseline: impl FnOnce() -> Option<f32>,
) -> (f32, f32) {
    if (top - bottom).abs() <= MAX_BOX_TO_SIZE * font_size || angle.abs() > 0.5 {
        return (top, bottom);
    }
    match baseline() {
        Some(base) => (
            base + TYPICAL_ASCENT * font_size,
            base - TYPICAL_DESCENT * font_size,
        ),
        None => (top, bottom),
    }
}

/// Whether a glyph drawn at `angle` degrees reads up or down the page as
/// displayed, once the page's own `/Rotate` is taken out.
fn is_vertical(angle: f32, rotation: PdfPageRenderRotation) -> bool {
    let page = match rotation {
        PdfPageRenderRotation::Degrees90 => 90.0,
        PdfPageRenderRotation::Degrees180 => 180.0,
        PdfPageRenderRotation::Degrees270 => 270.0,
        PdfPageRenderRotation::None => 0.0,
    };
    ((angle - page).rem_euclid(180.0) - 90.0).abs() < 30.0
}

/// pdfium's code for a hyphen that ends a line ("cul-" before "de-sac"). It is
/// printed, so markdown keeps it as `-`; `spans` and the routing flag leave it
/// out, as they always have.
const LINE_END_HYPHEN: char = '\u{2}';

/// The page's `spans` as 0.17 reported them: without line-end hyphens.
fn legacy_spans(chars: &[RawChar]) -> Vec<TextSpan> {
    if !chars.iter().any(|c| c.ch == LINE_END_HYPHEN) {
        return group_chars_into_spans_with(chars, LEGACY_SPANS);
    }
    let kept: Vec<RawChar> = chars
        .iter()
        .filter(|c| c.ch != LINE_END_HYPHEN)
        .cloned()
        .collect();
    group_chars_into_spans_with(&kept, LEGACY_SPANS)
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

/// Fallback style detection from the font name, for fonts that carry no
/// usable weight metadata (e.g. pdfium reports weight 0 for the standard
/// Helvetica-Bold). Returns `(bold, italic)`.
fn font_name_style(name: &str) -> (bool, bool) {
    let name = name.to_ascii_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| name.contains(w));
    (has(&FONT_STYLE_WORDS[..3]), has(&FONT_STYLE_WORDS[3..5]))
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
    group_chars_into_spans_with(chars, READING_SPANS)
}

fn group_chars_into_spans_with(chars: &[RawChar], rules: SpanRules) -> Vec<TextSpan> {
    if chars.is_empty() {
        return Vec::new();
    }

    let mut spans: Vec<TextSpan> = Vec::new();

    for raw in chars {
        let should_merge = spans.last().is_some_and(|prev| {
            same_line_y(prev.y, raw.y, prev.font_size)
                && same_font(prev, raw)
                && horizontal_gap_ok(prev, raw, rules)
        });

        if should_merge {
            let prev = spans.last_mut().unwrap();
            if raw.space_before {
                prev.text.push(' ');
            }
            prev.text.push(raw.ch);
            prev.glyphs.push((raw.x, raw.width));
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
                glyphs: vec![(raw.x, raw.width)],
            });
        }
    }

    spans
}

/// Gap (× font size) at which a table line's spans are split again once the
/// line is known to be in a table. Narrow enough to part a row label from its
/// first value; used only inside a detected table, where a split can move
/// nothing into a wrong column that alignment has not already put there.
const TABLE_SPAN_SPLIT_FACTOR: f32 = 0.4;

/// A span cut wherever two consecutive glyphs are further apart than
/// `gap_factor` em. Word spaces before a cut are dropped with it.
fn split_span(span: &TextSpan, gap_factor: f32) -> Vec<TextSpan> {
    let text_chars: Vec<char> = span.text.chars().collect();
    if span.glyphs.len() != text_chars.iter().filter(|c| **c != ' ').count()
        || span.glyphs.len() < 2
    {
        return vec![span.clone()];
    }
    let mut pieces: Vec<TextSpan> = Vec::new();
    let mut text = String::new();
    let mut glyphs: Vec<(f32, f32)> = Vec::new();
    let flush = |text: &mut String, glyphs: &mut Vec<(f32, f32)>, pieces: &mut Vec<TextSpan>| {
        let trimmed = text.trim();
        if let (false, Some(first), Some(last)) =
            (trimmed.is_empty(), glyphs.first(), glyphs.last())
        {
            pieces.push(TextSpan {
                text: trimmed.to_string(),
                x: first.0,
                width: last.0 + last.1 - first.0,
                glyphs: std::mem::take(glyphs),
                ..span.clone()
            });
        }
        text.clear();
        glyphs.clear();
    };
    let mut g = 0;
    for ch in text_chars {
        if ch == ' ' {
            text.push(ch);
            continue;
        }
        let glyph = span.glyphs[g];
        if let Some(prev) = glyphs.last()
            && glyph.0 - (prev.0 + prev.1) > span.font_size * gap_factor
        {
            flush(&mut text, &mut glyphs, &mut pieces);
        }
        text.push(ch);
        glyphs.push(glyph);
        g += 1;
    }
    flush(&mut text, &mut glyphs, &mut pieces);
    pieces
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

fn horizontal_gap_ok(span: &TextSpan, raw: &RawChar, rules: SpanRules) -> bool {
    let gap = raw.x - (span.x + span.width);
    gap < span.font_size * rules.gap_factor && gap > -span.font_size * rules.overlap_factor
}

// ── Step 3: Group spans into lines ─────────────────────────────────

/// Clusters spans into lines by vertical position and orders the result
/// visually: lines top-to-bottom, spans within a line left-to-right.
///
/// Spans are sorted top-to-bottom first, so a single pass comparing each
/// span against the current line suffices even when the PDF content stream
/// emits text out of visual order (e.g. footers first, or two-column
/// layouts interleaved).
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
            let rendered = lines_to_markdown(&column.lines, body_size, column.tables);
            if !rendered.is_empty() {
                md.push_str(&rendered);
                md.push_str("\n\n");
            }
        }
    }

    md.trim_end().to_string()
}

/// A final rendering unit: a column's lines, plus the tables the gutter
/// guard already detected for exactly this line set (reused by rendering so
/// the expensive detection pass runs once per line set).
struct ColumnLines {
    lines: Vec<TextLine>,
    tables: Option<Vec<DetectedTable>>,
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
fn split_band_into_columns(lines: Vec<TextLine>, depth: usize) -> Vec<ColumnLines> {
    if depth >= MAX_COLUMN_SPLIT_DEPTH {
        return vec![ColumnLines {
            lines,
            tables: None,
        }];
    }

    let (gutter, tables) = find_column_gutter(&lines);
    let Some(gutter_x) = gutter else {
        // Tables detected by the guard describe exactly this line set.
        return vec![ColumnLines { lines, tables }];
    };
    if let Some(tops) = side_head_rows(&lines, gutter_x) {
        return side_head_columns(lines, gutter_x, &tops);
    }

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
/// flows. Returns the gutter's center x (or `None` when no plausible gutter
/// exists, including when the gap is a table column divider, recognizable by
/// rows aligned across it), alongside any tables detected for the guard so
/// the caller can reuse them when the lines are left unsplit.
fn find_column_gutter(lines: &[TextLine]) -> (Option<f32>, Option<Vec<DetectedTable>>) {
    // Two lines are enough for a side heading beside its text; every other
    // split still needs MIN_COLUMN_LINES on each side (column_split_is_plausible).
    if lines.len() < 2 {
        return (None, None);
    }

    let (min_x, max_x) = lines
        .iter()
        .flat_map(|line| &line.spans)
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(low, high), span| {
            (low.min(span.x), high.max(span.x + span.width))
        });
    let extent = max_x - min_x;
    if !extent.is_finite() || extent <= COLUMN_GUTTER_MIN_WIDTH * 4.0 {
        return (None, None);
    }

    // Coverage histogram over x: how many spans touch each bin.
    let bin_count = ((extent / GUTTER_BIN_WIDTH).ceil() as usize).clamp(1, 2048);
    let bin_width = extent / bin_count as f32;
    let mut coverage = vec![0u32; bin_count];
    for line in lines {
        for span in &line.spans {
            let first = (((span.x - min_x) / bin_width) as usize).min(bin_count - 1);
            let last = (((span.x + span.width - min_x) / bin_width) as usize).min(bin_count - 1);
            // A span of negative width (a right-to-left run) puts `last`
            // before `first`; the bins it covers are the same either way.
            let (first, last) = (first.min(last), first.max(last));
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
            offset > extent * 0.1 && offset < extent * 0.9
        })
        .collect();
    if candidates.is_empty() {
        return (None, None);
    }

    // Table detection is the expensive pass; run it only for the rare bands
    // that actually have a plausible-looking gap. Columns of running text line
    // up row by row too; those are the columns being looked for, not a table.
    let tables: Vec<DetectedTable> = detect_tables(lines)
        .into_iter()
        .filter(|table| !is_column_flow(lines, table))
        .collect();
    // Only a table that fills a real share of the band can veto a gutter: two
    // or three lines of running text that happen to line up across the
    // columns (the Form-Based Code's p.28 has two) are not a table the page
    // is built around.
    let min_veto_lines = (lines.len() as f32 * MIN_VETO_TABLE_SHARE).ceil() as usize;
    let vetoing: Vec<DetectedTable> = tables
        .iter()
        .filter(|t| t.end_line() - t.start_line() >= min_veto_lines)
        .map(|t| DetectedTable {
            columns: t.columns.clone(),
            rows: t.rows.clone(),
        })
        .collect();

    let gutter = candidates
        .into_iter()
        .filter(|&(_, center)| !gutter_is_inside_table(&vetoing, center))
        .filter(|&(_, center)| {
            // A gutter near the band's edge is usually a margin (line numbers,
            // list markers); only a side heading justifies a split there.
            let offset = center - min_x;
            let near_edge = offset < extent * 0.2 || offset > extent * 0.8;
            column_split_is_plausible(lines, center, near_edge)
        })
        // The widest gutter; of gutters as wide, the one nearest the middle,
        // so four equal columns split two and two and each half splits once
        // more within the split depth, rather than one and three.
        .max_by(|a, b| {
            let off_middle = |x: f32| (x - min_x - extent / 2.0).abs();
            a.0.cmp(&b.0)
                .then(off_middle(b.1).total_cmp(&off_middle(a.1)))
        })
        .map(|(_, center)| center);

    (gutter, Some(tables))
}

/// Share of a band's lines a table must cover before a gutter inside it is
/// read as the table's own column divider rather than a page column's edge.
const MIN_VETO_TABLE_SHARE: f32 = 0.3;

/// Share of its width a column's text must fill, line after line, in every
/// column for aligned rows to be page columns of prose rather than a table.
/// Measured: the four prose columns of the AIM Norman plan's p.59 fill 80-90%;
/// the fullest column of a real table (the shared parking table's percentages)
/// fills about half. A table has at least one column of short values.
const COLUMN_FLOW_FILL: f32 = 0.65;

/// True when a detected "table" is really several columns of running text:
/// every one of its columns is filled nearly edge to edge on most lines.
fn is_column_flow(lines: &[TextLine], table: &DetectedTable) -> bool {
    let anchors = &table.columns;
    let right_edge = (table.start_line()..table.end_line())
        .flat_map(|i| &lines[i].spans)
        .map(|s| s.x + s.width)
        .fold(f32::NEG_INFINITY, f32::max);
    let table_lines = &lines[table.start_line()..table.end_line()];
    (0..anchors.len()).all(|c| {
        let column: Vec<&TextSpan> = table_lines
            .iter()
            .flat_map(|l| &l.spans)
            .filter(|s| nearest_column(s.x, anchors) == c)
            .collect();
        // A side heading or a column of bullet glyphs belongs to the text beside it.
        if is_side_heading(&column, table_lines)
            || column.iter().all(|s| {
                list_marker_len(s.text.trim()).is_some() && s.text.trim().chars().count() == 1
            })
        {
            return true;
        }
        let width = anchors.get(c + 1).copied().unwrap_or(right_edge) - anchors[c];
        if width <= 0.0 {
            return false;
        }
        let mut fills: Vec<f32> = (table.start_line()..table.end_line())
            .filter_map(|i| {
                let filled: f32 = lines[i]
                    .spans
                    .iter()
                    .filter(|s| nearest_column(s.x, anchors) == c)
                    .map(|s| s.width)
                    .sum();
                (filled > 0.0).then_some(filled / width)
            })
            .collect();
        if fills.is_empty() {
            return false;
        }
        fills.sort_by(f32::total_cmp);
        fills[fills.len() / 2] >= COLUMN_FLOW_FILL
    })
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
fn column_split_is_plausible(lines: &[TextLine], gutter_x: f32, near_edge: bool) -> bool {
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

    let (left, right): (Vec<&TextSpan>, Vec<&TextSpan>) = lines
        .iter()
        .flat_map(|line| &line.spans)
        .partition(|span| span_is_left_of(span, gutter_x));
    // A heading printed beside its text, in the band's first lines, or a
    // column of them, each beside the paragraph it heads.
    if (is_side_heading(&left, lines) && right.len() >= 2 && is_running_text(&right))
        || side_head_rows(lines, gutter_x).is_some()
    {
        return true;
    }
    if near_edge {
        return false;
    }
    if left_only >= MIN_COLUMN_LINES
        && right_only >= MIN_COLUMN_LINES
        && both <= left_only.min(right_only)
    {
        return true;
    }
    // Typeset columns often share baselines, so most lines cross the gutter.
    // They are still columns when both sides are running text; a table always
    // has a side of short values or single terms.
    left.len() >= MIN_COLUMN_LINES
        && right.len() >= MIN_COLUMN_LINES
        && is_running_text(&left)
        && is_running_text(&right)
}

/// Lines from the band's top within which a side heading must sit. A column
/// of bold terms running down every row is a definitions table instead.
const SIDE_HEADING_MAX_LINES: usize = 3;

/// A few bold or capitalised spans, all in the band's first lines: the
/// "LOCATIONAL CRITERIA" a plan prints to the left of the text it heads.
fn is_side_heading(spans: &[&TextSpan], lines: &[TextLine]) -> bool {
    let Some(limit) = lines
        .get(SIDE_HEADING_MAX_LINES.min(lines.len()) - 1)
        .map(|l| l.y)
    else {
        return false;
    };
    !spans.is_empty()
        && spans.len() <= SIDE_HEADING_MAX_LINES * 2
        && spans.iter().all(|s| is_styled(s) && s.y >= limit - 0.5)
}

/// Set apart as a label is: bold, or every letter a capital.
fn is_styled(span: &TextSpan) -> bool {
    let mut letters = span.text.chars().filter(|c| c.is_alphabetic()).peekable();
    span.bold || (letters.peek().is_some() && letters.all(char::is_uppercase))
}

/// The first of each run of `ys` (top first) that starts after a gap wider
/// than `gap`: the first lines of paragraphs, or the tops of wrapped labels.
fn run_starts(ys: &[f32], gap: f32) -> Vec<f32> {
    ys.iter()
        .enumerate()
        .filter(|&(i, &y)| i == 0 || ys[i - 1] - y > gap)
        .map(|(_, &y)| y)
        .collect()
}

/// Words a side-head label may run to on one line: "CIRCULATION &", not prose.
const SIDE_HEAD_MAX_WORDS: usize = 4;

/// Line pitches between two paragraphs of the text a side head sits beside,
/// beyond the usual line spacing; and how close, in pitches, a label's top
/// must be to a paragraph's first line.
const SIDE_HEAD_PARAGRAPH_GAP: f32 = 1.5;
const SIDE_HEAD_ALIGN: f32 = 0.5;

/// The tops of the labels of a side-head layout, top first: a column of short
/// bold or capitalised labels left of `gutter_x`, each group of them level
/// with the first line of a paragraph of running text on the right. The AIM
/// Norman plan's land use pages print DESCRIPTION & CONTEXT, BUILDING TYPES
/// and SITE DESIGN down the left of the text each heads. Read as two columns,
/// every label would come before every paragraph; read as lines, each label
/// is spliced into the paragraph's first lines ("would make & CONTEXT good").
/// A single label is a side heading ([`is_side_heading`]); two or more are
/// this.
fn side_head_rows(lines: &[TextLine], gutter_x: f32) -> Option<Vec<f32>> {
    let mut label_ys: Vec<f32> = Vec::new();
    let mut right_ys: Vec<f32> = Vec::new();
    let mut right: Vec<&TextSpan> = Vec::new();
    for line in lines {
        let (left, r): (Vec<&TextSpan>, Vec<&TextSpan>) = line
            .spans
            .iter()
            .partition(|s| span_is_left_of(s, gutter_x));
        let is_label = |s: &&TextSpan| {
            is_styled(s) && s.text.split_whitespace().count() <= SIDE_HEAD_MAX_WORDS
        };
        if !left.iter().all(is_label) {
            return None;
        }
        if !left.is_empty() {
            label_ys.push(line.y);
        }
        if !r.is_empty() {
            right_ys.push(line.y);
            right.extend(r);
        }
    }
    if label_ys.len() < 2 || right_ys.len() < 2 || !is_running_text(&right) {
        return None;
    }

    let mut gaps: Vec<f32> = right_ys.windows(2).map(|w| w[0] - w[1]).collect();
    gaps.sort_by(f32::total_cmp);
    let pitch = gaps[gaps.len() / 2];
    if pitch <= 0.0 {
        return None;
    }
    let gap = SIDE_HEAD_PARAGRAPH_GAP * pitch;
    let paragraph_starts = run_starts(&right_ys, gap);
    // A label may wrap over several lines; a wider gap starts the next one.
    let tops = run_starts(&label_ys, gap);
    let aligned = tops.iter().all(|&top| {
        paragraph_starts
            .iter()
            .any(|&start| (start - top).abs() <= SIDE_HEAD_ALIGN * pitch)
    });
    (tops.len() >= 2 && aligned).then_some(tops)
}

/// A side-head layout read row by row: whatever precedes the first label,
/// then each label followed by the text beside it, down to the next label.
fn side_head_columns(lines: Vec<TextLine>, gutter_x: f32, tops: &[f32]) -> Vec<ColumnLines> {
    // Row 0 is above the first label; row k + 1 starts at label k's top.
    let mut rows: Vec<(Vec<TextSpan>, Vec<TextSpan>)> = vec![Default::default(); tops.len() + 1];
    for span in lines.into_iter().flat_map(|line| line.spans) {
        let slack = span.font_size * SIDE_HEAD_ALIGN;
        let row = tops.iter().filter(|&&top| span.y <= top + slack).count();
        let (left, right) = &mut rows[row];
        if span_is_left_of(&span, gutter_x) {
            left.push(span);
        } else {
            right.push(span);
        }
    }
    rows.into_iter()
        .flat_map(|(left, right)| [left, right])
        .filter(|spans| !spans.is_empty())
        .map(|spans| ColumnLines {
            lines: group_spans_into_lines_owned(spans),
            tables: None,
        })
        .collect()
}

/// Share of a side's lines that must read as prose for it to be running text
/// rather than table cells.
const RUNNING_TEXT_SHARE: f32 = 0.75;

/// True when most of the side's lines are prose: three words or more, most of
/// them lowercase words. Judged per line, not per span, since small capitals
/// and inline styles cut a line of prose into many short spans, while a
/// table's value column ("P P P", "1, 3") never reads as words.
fn is_running_text(spans: &[&TextSpan]) -> bool {
    // (y, text, every span of it set apart as a heading is)
    let mut lines: Vec<(f32, String, bool)> = Vec::new();
    for span in spans {
        // A bullet glyph marks the text; it is not a word of it.
        let text = span.text.trim();
        if list_marker_len(text).is_some() && text.chars().count() == 1 {
            continue;
        }
        match lines
            .iter_mut()
            .find(|(y, ..)| same_line_y(*y, span.y, span.font_size))
        {
            Some((_, line, styled)) => {
                line.push(' ');
                line.push_str(text);
                *styled &= is_styled(span);
            }
            None => lines.push((span.y, text.to_string(), is_styled(span))),
        }
    }
    // A heading over a paragraph ("INSTITUTIONAL/PUBLIC | 5%" over the three
    // columns of a plan's land use categories) is neither prose nor a row of
    // values: it is left out of the count, as a table's header row is. A side
    // mostly of capitals (a column of codes: "R-1", "C-2") is not headings over
    // paragraphs, so then every line counts.
    let styled = lines.iter().filter(|(.., styled)| *styled).count();
    let skip_styled = styled * 2 <= lines.len();
    let (mut counted, mut prose) = (0usize, 0usize);
    for (_, line, styled) in &lines {
        if !(skip_styled && *styled) {
            counted += 1;
            prose += usize::from(is_wordy_line(line));
        }
    }
    counted > 0 && prose as f32 >= counted as f32 * RUNNING_TEXT_SHARE
}

/// Three words or more, at least three in five of them lowercase words: a line
/// of text rather than a row of values. Deliberately loose, since a narrow
/// column's line ("Norman's two main street mixed-use areas") may hold no
/// function word at all.
fn is_wordy_line(line: &str) -> bool {
    let words: Vec<&str> = line.split_whitespace().collect();
    let wordlike = words
        .iter()
        .filter(|w| {
            w.chars().filter(|c| c.is_alphabetic()).count() >= 2
                && w.chars().any(char::is_lowercase)
        })
        .count();
    words.len() >= 3 && wordlike * 5 >= words.len() * 3
}

/// Words that carry a sentence and almost never a table cell. A label and its
/// value ("Front yard 25 feet", "Lot area 6,000") has none of them.
const FUNCTION_WORDS: [&str; 25] = [
    "the", "of", "and", "to", "a", "an", "in", "for", "is", "are", "be", "shall", "may", "or",
    "not", "any", "with", "by", "at", "on", "from", "as", "that", "this", "which",
];

/// A line of a sentence: wordy, and holding at least one function word. A
/// table row reads as a label and values ("Office 100 percent 80 percent",
/// "Single-family detached, one 1 1 1 1"), never as a sentence. Strict, since
/// it decides that something detected as a table is not one.
fn is_prose_line(line: &str) -> bool {
    let function_words = || line.split_whitespace().filter(|w| is_function_word(w));
    // A sentence about zones is thick with codes ("for R1, RA, RE and RS
    // properties in Height Districts 2, 3 and 4") that are not words, but
    // still carries its function words.
    (is_wordy_line(line) && function_words().next().is_some())
        || (line.split_whitespace().count() >= PROSE_MIN_WORDS
            && function_words().take(PROSE_FUNCTION_WORDS).count() == PROSE_FUNCTION_WORDS)
}

/// Whether a word, bare of punctuation and case ("To", "to,"), is one of
/// `FUNCTION_WORDS`.
fn is_function_word(word: &str) -> bool {
    let bare = word.trim_matches(|c: char| !c.is_alphanumeric());
    FUNCTION_WORDS.iter().any(|f| f.eq_ignore_ascii_case(bare))
}

/// A line this long holding this many function words is a sentence however
/// many of its other words are codes.
const PROSE_MIN_WORDS: usize = 8;
const PROSE_FUNCTION_WORDS: usize = 3;

/// True when a detected table's lines are sentences: lines of one paragraph
/// whose inline styles (small capitals, italics) split them into spans that
/// happen to line up. Lines under three words (a heading, a wrapped last
/// word) are evidence neither way.
fn reads_as_prose(lines: &[TextLine], table: &DetectedTable) -> bool {
    let judged: Vec<String> = lines[table.start_line()..table.end_line()]
        .iter()
        .map(line_to_text)
        .filter(|text| text.split_whitespace().count() >= 3)
        .collect();
    let prose = judged.iter().filter(|text| is_prose_line(text)).count();
    !judged.is_empty() && prose as f32 >= judged.len() as f32 * RUNNING_TEXT_SHARE
}

// ── Step 4: Detect tables ──────────────────────────────────────────

/// True when the page contains a table whose extracted structure is
/// approximate: wrapped (multi-line) rows spread across several columns,
/// where cell-to-column association relies on geometry alone. Used by page
/// inspection to recommend a vision-LLM verification pass.
pub fn page_has_complex_tables(page: &PdfPage<'_>) -> bool {
    let Ok(text) = page.text() else {
        return false;
    };

    // The routing flag reads the page as displayed, as it always has; only the
    // markdown turns a page to the frame its text is upright in.
    let rotation = page.rotation().unwrap_or(PdfPageRenderRotation::None);
    let mut raw_chars = extract_raw_chars(page, &text, rotation);
    raw_chars.retain(|c| c.ch != LINE_END_HYPHEN);
    repair_legacy_armenian(&mut raw_chars);
    if raw_chars.is_empty() {
        return false;
    }

    let spans = group_chars_into_spans_with(&raw_chars, LEGACY_SPANS);
    let lines = group_spans_into_lines(&spans);
    detect_tables_with(&lines, false)
        .iter()
        .any(is_complex_table)
}

fn is_complex_table(table: &DetectedTable) -> bool {
    table.columns.len() >= COMPLEX_TABLE_MIN_COLUMNS && table.rows.iter().any(|row| row.len() > 1)
}

fn detect_tables(lines: &[TextLine]) -> Vec<DetectedTable> {
    detect_tables_with(lines, true)
}

/// `guard_lists`: a line opening with a list marker cannot be a table row.
/// Off only for the routing flag (see `LEGACY_SPANS`).
fn detect_tables_with(lines: &[TextLine], guard_lists: bool) -> Vec<DetectedTable> {
    // Precomputed once: a rejected table attempt retries at the next line
    // and rescans largely the same lines.
    let line_xs: Vec<Vec<f32>> = lines
        .iter()
        .map(|line| line.spans.iter().map(|span| span.x).collect())
        .collect();

    let mut tables = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        match detect_table_at(lines, &line_xs, i, COLUMN_ANCHOR_TOLERANCE, guard_lists)
            .filter(|table| !guard_lists || !reads_as_prose(lines, table))
        {
            Some(table) => {
                i = table.end_line();
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
fn detect_table_at(
    lines: &[TextLine],
    line_xs: &[Vec<f32>],
    start: usize,
    tolerance: f32,
    guard_lists: bool,
) -> Option<DetectedTable> {
    let is_list_line = |i: usize| guard_lists && starts_with_list_marker(&lines[i]);
    if line_xs[start].len() < MIN_TABLE_COLUMNS || is_list_line(start) {
        return None;
    }

    // Column anchors are the union of x-positions across rows that align,
    // kept sorted so the continuation check can search them directly.
    let mut anchors = line_xs[start].clone();
    anchors.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut row_xs: Vec<&[f32]> = vec![&line_xs[start]];
    let mut end = start + 1;

    while end < lines.len() {
        let candidate = &line_xs[end];
        if is_list_line(end) {
            break;
        }

        if candidate.len() >= MIN_TABLE_COLUMNS {
            let aligned = count_aligned(&anchors, candidate, tolerance);

            // A candidate is a row if a majority of its columns hit anchors.
            if aligned >= MIN_TABLE_COLUMNS && aligned * 2 >= candidate.len() {
                for &cx in candidate {
                    if !aligned_to_any(&anchors, cx, tolerance) {
                        let position = anchors.partition_point(|&a| a < cx);
                        anchors.insert(position, cx);
                    }
                }
                row_xs.push(candidate);
                end += 1;
                continue;
            }
        }

        if is_continuation_line(&lines[end], &anchors, tolerance) {
            end += 1;
        } else {
            break;
        }
    }

    if row_xs.len() < MIN_TABLE_ROWS {
        return None;
    }

    // Keep only anchors that have hits in at least half the rows (drops noise).
    let final_anchors = filter_anchors(&anchors, &row_xs, tolerance);
    if final_anchors.len() < MIN_TABLE_COLUMNS {
        return None;
    }

    let rows = cluster_lines_into_rows(lines, start, end, &final_anchors, tolerance);

    Some(DetectedTable {
        columns: final_anchors,
        rows,
    })
}

/// True when every span of the line sits inside a single column's territory:
/// aligned to an anchor and not reaching into the next column. Full-width
/// paragraph text following a table fails this and ends the table.
/// `sorted_anchors` must be in ascending order.
fn is_continuation_line(line: &TextLine, sorted_anchors: &[f32], tolerance: f32) -> bool {
    !line.spans.is_empty()
        && line
            .spans
            .iter()
            .all(|span| span_stays_in_column(span, sorted_anchors, tolerance))
}

fn span_stays_in_column(span: &TextSpan, sorted_anchors: &[f32], tolerance: f32) -> bool {
    let column = nearest_column(span.x, sorted_anchors);
    if !is_aligned(sorted_anchors[column], span.x, tolerance) {
        return false;
    }

    match sorted_anchors.get(column + 1) {
        Some(&next_anchor) => span.x + span.width < next_anchor,
        None => true,
    }
}

/// The single alignment rule shared by table growth, anchor filtering, row
/// clustering, and the continuation check.
fn is_aligned(anchor: f32, x: f32, tolerance: f32) -> bool {
    (anchor - x).abs() < tolerance
}

/// `sorted_anchors` must be ascending; only the two anchors flanking the
/// insertion point can be within tolerance.
fn aligned_to_any(sorted_anchors: &[f32], x: f32, tolerance: f32) -> bool {
    let insertion = sorted_anchors.partition_point(|&a| a < x);
    let below = insertion.checked_sub(1).map(|i| sorted_anchors[i]);
    let above = sorted_anchors.get(insertion).copied();
    below
        .into_iter()
        .chain(above)
        .any(|anchor| is_aligned(anchor, x, tolerance))
}

/// Groups the table's lines into logical rows: a line with content in the
/// first column starts a new row, every other line continues the row above
/// (wrapped cell text).
fn cluster_lines_into_rows(
    lines: &[TextLine],
    start: usize,
    end: usize,
    columns: &[f32],
    tolerance: f32,
) -> Vec<Vec<usize>> {
    let first_anchor = columns[0];
    let mut rows: Vec<Vec<usize>> = Vec::new();

    for (offset, line) in lines[start..end].iter().enumerate() {
        let index = start + offset;
        // A span opens a new row only when the first column is its nearest
        // anchor — plain distance would also match spans of a narrowly
        // indented second column.
        let starts_row = line.spans.iter().any(|span| {
            nearest_column(span.x, columns) == 0 && is_aligned(first_anchor, span.x, tolerance)
        });

        match rows.last_mut() {
            Some(row) if !starts_row => row.push(index),
            _ => rows.push(vec![index]),
        }
    }

    rows
}

fn count_aligned(anchors: &[f32], candidate: &[f32], tolerance: f32) -> usize {
    candidate
        .iter()
        .filter(|&&cx| aligned_to_any(anchors, cx, tolerance))
        .count()
}

fn filter_anchors(anchors: &[f32], rows: &[&[f32]], tolerance: f32) -> Vec<f32> {
    let threshold = (rows.len() / 2).max(1);
    anchors
        .iter()
        .filter(|&&anchor| {
            rows.iter()
                .filter(|row| row.iter().any(|&cx| is_aligned(anchor, cx, tolerance)))
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

    // Set larger than the body, a numbered line is a section heading ("702.
    // Use Table", "3. Zoning Districts"), not an item of a list.
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

    if is_list_item_start(trimmed) {
        return LineKind::ListItem;
    }

    LineKind::Paragraph
}

fn is_list_item_start(text: &str) -> bool {
    list_marker_len(text).is_some() || ordered_marker_len(text).is_some()
}

/// Words a code opens a division of itself with, lowercase.
const SECTION_LABELS: [&str; 11] = [
    "sec",
    "section",
    "article",
    "chapter",
    "appendix",
    "part",
    "division",
    "subchapter",
    "subpart",
    "title",
    "schedule",
];

/// Whether a line opens with a section label and its number: "SEC. 520",
/// "Section 4.1", "Article IV", "Appendix B", "§ 36-520". Such a line starts
/// a heading even when the line above is a heading of the same size.
fn starts_section_label(text: &str) -> bool {
    section_label(text).is_some()
}

/// A section label and its number with nothing after them: "Appendix C",
/// "CHAPTER 11", "Article IV."
fn is_bare_section_label(text: &str) -> bool {
    section_label(text) == Some(0)
}

/// When a line opens with a section label and its number, how many words
/// follow them.
fn section_label(text: &str) -> Option<usize> {
    let mut words = text.split_whitespace();
    let first = words.next()?;
    let (label, id) = match first.strip_prefix('§') {
        Some("") => ("§", words.next()),
        Some(rest) => ("§", Some(rest)),
        None => (first, words.next()),
    };
    let label = label.trim_end_matches('.').to_lowercase();
    let id = id?.trim_end_matches(['.', ':', ',']);
    // An identifier: a number ("520", "4.1", "36-520"), a roman numeral or a
    // single letter, never a word ("Section Purpose").
    // A lowercase numeral is short ("iv"); a lowercase word of numeral
    // letters is a word ("civil").
    let roman = !id.is_empty()
        && (id.chars().all(|c| "IVXLC".contains(c))
            || (id.len() <= 4 && id.chars().all(|c| "ivxlc".contains(c))));
    let identifier = id.starts_with(|c: char| c.is_ascii_digit())
        || (id.len() == 1 && id.chars().all(|c| c.is_ascii_alphabetic()))
        || roman;
    ((label == "§" || SECTION_LABELS.contains(&label.as_str())) && identifier)
        .then(|| words.count())
}

/// An enumerator as a code or a plan prints one: `1.`, `a.`, `iv.`, `A)`,
/// `(2)`, `(b)`, followed by a space. Returns the marker's byte length,
/// without the space. The marker is kept in the output: in an ordinance it is
/// the address of the provision ("subsection (g)(1)"), not decoration.
fn ordered_marker_len(text: &str) -> Option<usize> {
    // A note's marker ("*", "**", "†", "‡", "¤", "§") is the reference the
    // table's cells point to; it is kept, like an enumerator.
    // Printed tight against its note too ("***Coastal Zone properties"). A
    // section sign before a number cites a section ("§ 12.21"), no note.
    let note = text.len() - text.trim_start_matches(['*', '†', '‡', '¤', '§']).len();
    let rest = &text[note..];
    let citation =
        text.starts_with('§') && rest.trim_start().starts_with(|c: char| c.is_ascii_digit());
    if note > 0
        && !citation
        && (rest.starts_with(' ') || rest.is_empty() || rest.starts_with(char::is_uppercase))
    {
        return Some(note);
    }
    // "(2)", or a legend's "[1]".
    let open = text.chars().next().filter(|c| matches!(c, '(' | '['));
    let body_start = usize::from(open.is_some());
    let body_len = text[body_start..]
        .char_indices()
        .find(|(_, c)| !c.is_ascii_alphanumeric())
        .map(|(i, _)| i)?;
    let body = &text[body_start..body_start + body_len];
    let is_number = (1..=3).contains(&body.len()) && body.bytes().all(|b| b.is_ascii_digit());
    let is_letter = body.len() == 1 && body.bytes().all(|b| b.is_ascii_alphabetic());
    let is_roman = (1..=6).contains(&body.len())
        && (body.bytes().all(|b| b"ivxlc".contains(&b))
            || body.bytes().all(|b| b"IVXLC".contains(&b)));
    if !(is_number || is_letter || is_roman) {
        return None;
    }
    let close_at = body_start + body_len;
    let close = text[close_at..].chars().next()?;
    let valid = match open {
        Some('(') => close == ')',
        Some(_) => close == ']',
        None => close == '.' || close == ')',
    };
    let marker_len = close_at + close.len_utf8();
    // A marker alone on its line ("(6)" above its sub-items) is still one.
    let rest = &text[marker_len..];
    (valid && (rest.starts_with(' ') || rest.is_empty())).then_some(marker_len)
}

/// A line opening with a list marker is a list item, never a table row, even
/// when an italic run heading it ("6. *Revocation*.") splits it into spans that
/// happen to line up with the next item's.
fn starts_with_list_marker(line: &TextLine) -> bool {
    is_list_item_start(line_to_text(line).trim())
}

/// The single definition of a list marker, shared by classification and
/// rendering: a bullet glyph, or `-` followed by a space. Returns the
/// marker's byte length.
fn list_marker_len(text: &str) -> Option<usize> {
    let mut chars = text.chars();
    let first = chars.next()?;
    match first {
        '•' | '●' | '◦' | '▪' | '■' | '–' | '—' | '·' => Some(first.len_utf8()),
        '-' if chars.next() == Some(' ') => Some(1),
        _ => None,
    }
}

// ── Step 6: Group lines into blocks ────────────────────────────────

fn group_into_blocks(lines: &[TextLine], tables: &[DetectedTable], body_size: f32) -> Vec<Block> {
    // Classification and style extraction rebuild each line's text, and the
    // lookahead loops below revisit lines; precompute both once per line.
    let kinds: Vec<LineKind> = lines
        .iter()
        .map(|line| classify_line(line, body_size))
        .collect();
    let styles: Vec<(f32, bool, bool)> = lines.iter().map(dominant_style).collect();
    let (left, right) = lines.iter().fold((f32::MAX, f32::MIN), |(l, r), line| {
        let (line_left, line_right) = spans_extent(&line.spans);
        (l.min(line_left), r.max(line_right))
    });
    let ends: Vec<bool> = lines
        .iter()
        .map(|line| ends_sentence_short(line, left, right))
        .collect();
    let ends_paragraph = |k: usize| ends[k];

    let mut blocks = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        if let Some((idx, table)) = tables.iter().enumerate().find(|(_, t)| t.contains_line(i)) {
            blocks.push(Block::Table { index: idx });
            i = table.end_line();
            continue;
        }

        let kind = &kinds[i];
        if matches!(kind, LineKind::Empty) {
            i += 1;
            continue;
        }

        // Lists need special handling: collect items + continuation lines.
        if matches!(kind, LineKind::ListItem) {
            let mut items: Vec<Vec<usize>> = vec![vec![i]];
            let mut j = i + 1;
            while j < lines.len() {
                if tables.iter().any(|t| t.contains_line(j)) {
                    break;
                }
                if !is_vertically_close(&lines[j - 1], &lines[j]) {
                    break;
                }
                match kinds[j] {
                    LineKind::ListItem => {
                        items.push(vec![j]);
                        j += 1;
                    }
                    LineKind::Paragraph if !ends_paragraph(j - 1) => {
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
            if tables.iter().any(|t| t.contains_line(j)) {
                break;
            }

            if !same_block_kind(kind, &kinds[j]) {
                break;
            }

            // A numbered heading starts a heading of its own ("Part 1.
            // General Provisions" then "101. Title"), and so does a section's
            // label ("Appendix B" then "SEC. 520 CENTER CITY ..."). A label
            // printed alone on its line is a line of its own: "Appendix C"
            // over "Bicycle Parking Design Guidelines" is how a code heads it,
            // and how a reader finds it.
            if matches!(kind, LineKind::Heading(_)) {
                let text = line_to_text(&lines[j]);
                if is_list_item_start(text.trim())
                    || starts_section_label(&text)
                    || is_bare_section_label(&line_to_text(&lines[j - 1]))
                {
                    break;
                }
            }

            if !is_vertically_close(&lines[j - 1], &lines[j]) {
                break;
            }

            if matches!(kind, LineKind::Paragraph)
                && (!same_style(styles[j - 1], styles[j]) || ends_paragraph(j - 1))
            {
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

/// Share of the column's width a line must stop short of to end its
/// paragraph when it also ends a sentence.
const SHORT_LINE: f32 = 0.25;

/// Whether a line ends a sentence well before the column's right edge
/// (`left`..`right`), so the next line starts something new: definitions or
/// notes printed one per line ("FAR – Floor Area Ratio – …" then "RFAR – …"),
/// or a paragraph after a note, which a wrapped line never is. A marker or a
/// label alone on its line ("i.", "Key:") is no sentence.
fn ends_sentence_short(line: &TextLine, left: f32, right: f32) -> bool {
    let (_, end) = spans_extent(&line.spans);
    if end >= right - (right - left) * SHORT_LINE {
        return false;
    }
    let text = line_to_text(line);
    text.trim_end()
        .trim_end_matches([')', ']', '"', '\u{201D}', '\u{2019}'])
        .ends_with(['.', ':', '!', '?'])
        && text.split_whitespace().nth(2).is_some()
}

/// The leftmost start and rightmost end of a set of spans.
fn spans_extent(spans: &[TextSpan]) -> (f32, f32) {
    spans.iter().fold((f32::MAX, f32::MIN), |(l, r), s| {
        (l.min(s.x), r.max(s.x + s.width))
    })
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

fn same_style(a: (f32, bool, bool), b: (f32, bool, bool)) -> bool {
    (a.0 - b.0).abs() < 1.0 && a.1 == b.1 && a.2 == b.2
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

fn lines_to_markdown(
    lines: &[TextLine],
    body_size: f32,
    detected_tables: Option<Vec<DetectedTable>>,
) -> String {
    if lines.is_empty() {
        return String::new();
    }

    let tables = detected_tables.unwrap_or_else(|| detect_tables(lines));
    let (resplit, tables) = resplit_tables(lines, tables);
    let lines = resplit.as_deref().unwrap_or(lines);
    let blocks = group_into_blocks(lines, &tables, body_size);

    let mut md = String::new();
    for block in &blocks {
        render_block(&mut md, block, lines, &tables);
    }

    // Leading spaces are a list's indentation: only blank lines are trimmed.
    collapse_blank_lines(&md)
        .trim_end()
        .trim_start_matches('\n')
        .to_string()
}

/// Re-splits the spans of each detected table's lines at narrow gaps and
/// detects the table again on them, keeping the new reading when it still
/// finds a table there. Lines outside tables are never re-split, so prose is
/// unaffected. Returns the replaced lines, if any changed, and the tables.
fn resplit_tables(
    lines: &[TextLine],
    tables: Vec<DetectedTable>,
) -> (Option<Vec<TextLine>>, Vec<DetectedTable>) {
    if tables.is_empty() {
        return (None, tables);
    }
    let mut out: Vec<TextLine> = lines
        .iter()
        .map(|l| TextLine {
            y: l.y,
            spans: l.spans.clone(),
        })
        .collect();
    let mut result = Vec::with_capacity(tables.len());
    for table in tables {
        let (start, end) = (table.start_line(), table.end_line());
        let original: Vec<Vec<TextSpan>> =
            out[start..end].iter().map(|l| l.spans.clone()).collect();
        for line in &mut out[start..end] {
            line.spans = line
                .spans
                .iter()
                .flat_map(|s| split_span(s, TABLE_SPAN_SPLIT_FACTOR))
                .collect();
        }
        let line_xs: Vec<Vec<f32>> = out
            .iter()
            .map(|l| l.spans.iter().map(|s| s.x).collect())
            .collect();
        let tolerance = pitch_tolerance(&line_xs[start..end]);
        match detect_table_at(&out, &line_xs, start, tolerance, true)
            .filter(|t| t.end_line() <= end)
        {
            Some(better) if better.columns.len() >= table.columns.len() => result.push(better),
            _ => {
                for (line, spans) in out[start..end].iter_mut().zip(original) {
                    line.spans = spans;
                }
                result.push(table);
            }
        }
    }
    (Some(out), result)
}

/// Column tolerance for a table whose columns are closer than the default
/// allows: under half its typical span-to-span pitch. The AIM Norman plan's
/// compatibility matrix has row labels 29.7 pt left of the first values, so
/// the default 30 pt tolerance read label and value as one column.
fn pitch_tolerance(line_xs: &[Vec<f32>]) -> f32 {
    let mut pitches: Vec<f32> = line_xs
        .iter()
        .flat_map(|xs| xs.windows(2).map(|w| w[1] - w[0]))
        .filter(|d| *d > 0.0)
        .collect();
    if pitches.is_empty() {
        return COLUMN_ANCHOR_TOLERANCE;
    }
    pitches.sort_by(f32::total_cmp);
    let median = pitches[pitches.len() / 2];
    COLUMN_ANCHOR_TOLERANCE.min(median * 0.45)
}

/// Limits consecutive newlines to two (one blank line) in a single pass.
fn collapse_blank_lines(md: &str) -> String {
    let mut collapsed = String::with_capacity(md.len());
    let mut newline_run = 0usize;

    for ch in md.chars() {
        if ch == '\n' {
            newline_run += 1;
            if newline_run > 2 {
                continue;
            }
        } else {
            newline_run = 0;
        }
        collapsed.push(ch);
    }

    collapsed
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
            let levels = list_levels(items, lines);
            for (item_lines, level) in items.iter().zip(levels) {
                let mut text = String::new();
                for (i, &idx) in item_lines.iter().enumerate() {
                    if i > 0 {
                        text.push(' ');
                    }
                    text.push_str(&line_to_text(&lines[idx]));
                }
                let text = text.trim();
                md.push_str(&"  ".repeat(level));
                match ordered_marker_len(text) {
                    Some(marker) => {
                        escape_markdown_into(md, &text[..marker]);
                        md.push(' ');
                        escape_markdown_into(md, text[marker..].trim_start());
                    }
                    None => {
                        md.push_str("- ");
                        escape_markdown_into(md, &strip_list_marker(text));
                    }
                }
                md.push('\n');
            }
            md.push('\n');
        }
        Block::Table { index } => {
            render_table_markdown(md, lines, &tables[*index]);
        }
    }
}

/// Each item's nesting depth: the rank of its left edge among the distinct
/// left edges of the list's items, so "1. Starting date;" printed under
/// "4. Any changes ..." reads as its sub-item.
fn list_levels(items: &[Vec<usize>], lines: &[TextLine]) -> Vec<usize> {
    const SAME_INDENT: f32 = 4.0;
    let xs: Vec<f32> = items
        .iter()
        .map(|item| lines[item[0]].spans.first().map_or(0.0, |s| s.x))
        .collect();
    let mut indents: Vec<f32> = xs.clone();
    indents.sort_by(f32::total_cmp);
    indents.dedup_by(|a, b| (*a - *b).abs() < SAME_INDENT);
    xs.iter()
        .map(|x| {
            indents
                .iter()
                .filter(|&&indent| indent < x - SAME_INDENT)
                .count()
        })
        .collect()
}

fn strip_list_marker(text: &str) -> String {
    match list_marker_len(text) {
        Some(marker_len) => text[marker_len..].trim_start().to_string(),
        None => text.to_string(),
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

/// A span prepared for inline rendering: `(span, needs_leading_space)`.
type InlineSpan<'a> = (&'a TextSpan, bool);

/// Flattens paragraph lines into render-ready spans: word spaces are decided
/// from horizontal gaps, line breaks become spaces, and a word broken at a
/// hyphen on a line break is joined, the hyphen kept. Nothing tells a
/// typesetter's break from a compound broken at its own hyphen, and in a
/// code the compound is the rule ("single-family", "off-street", "on-site");
/// dropping the hyphen writes "singlefamily", keeping it writes "exam-ple"
/// at worst, losing no letter.
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
                && let Some((previous, _)) = spans.last()
                && ends_with_soft_hyphen(&previous.text)
                && starts_lowercase(&span.text)
            {
                leading_space = false;
            }

            spans.push((span, leading_space));
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

    for (index, (span, leading_space)) in spans.iter().enumerate() {
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

        let text = span.text.as_str();

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

    let mut rows: Vec<Vec<String>> = Vec::with_capacity(table.rows.len());
    for row in &table.rows {
        let mut cells: Vec<String> = vec![String::new(); num_cols];
        // (previous span, its line index) per cell, to decide separators.
        let mut cell_state: Vec<Option<(&TextSpan, usize)>> = vec![None; num_cols];

        for &line_index in row {
            for span in &lines[line_index].spans {
                let col_idx = nearest_column(span.x, columns);
                let wants_space = match cell_state[col_idx] {
                    // Wrapped text from a later line joins with a space unless
                    // the line ended in a word broken at a hyphen; same-line
                    // neighbors join on the geometric gap rule.
                    Some((_, previous_line)) if previous_line != line_index => {
                        !regions::ends_broken_word(&cells[col_idx])
                    }
                    Some((previous, _)) => needs_space_between(previous, span),
                    None => false,
                };
                if wants_space && !cells[col_idx].is_empty() {
                    cells[col_idx].push(' ');
                }
                escape_markdown_into(&mut cells[col_idx], &span.text);
                cell_state[col_idx] = Some((span, line_index));
            }
        }
        rows.push(cells.iter().map(|c| c.trim().to_string()).collect());
    }

    // Markdown needs a header row. A table whose first row is data (terms and
    // their definitions) gets an empty one, so that its first entry is not
    // presented as the columns' names.
    if !regions::is_header_row(&rows[0]) {
        rows.insert(0, vec![String::new(); num_cols]);
    }
    regions::push_header(md, &rows[0]);
    for cells in &rows[1..] {
        regions::push_row(md, cells);
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
            vertical: false,
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
            glyphs: Vec::new(),
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
        assert_eq!(tables[0].start_line(), 0);
        assert_eq!(tables[0].end_line(), 3);
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
    fn complex_table_requires_wrapped_rows_and_width() {
        let wrapped_wide = detect_tables(&wrapped_table_lines());
        assert!(wrapped_wide.iter().any(is_complex_table));

        // Single-line rows: regular grid, reliable extraction.
        let simple = detect_tables(&table_lines());
        assert!(!simple.iter().any(is_complex_table));

        // Two columns with wrapped rows (footnotes / definition lists)
        // reconstruct reliably and must not be flagged.
        let narrow = DetectedTable {
            columns: vec![60.0, 150.0],
            rows: vec![vec![0, 1], vec![2]],
        };
        assert!(!is_complex_table(&narrow));
    }

    #[test]
    fn detect_tables_absorbs_wrapped_cell_lines() {
        let lines = wrapped_table_lines();
        let tables = detect_tables(&lines);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].start_line(), 0);
        assert_eq!(tables[0].end_line(), 5);
        assert_eq!(tables[0].rows, vec![vec![0, 1, 2], vec![3, 4]]);
    }

    #[test]
    fn markdown_merges_wrapped_cells_into_their_row() {
        let lines = wrapped_table_lines();
        let md = lines_to_markdown(&lines, estimate_body_font_size(&lines), None);
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
            tables[0].end_line(),
            5,
            "wide paragraph must not be swallowed into the table"
        );

        let md = lines_to_markdown(&lines, estimate_body_font_size(&lines), None);
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

    #[test]
    fn inline_formatting_escapes_markdown_characters() {
        let literal = styled_span("2 * 3 = [6]_note", 50.0, 700.0, 12.0, false);
        let mut md = String::new();
        render_spans_with_inline_formatting(&mut md, &[(&literal, false)]);
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
    fn paragraph_spans_join_a_word_broken_at_its_hyphen() {
        let lines = vec![
            text_line(700.0, vec![span("single-", 50.0, 700.0, 30.0, 12.0)]),
            text_line(
                685.0,
                vec![span("family dwellings", 50.0, 685.0, 80.0, 12.0)],
            ),
        ];
        let spans = paragraph_spans(&lines, &[0, 1]);
        assert!(!spans[1].1, "no space after the hyphen");

        let mut md = String::new();
        render_spans_with_inline_formatting(&mut md, &spans);
        assert_eq!(md, "single-family dwellings");
    }

    #[test]
    fn paragraph_spans_keep_hyphen_before_capitalized_words() {
        let lines = vec![
            text_line(700.0, vec![span("Jean-", 50.0, 700.0, 30.0, 12.0)]),
            text_line(685.0, vec![span("Paul spoke", 50.0, 685.0, 60.0, 12.0)]),
        ];
        let spans = paragraph_spans(&lines, &[0, 1]);
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
        render_spans_with_inline_formatting(&mut md, &[(&plain, false)]);
        assert_eq!(md, "\\# not a heading");

        let numbered = span("1. not a list", 50.0, 700.0, 70.0, 12.0);
        let mut md = String::new();
        render_spans_with_inline_formatting(&mut md, &[(&numbered, false)]);
        assert_eq!(md, "1\\. not a list");
    }

    #[test]
    fn styled_paragraph_start_needs_no_marker_escape() {
        let bold = styled_span("#tag", 50.0, 700.0, 12.0, true);
        let mut md = String::new();
        render_spans_with_inline_formatting(&mut md, &[(&bold, false)]);
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
        assert!(find_column_gutter(&lines).0.is_none());

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
        assert!(find_column_gutter(&lines).0.is_none());
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

        let md = lines_to_markdown(&lines, estimate_body_font_size(&lines), None);
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
        let md = lines_to_markdown(&lines, estimate_body_font_size(&lines), None);
        assert!(md.contains("| Name | Value |"), "missing header row: {md}");
        assert!(md.contains("| --- | --- |"), "missing separator: {md}");
        assert!(md.contains("| Alpha | 1 |"), "missing data row: {md}");
        assert!(md.contains("| Beta | 2 |"), "missing data row: {md}");
    }

    #[test]
    fn markdown_of_empty_lines_is_empty() {
        assert_eq!(lines_to_markdown(&[], 12.0, None), "");
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
        let md = lines_to_markdown(&lines, estimate_body_font_size(&lines), None);
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

    #[test]
    fn an_implausible_character_box_is_rebuilt_on_its_baseline() {
        // Juneau's 2013 plan: 10.5 pt body text in a 58 pt box.
        let (top, bottom) = plausible_box(759.0, 700.4, 10.5, 0.0, || Some(740.0));
        assert!((top - (740.0 + TYPICAL_ASCENT * 10.5)).abs() < 1e-3);
        assert!((bottom - (740.0 - TYPICAL_DESCENT * 10.5)).abs() < 1e-3);
        // An ordinary box is kept, and its baseline never asked for.
        assert_eq!(
            plausible_box(750.0, 736.0, 10.5, 0.0, || unreachable!()),
            (750.0, 736.0)
        );
        // Text drawn at an angle keeps its box: its baseline is no height.
        assert_eq!(
            plausible_box(759.0, 700.4, 10.5, 90.0, || Some(740.0)),
            (759.0, 700.4)
        );
    }

    #[test]
    fn a_section_label_starts_a_heading_and_a_title_word_does_not() {
        for text in [
            "SEC. 520 CENTER CITY PLANNED UNIT DEVELOPMENT",
            "Section 4.1 Purpose",
            "ARTICLE IV",
            "Appendix B",
            "§ 36-520 R-3, Multifamily Dwelling District",
            "§36-520",
            "Chapter 11 Land Use Maps",
            "Part iv.",
        ] {
            assert!(starts_section_label(text), "{text}");
        }
        for text in [
            "Appendix C",
            "CHAPTER 11",
            "Article IV.",
            "§36-520",
            "§ 36-520",
        ] {
            assert!(is_bare_section_label(text), "{text}");
        }
        for text in [
            "Appendix C Bicycle Parking",
            "SEC. 520 CENTER CITY",
            "Appendix",
        ] {
            assert!(!is_bare_section_label(text), "{text}");
        }
        for text in [
            "Section Purpose",
            "Chapter Eleven",
            "Land Use Maps",
            "Title of the plan",
            "Division civil engineering",
            "",
        ] {
            assert!(!starts_section_label(text), "{text}");
        }
    }

    /// The AIM Norman plan's land use page: labels down the left, each level
    /// with the first line of the paragraph it heads.
    fn side_head_page() -> Vec<TextLine> {
        let body = |y: f32, text: &str| span(text, 165.0, y, 370.0, 10.0);
        let label = |y: f32, text: &str| styled_span(text, 58.0, y, 10.0, true);
        vec![
            text_line(
                664.0,
                vec![
                    label(664.0, "DESCRIPTION"),
                    body(
                        664.0,
                        "Contains valuable environmental features that should not be",
                    ),
                ],
            ),
            text_line(
                652.0,
                vec![
                    label(652.0, "& CONTEXT"),
                    body(
                        652.0,
                        "developed or would make good recreational spaces for the city",
                    ),
                ],
            ),
            text_line(
                640.0,
                vec![body(
                    640.0,
                    "and the parks are developed with recreational features",
                )],
            ),
            text_line(
                548.0,
                vec![
                    label(548.0, "BUILDING TYPES"),
                    body(
                        548.0,
                        "small single-story structures designed to support the function",
                    ),
                ],
            ),
            text_line(
                536.0,
                vec![body(
                    536.0,
                    "of a recreation or a natural amenity in the area",
                )],
            ),
            text_line(
                506.0,
                vec![
                    label(506.0, "SITE DESIGN"),
                    body(
                        506.0,
                        "based around natural areas where the wetlands should be kept",
                    ),
                ],
            ),
            text_line(
                494.0,
                vec![body(
                    494.0,
                    "and native vegetation and habitats are to be preserved",
                )],
            ),
        ]
    }

    #[test]
    fn side_heads_are_read_each_before_the_paragraph_it_heads() {
        let lines = side_head_page();
        assert_eq!(
            side_head_rows(&lines, 150.0),
            Some(vec![664.0, 548.0, 506.0])
        );
        let columns = side_head_columns(lines, 150.0, &[664.0, 548.0, 506.0]);
        let firsts: Vec<String> = columns
            .iter()
            .map(|c| c.lines[0].spans[0].text.clone())
            .collect();
        assert_eq!(firsts[0], "DESCRIPTION");
        assert!(firsts[1].starts_with("Contains"));
        assert_eq!(firsts[2], "BUILDING TYPES");
        assert!(firsts[3].starts_with("small"));
        assert_eq!(firsts[4], "SITE DESIGN");
    }

    #[test]
    fn a_term_on_every_line_is_no_side_head_layout() {
        // A definitions column: a bold term on each line, no paragraph gaps.
        let lines: Vec<TextLine> = (0..6)
            .map(|i| {
                let y = 700.0 - i as f32 * 12.0;
                text_line(
                    y,
                    vec![
                        styled_span("TERM", 58.0, y, 10.0, true),
                        span(
                            "means the thing that the code defines in its text",
                            165.0,
                            y,
                            370.0,
                            10.0,
                        ),
                    ],
                )
            })
            .collect();
        assert_eq!(side_head_rows(&lines, 150.0), None);
    }

    #[test]
    fn a_column_of_headed_paragraphs_is_running_text() {
        // A plan's land use categories: a bold heading over each paragraph.
        let head = |y: f32, t: &str| styled_span(t, 65.0, y, 10.0, true);
        let body = |y: f32, t: &str| span(t, 65.0, y, 140.0, 9.0);
        let column = [
            head(340.0, "INSTITUTIONAL/PUBLIC | 5%"),
            body(324.0, "Facilities that are accessible to the"),
            body(313.0, "public, such as schools, churches,"),
            body(303.0, "and some medical facilities."),
            head(250.0, "PARKS & OPEN SPACE | 5%"),
            body(233.0, "Public park land, open space, and/or"),
            body(222.0, "recreational areas located outdoors,"),
            body(212.0, "courts."),
        ];
        assert!(is_running_text(&column.iter().collect::<Vec<_>>()));
        // A table's column of values stays a table, headed or not.
        let values = [
            head(340.0, "LOT AREA"),
            body(324.0, "6,000"),
            body(313.0, "7,500"),
            body(303.0, "10,000"),
        ];
        assert!(!is_running_text(&values.iter().collect::<Vec<_>>()));
    }

    fn turned(ch: char, y: f32) -> RawChar {
        // A glyph turned on its side: its box spans the line across (x) and
        // its advance along (y); pdfium reports no angle and size 1.
        RawChar {
            width: 19.8,
            height: 6.0,
            ..raw_char(ch, 16.7, y, 19.8, 1.0)
        }
    }

    #[test]
    fn a_title_running_up_the_margin_reads_upright() {
        let chars: Vec<RawChar> = "Plan"
            .chars()
            .enumerate()
            .map(|(i, ch)| turned(ch, 500.0 + i as f32 * 7.0))
            .collect();
        let upright = upright_sideways(chars);
        let text: String = upright.iter().map(|c| c.ch).collect();
        assert_eq!(text, "Plan");
        assert!(upright.iter().all(|c| c.y == upright[0].y && !c.vertical));
        assert!(upright.windows(2).all(|w| w[1].x > w[0].x));
        assert!(
            upright[0].font_size > 10.0,
            "size read from the line the box spans"
        );
        // Upright text is left as it is.
        let line = vec![
            raw_char('a', 10.0, 700.0, 6.0, 12.0),
            raw_char('b', 16.0, 700.0, 6.0, 12.0),
        ];
        assert_eq!(
            upright_sideways(line.clone())
                .iter()
                .map(|c| c.x)
                .collect::<Vec<_>>(),
            vec![10.0, 16.0]
        );
    }

    #[test]
    fn a_face_named_in_full_is_trusted_over_an_estimated_weight() {
        assert!(names_its_face("DAAAAA+Georgia"));
        assert!(names_its_face("FrutigerLTStd-Light"));
        assert!(!names_its_face("F1"));
        assert!(!names_its_face("T1_0"));
        assert!(!names_its_face("ABCDEF+TT2"));
    }

    #[test]
    fn enumerators_are_list_markers_and_prose_is_not() {
        for marker in [
            "1. Starting date;",
            "12. Items",
            "a. the rbl",
            "(2) Procedures",
            "(b) text",
            "iv. Roman",
            "A) Upper",
        ] {
            assert!(ordered_marker_len(marker).is_some(), "{marker}");
        }
        assert_eq!(ordered_marker_len("(2) Procedures"), Some(3));
        assert_eq!(ordered_marker_len("***Coastal Zone properties"), Some(3));
        assert_eq!(ordered_marker_len("§ Height limited"), Some(2));
        assert_eq!(ordered_marker_len("§ 12.21 Height"), None);
        assert_eq!(ordered_marker_len("[1] Mayor and Council"), Some(3));
        assert_eq!(ordered_marker_len("[See Section 131.0430]"), None);
        for prose in [
            "4.9.2.C and:",
            "U.S. Code",
            "No. 5 applies",
            "2024. The year",
            "(2 Procedures",
            "1.The",
            "Sec. 704",
        ] {
            assert!(ordered_marker_len(prose).is_none(), "{prose}");
        }
    }

    #[test]
    fn sentences_read_as_prose_and_table_rows_do_not() {
        assert!(is_prose_line(
            "for the non-addressed street frontage shall be 65%."
        ));
        assert!(is_prose_line(
            "1.The buildable area is delineated as anywhere behind the"
        ));
        assert!(is_prose_line(
            "March 2020 Correction: Corrected FAR/RFAR restrictions for R1, RA, RE and RS \
             properties in Height Districts 2, 3 and 4."
        ));
        for row in [
            "Office 100 percent 80 percent 100 percent",
            "Front yard 25 feet",
            "Lot area 6,000",
            "Single-family detached, one 1 1 1 1 1",
            "UL - 4 3 2 4 4 5",
            "Buildable Area",
        ] {
            assert!(!is_prose_line(row), "{row}");
        }
    }

    #[test]
    fn a_sentence_ending_short_of_the_column_ends_its_paragraph() {
        let full = text_line(
            700.0,
            vec![span(
                "Properties located in one of the zones are",
                50.0,
                700.0,
                450.0,
                10.0,
            )],
        );
        let short = text_line(
            688.0,
            vec![span(
                "FAR – Floor Area Ratio – defined.",
                50.0,
                688.0,
                200.0,
                10.0,
            )],
        );
        let marker = text_line(676.0, vec![span("i.", 50.0, 676.0, 10.0, 10.0)]);
        assert!(!ends_sentence_short(&full, 50.0, 500.0));
        assert!(ends_sentence_short(&short, 50.0, 500.0));
        assert!(!ends_sentence_short(&marker, 50.0, 500.0));
    }

    #[test]
    fn a_numbered_line_set_as_a_heading_is_a_heading() {
        let heading = text_line(
            700.0,
            vec![span("702. Use Table", 50.0, 700.0, 120.0, 16.0)],
        );
        let item = text_line(
            680.0,
            vec![span("1. The building line", 50.0, 680.0, 120.0, 10.0)],
        );
        assert!(matches!(
            classify_line(&heading, 10.0),
            LineKind::Heading(_)
        ));
        assert_eq!(classify_line(&item, 10.0), LineKind::ListItem);
    }

    #[test]
    fn a_sentence_closed_by_a_bracket_ends_short_too() {
        let line = text_line(
            700.0,
            vec![span(
                "(Amended 11-13-08 by O-19799 N.S; effective 12-13-2008.)",
                50.0,
                700.0,
                250.0,
                10.0,
            )],
        );
        assert!(ends_sentence_short(&line, 50.0, 500.0));
    }

    #[test]
    fn glyphs_on_their_side_are_vertical_on_an_upright_page() {
        assert!(is_vertical(90.0, PdfPageRenderRotation::None));
        assert!(is_vertical(270.0, PdfPageRenderRotation::None));
        assert!(!is_vertical(0.0, PdfPageRenderRotation::None));
        assert!(!is_vertical(90.0, PdfPageRenderRotation::Degrees90));
    }
}
