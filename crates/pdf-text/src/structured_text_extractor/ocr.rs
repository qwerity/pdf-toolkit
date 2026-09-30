//! Reading a page by OCR instead of its text layer.
//!
//! A scanned page is one picture of the page. Its text layer is empty, or is
//! the scanner's own hidden OCR, which garbles digits ("32" read as "23") and
//! spaces letters apart. Such a page is rendered and read by the OCR engine,
//! and the page reader then works on what the picture shows exactly as it
//! works on a digital page: each OCR line becomes characters placed along its
//! box, and the rules printed in the picture (a table's grid) are found in its
//! pixels and stand in for the page's vector drawing. A scanned table is then
//! read from its grid, merged cells and headers included.

use image::GrayImage;
use pdf_core::page::image_stats;
use pdf_core::text_quality::is_vowelled_word;
use pdf_ocr::{Ocr, OcrLine};
use pdf_render::{PngSize, render_page};
use pdfium_render::prelude::{PdfPage, PdfPageObjectsCommon, PdfPageText, PdfPageTextRenderMode};

use super::graphics::{self, Rule};
use super::regions::TableTail;
use super::{RawChar, page_markdown_drawn, spread_chars};

/// Share of the page its images must cover for the page to be a scan.
const SCAN_IMAGE_COVERAGE: f32 = 0.6;

/// Visible characters below which a page has no text layer of its own.
const SCAN_MIN_VISIBLE_CHARS: usize = 20;

/// Share of the page a picture drawn under everything else must cover for
/// the text over it to be a scanner's OCR overlay rather than the page's own.
const SCAN_OVERLAY_COVERAGE: f32 = 0.9;

/// Render resolution for OCR, in dots per inch, up to the longest side the
/// engine reads at ([`Ocr::max_side`]); it shrinks anything larger itself.
const OCR_DPI: f32 = 300.0;

/// Share of a line box's height the type fills.
const OCR_TYPE_HEIGHT: f32 = 0.75;

/// Luminance below which a pixel is ink.
const INK: u8 = 140;

/// Shortest run of ink, in points, that is a rule rather than a stroke of a
/// letter. Longer than a vector rule's minimum: a scan's letters are ink too.
const RASTER_RULE_MIN_LENGTH: f32 = 20.0;

/// Thickest printed rule, in points: anything thicker is a filled bar.
const RASTER_RULE_MAX_THICKNESS: f32 = 3.0;

/// Rows a scanned line may drift across per pixel of its length: a scan is
/// rarely square to the page, and a long rule crosses several pixel rows as
/// it runs (about 1 degree).
const SKEW_SLOPE: f32 = 0.02;

/// Whether the page is a scan: images covering most of it (one picture, or
/// the strips a scanner tiles it into), and no text of its own to read
/// (none, only invisible text laid over the picture, or garbled text) — or a
/// picture of the whole page drawn first with the text over it, as a
/// scanner lays a visible OCR overlay on its scan. For the overlay the text
/// layer is not discarded: the page reader weighs it against its own OCR and
/// keeps the reading with more real words (see [`reads_better`]), so a
/// digital page on a full-page background keeps its text.
pub(super) fn is_scanned(page: &PdfPage<'_>, text: &PdfPageText<'_>, unreadable: bool) -> bool {
    let coverage = image_stats(page).1;
    if coverage < SCAN_IMAGE_COVERAGE {
        return false;
    }
    if unreadable || (coverage >= SCAN_OVERLAY_COVERAGE && drawn_over_a_picture(page)) {
        return true;
    }
    let visible = text
        .chars()
        .iter()
        .filter(|c| c.unicode_char().is_some_and(|ch| !ch.is_whitespace()))
        // pdfium reports no mode for characters it generates itself (spaces,
        // line breaks); only a known visible mode counts.
        .filter(|c| {
            c.render_mode()
                .is_ok_and(|mode| mode != PdfPageTextRenderMode::Invisible)
        })
        .take(SCAN_MIN_VISIBLE_CHARS)
        .count();
    visible < SCAN_MIN_VISIBLE_CHARS
}

/// Whether the first thing the page draws is a picture, so that everything
/// else — its text included — lies on top of it. A scanner that wraps its
/// scan and overlay in one form draws the form first, the picture first
/// within it.
fn drawn_over_a_picture(page: &PdfPage<'_>) -> bool {
    let Some(first) = page.objects().iter().next() else {
        return false;
    };
    first.as_image_object().is_some()
        || first
            .as_x_object_form_object()
            .and_then(|form| form.iter().next())
            .is_some_and(|inner| inner.as_image_object().is_some())
}

/// The page read by OCR, as markdown. `carry` works as for a digital page: a
/// scanned table continued from the page before keeps its values.
///
/// # Errors
/// When the page cannot be rendered or the OCR engine fails on it.
pub(super) fn ocr_markdown(
    page: &PdfPage<'_>,
    engine: &mut Ocr,
    carry: &mut Option<TableTail>,
) -> anyhow::Result<String> {
    let (width, height) = (page.width().value, page.height().value);
    let scale = (OCR_DPI / 72.0).min(engine.max_side() as f32 / width.max(height));
    let bitmap = render_page(page, PngSize::Scale(scale))?;
    let image = bitmap.as_image()?.to_rgb8();
    // The render is the page as displayed, `/Rotate` applied: points are its
    // pixels over the scale, measured from the top.
    let points = image.height() as f32 / scale;
    let lines = engine.read(&image)?;
    let chars: Vec<RawChar> = lines
        .iter()
        .flat_map(|line| line_chars(line, scale, points))
        .collect();
    // A page's own vector drawing is exact where it has one (a digital page
    // read with `--ocr all`); a scan's rules are only in its pixels.
    let mut drawing = graphics::collect(page);
    if drawing.rules.is_empty() {
        let luma = image::imageops::grayscale(&image);
        drawing.rules = raster_rules(&luma, scale, points);
    }
    let visible = graphics::Rect {
        left: 0.0,
        bottom: 0.0,
        right: image.width() as f32 / scale,
        top: points,
    };
    Ok(page_markdown_drawn(
        page,
        chars,
        Some(visible),
        &drawing,
        carry,
    ))
}

/// Words a text layer must read before it can win over OCR at all.
const MIN_LAYER_WORDS: usize = 20;

/// Share of OCR's words a text layer must hold too, so a layer covering only
/// a corner of the page does not win on the quality of that corner.
const MIN_LAYER_WORD_SHARE: f32 = 0.8;

/// Longest run of letters that is still one word: longer is words run
/// together ("Supportingcommunity"), as OCR reads a tight italic.
const MAX_WORD_LETTERS: usize = 12;

/// Whether a page's text layer reads better than OCR's reading of it: at
/// least as large a share of its tokens are words, and nearly as many words.
/// A scanner's hidden OCR can be good (the Juneau parks plan's, whose italics
/// the engine runs together; a Tucson brochure laid sideways, which the
/// engine reads letter by letter) or garbage (the Lafayette arterial plan's,
/// scattering letters and swapping digits); the words tell them apart.
pub(super) fn reads_better(text_layer: &str, ocr: &str) -> bool {
    let (layer_words, layer_tokens) = word_counts(text_layer);
    let (ocr_words, ocr_tokens) = word_counts(ocr);
    layer_words >= MIN_LAYER_WORDS
        && layer_words as f32 >= ocr_words as f32 * MIN_LAYER_WORD_SHARE
        && layer_words * ocr_tokens >= ocr_words * layer_tokens
}

/// Words and tokens in a page's markdown, markup aside. A word is 3 to
/// [`MAX_WORD_LETTERS`] letters; a Latin one also carries a vowel, which
/// letters from a broken font or a misread line mostly do not. Single
/// letters, fragments and symbols are tokens that are not words.
fn word_counts(markdown: &str) -> (usize, usize) {
    let mut words = 0;
    let mut tokens = 0;
    for token in markdown.split(|c: char| c.is_whitespace() || matches!(c, '|' | '*' | '#' | '\\'))
    {
        let token = token.trim_matches(|c: char| c.is_ascii_punctuation());
        if token.is_empty() {
            continue;
        }
        tokens += 1;
        let word = (3..=MAX_WORD_LETTERS).contains(&token.chars().count())
            && token.chars().all(char::is_alphabetic)
            && is_vowelled_word(token);
        words += usize::from(word);
    }
    (words, tokens)
}

/// An OCR line as characters: its box in page points, y growing upward, the
/// type size read from the box height, and its characters spread evenly
/// across the box (OCR places lines, not glyphs).
fn line_chars(line: &OcrLine, scale: f32, page_height: f32) -> Vec<RawChar> {
    let [left, top, right, bottom] = line.bounds.map(|v| v / scale);
    let height = (bottom - top).max(0.0);
    let text = line.text.trim();
    let pitch = (right - left).max(0.0) / text.chars().count().max(1) as f32;
    spread_chars(
        text,
        left,
        page_height - top,
        pitch,
        height,
        height * OCR_TYPE_HEIGHT,
    )
}

/// The rules printed in a page picture: runs of ink long enough to be lines,
/// thin enough not to be bars, in page points.
fn raster_rules(luma: &GrayImage, scale: f32, page_height: f32) -> Vec<Rule> {
    let (w, h) = (luma.width() as usize, luma.height() as usize);
    let ink: Vec<bool> = luma.pixels().map(|p| p.0[0] < INK).collect();
    let min_run = (RASTER_RULE_MIN_LENGTH * scale) as usize;
    let max_thick = (RASTER_RULE_MAX_THICKNESS * scale).ceil() as usize;
    let rows = lines_in(&ink, w, h, min_run, max_thick, true);
    let cols = lines_in(&ink, w, h, min_run, max_thick, false);
    let to_points = |v: f32| v / scale;
    rows.into_iter()
        .map(|(pos, start, end)| Rule {
            horizontal: true,
            pos: page_height - to_points(pos),
            start: to_points(start),
            end: to_points(end),
        })
        .chain(cols.into_iter().map(|(pos, start, end)| Rule {
            horizontal: false,
            pos: to_points(pos),
            start: page_height - to_points(end),
            end: page_height - to_points(start),
        }))
        .collect()
}

/// Lines of ink along rows (`horizontal`) or columns, as (centre across,
/// start along, end along) in pixels. Runs at least `min_run` long in
/// neighbouring rows that overlap join one line, which a skewed scan draws
/// over several rows; a line thicker than `max_thick` plus that drift is a
/// bar and dropped.
fn lines_in(
    ink: &[bool],
    w: usize,
    h: usize,
    min_run: usize,
    max_thick: usize,
    horizontal: bool,
) -> Vec<(f32, f32, f32)> {
    let (across, along) = if horizontal { (h, w) } else { (w, h) };
    let at = |a: usize, b: usize| {
        if horizontal {
            ink[a * w + b]
        } else {
            ink[b * w + a]
        }
    };
    // Open lines: (first index across, last index across, start, end).
    let mut open: Vec<(usize, usize, usize, usize)> = Vec::new();
    let mut done: Vec<(usize, usize, usize, usize)> = Vec::new();
    for a in 0..across {
        let mut runs = Vec::new();
        let mut b = 0;
        while b < along {
            if at(a, b) {
                let start = b;
                while b < along && at(a, b) {
                    b += 1;
                }
                if b - start >= min_run {
                    runs.push((start, b));
                }
            } else {
                b += 1;
            }
        }
        let mut next = Vec::with_capacity(open.len());
        for line in open.drain(..) {
            if let Some(i) = runs.iter().position(|&(s, e)| s < line.3 && e > line.2) {
                let (s, e) = runs.swap_remove(i);
                next.push((line.0, a, line.2.min(s), line.3.max(e)));
            } else {
                done.push(line);
            }
        }
        next.extend(runs.into_iter().map(|(s, e)| (a, a, s, e)));
        open = next;
    }
    done.extend(open);
    done.into_iter()
        .filter(|&(first, last, s, e)| {
            (last - first) as f32 <= max_thick as f32 + (e - s) as f32 * SKEW_SLOPE
        })
        .map(|(first, last, s, e)| ((first + last) as f32 / 2.0, s as f32, e as f32))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ocr_line_becomes_characters_placed_in_page_points() {
        let line = OcrLine {
            text: "WB 32".into(),
            bounds: [100.0, 50.0, 200.0, 90.0],
        };
        let chars = line_chars(&line, 2.0, 792.0);
        assert_eq!(chars.iter().map(|c| c.ch).collect::<String>(), "WB32");
        assert_eq!(
            (chars[0].x, chars[0].y, chars[0].height),
            (50.0, 767.0, 20.0)
        );
        assert!(chars[2].space_before && !chars[1].space_before);
    }

    #[test]
    fn a_clean_text_layer_reads_better_than_words_run_together() {
        let layer = "Continue working with community partners to provide accessible facilities \
                     and adaptive programming for seniors and residents with disabilities while \
                     managing demands for facility time fairly";
        let ocr = "Continueworking with communitypartners to provide accessible facilities \
                   and adaptiveprogramming for seniorsandresidents withdisabilities while \
                   managingdemands for facilitytime fairly";
        assert!(reads_better(layer, ocr));
        assert!(!reads_better(ocr, layer));
    }

    #[test]
    fn a_scattered_text_layer_loses_to_ocr() {
        let layer = "Arterial Master Plan C;t, of Lafayette south leg SH 287lPublic Road e | l | i \
                     | m | i | n | a | t | e | d t or the f — he exclusive pedestrian phase \
                     could be SH 287/SH 42 split phasing";
        let ocr = "Arterial Master Plan City of Lafayette SH 287/Public Road The sidewalk across \
                   the south leg was moved to the north leg so that the exclusive pedestrian \
                   phase could be eliminated SH 287/SH 42 The split phasing for the eastbound \
                   and westbound movements was eliminated";
        assert!(!reads_better(layer, ocr));
    }

    #[test]
    fn a_ruled_box_in_pixels_is_four_rules_and_text_is_none() {
        // A 100 x 60 px box drawn 2 px thick, a short stroke inside it.
        let mut image = GrayImage::from_pixel(200, 120, image::Luma([255]));
        for x in 20..120 {
            for t in 0..2 {
                image.put_pixel(x, 20 + t, image::Luma([0]));
                image.put_pixel(x, 80 + t, image::Luma([0]));
            }
        }
        for y in 20..82 {
            for t in 0..2 {
                image.put_pixel(20 + t, y, image::Luma([0]));
                image.put_pixel(118 + t, y, image::Luma([0]));
            }
        }
        for x in 50..58 {
            image.put_pixel(x, 50, image::Luma([0]));
        }
        let rules = raster_rules(&image, 1.0, 120.0);
        assert_eq!(rules.iter().filter(|r| r.horizontal).count(), 2);
        assert_eq!(rules.iter().filter(|r| !r.horizontal).count(), 2);
    }
}
