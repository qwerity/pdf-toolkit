use crate::chart::has_vector_chart;
use crate::page_info::{LlmReason, PageInfo};
use pdf_core::error::Result;
use pdf_core::page::{
    DrawingStats, drawing_stats, image_stats, open_document, page_count_u16, page_dimensions_px,
    page_index, visible_text,
};
use pdf_core::text_quality::is_probably_unreadable_text;
use pdf_core::warning::ExtractionWarning;
use pdf_text::page_has_complex_tables;
use pdfium_render::prelude::{PdfPage, Pdfium};
use std::num::NonZeroU16;
use std::path::Path;

/// Image coverage (fraction of page area) above which a page is flagged for
/// a vision-LLM pass: text inside embedded images is invisible to the text
/// layer. A map, photo or diagram filling a tenth of the page already carries
/// content the text layer cannot see; on a routing benchmark, pages
/// between 10% and 20% needed a vision pass about as often as those above.
const LLM_IMAGE_COVERAGE_THRESHOLD: f32 = 0.1;

/// Pages with fewer visible text characters than this — while carrying
/// images — are treated as having no usable text layer (scanned pages).
const LLM_MIN_TEXT_CHARS: usize = 20;

/// Curved or diagonal paths (see [`DrawingStats::curved_paths`]) that make a
/// page a drawing — a map, diagram, chart or plan — whose meaning its text
/// layer's labels do not carry. Rounded boxes and a logo stay under it;
/// calibrated on a routing benchmark, where pages past it needed a
/// vision pass two to seven times as often as pages without line work.
const VECTOR_DRAWING_MIN_CURVED_PATHS: usize = 10;

/// Path objects above which a page is line work even when it is all straight
/// lines: a plan or a dense rectilinear diagram.
const VECTOR_DRAWING_MIN_PATHS: usize = 1000;

/// Path objects that make a page with no text and no images more than blank:
/// text drawn as outlines, or a drawing with no labels in its text layer.
const OUTLINED_CONTENT_MIN_PATHS: usize = 50;

/// Detailed metadata for a PDF document.
pub struct DocumentInfo {
    pub page_count: u16,
    /// One entry per page, in order — a page pdfium cannot load included,
    /// flagged [`LlmReason::PageUnloadable`].
    pub pages: Vec<PageInfo>,
    /// Pages that could not be loaded, with pdfium's reason.
    pub warnings: Vec<ExtractionWarning>,
}

/// Returns the page count for a PDF.
pub fn document_page_count(pdfium: &Pdfium, pdf_path: &Path) -> Result<u16> {
    page_count_u16(open_document(pdfium, pdf_path)?.pages())
}

/// Returns page count and per-page metadata for a PDF.
///
/// A page pdfium cannot load (a damaged page object or page tree) does not
/// fail the run: it is reported with [`LlmReason::PageUnloadable`], so a
/// caller routes it to a renderer that may tolerate the damage, and its
/// failure is in `warnings`.
pub fn inspect_document(pdfium: &Pdfium, pdf_path: &Path) -> Result<DocumentInfo> {
    let document = open_document(pdfium, pdf_path)?;
    let pages = document.pages();
    let page_count = page_count_u16(pages)?;
    let mut page_info = Vec::with_capacity(usize::from(page_count));
    let mut warnings = Vec::new();

    for page_number in (1..=page_count).filter_map(NonZeroU16::new) {
        let index = page_index(page_number);
        match pages.get(index) {
            Ok(page) => page_info.push(page_info_for_page(page_number.get(), &page)),
            Err(error) => {
                let (width_px, height_px) = pages.page_size(index).map_or((0, 0), |size| {
                    (
                        size.width().value.round() as u32,
                        size.height().value.round() as u32,
                    )
                });
                page_info.push(unloadable_page(page_number.get(), width_px, height_px));
                warnings.push(ExtractionWarning::PageUnloadable {
                    page_number: page_number.get(),
                    reason: error.to_string(),
                });
            }
        }
    }

    Ok(DocumentInfo {
        page_count,
        pages: page_info,
        warnings,
    })
}

/// The entry for a page pdfium could not load: nothing about it could be
/// read but its size, if the page tree still states one.
fn unloadable_page(page_number: u16, width_px: u32, height_px: u32) -> PageInfo {
    PageInfo {
        page_number,
        width_px,
        height_px,
        contains_images: false,
        image_count: 0,
        image_coverage: 0.0,
        llm_reasons: vec![LlmReason::PageUnloadable],
    }
}

pub fn page_info_for_page(page_number: u16, page: &PdfPage<'_>) -> PageInfo {
    page_info_and_text(page_number, page).0
}

/// [`page_info_for_page`], also returning the sanitized text it judged.
///
/// The routing signals are computed *from* the page's text, so a caller that
/// needs both — the dossier pipeline does — should not pay for a second
/// pdfium extraction of the same layer.
pub fn page_info_and_text(page_number: u16, page: &PdfPage<'_>) -> (PageInfo, String) {
    let (width_px, height_px) = page_dimensions_px(page, 1.0);
    let (image_count, image_coverage) = image_stats(page);
    let drawing = drawing_stats(page, VECTOR_DRAWING_MIN_CURVED_PATHS);

    let page_text = page.text().ok();
    let text = page_text
        .as_ref()
        .map(|text| visible_text(page, text))
        .unwrap_or_default();

    let mut llm_reasons = llm_reasons(&text, image_count, image_coverage, drawing);
    // Only readable pages can be judged for table complexity; an unreadable
    // or empty text layer is already flagged by the reasons above.
    if !llm_reasons.contains(&LlmReason::UnreadableText) && page_has_complex_tables(page) {
        llm_reasons.push(LlmReason::ComplexTable);
    }
    // A chart only changes where a page goes when nothing else has sent it to
    // a vision pass already: text pages, and table pages bound for the
    // text-assisted route. Other flagged pages skip the path walk.
    if drawing.paths > 0
        && llm_reasons.iter().all(|r| *r == LlmReason::ComplexTable)
        && has_vector_chart(page, page_text.as_ref())
    {
        llm_reasons.push(LlmReason::VectorChart);
    }

    let info = PageInfo {
        page_number,
        width_px,
        height_px,
        contains_images: image_count > 0,
        image_count,
        image_coverage,
        llm_reasons,
    };
    (info, text)
}

/// Determines why (if at all) a vision-LLM/OCR pass would recover content
/// that text extraction cannot, from the page's text layer, image stats and
/// vector line work.
fn llm_reasons(
    text: &str,
    image_count: u32,
    image_coverage: f32,
    drawing: DrawingStats,
) -> Vec<LlmReason> {
    let mut reasons = Vec::new();
    let visible_chars = text.chars().filter(|ch| !ch.is_whitespace()).count();
    let textless = visible_chars < LLM_MIN_TEXT_CHARS;

    if textless && image_count > 0 {
        reasons.push(LlmReason::NoTextLayer);
    }
    if image_coverage >= LLM_IMAGE_COVERAGE_THRESHOLD {
        reasons.push(LlmReason::ImageHeavy);
    }
    if is_probably_unreadable_text(text) {
        reasons.push(LlmReason::UnreadableText);
    }
    // A scan's content is already flagged above; outlines on a page with no
    // images are drawn content, which is what this reason is for.
    if drawing.curved_paths >= VECTOR_DRAWING_MIN_CURVED_PATHS
        || drawing.paths >= VECTOR_DRAWING_MIN_PATHS
        || (textless && image_count == 0 && drawing.paths >= OUTLINED_CONTENT_MIN_PATHS)
    {
        reasons.push(LlmReason::VectorDrawing);
    }

    reasons
}

#[cfg(test)]
mod tests {
    use super::*;

    const NO_DRAWING: DrawingStats = DrawingStats {
        paths: 0,
        curved_paths: 0,
    };

    fn drawing(paths: usize, curved_paths: usize) -> DrawingStats {
        DrawingStats {
            paths,
            curved_paths,
        }
    }

    const READABLE_TEXT: &str = "This page carries a perfectly ordinary paragraph of extractable text \
         with plenty of readable words in it.";

    #[test]
    fn text_pages_are_not_flagged_for_llm() {
        assert!(llm_reasons(READABLE_TEXT, 0, 0.0, NO_DRAWING).is_empty());
        assert!(llm_reasons(READABLE_TEXT, 2, 0.05, NO_DRAWING).is_empty());
        // A ruled table's lines and a few rounded boxes are not a drawing.
        assert!(llm_reasons(READABLE_TEXT, 0, 0.0, drawing(400, 9)).is_empty());
    }

    #[test]
    fn scanned_pages_have_no_text_layer() {
        assert_eq!(
            llm_reasons("", 1, 0.9, NO_DRAWING),
            vec![LlmReason::NoTextLayer, LlmReason::ImageHeavy]
        );
        // Images plus a trivial caption still count as no usable text layer.
        assert!(llm_reasons("p. 4", 1, 0.05, NO_DRAWING).contains(&LlmReason::NoTextLayer));
        // No images and no text: an empty page, not a scan.
        assert!(llm_reasons("", 0, 0.0, NO_DRAWING).is_empty());
        // A scan with a few vector marks on it is still only a scan.
        assert_eq!(
            llm_reasons("", 1, 0.9, drawing(60, 0)),
            vec![LlmReason::NoTextLayer, LlmReason::ImageHeavy]
        );
    }

    #[test]
    fn image_heavy_pages_are_flagged() {
        let reasons = llm_reasons(READABLE_TEXT, 2, 0.39, NO_DRAWING);
        assert_eq!(reasons, vec![LlmReason::ImageHeavy]);
        assert_eq!(
            llm_reasons(READABLE_TEXT, 1, 0.1, NO_DRAWING),
            vec![LlmReason::ImageHeavy]
        );
        assert!(llm_reasons(READABLE_TEXT, 2, 0.09, NO_DRAWING).is_empty());
    }

    #[test]
    fn garbled_text_layers_are_flagged() {
        let garbled = "X9#Z 7Q$W 3K%T ".repeat(10);
        assert_eq!(
            llm_reasons(&garbled, 0, 0.0, NO_DRAWING),
            vec![LlmReason::UnreadableText]
        );
    }

    #[test]
    fn drawings_are_flagged_by_their_line_work() {
        // A map, diagram or chart: labels in the text layer, meaning in the
        // curved and diagonal paths.
        assert_eq!(
            llm_reasons(
                READABLE_TEXT,
                0,
                0.0,
                drawing(40, VECTOR_DRAWING_MIN_CURVED_PATHS)
            ),
            vec![LlmReason::VectorDrawing]
        );
        // A plan drawn in straight lines only, dense enough to be line work.
        assert_eq!(
            llm_reasons(READABLE_TEXT, 0, 0.0, drawing(VECTOR_DRAWING_MIN_PATHS, 0)),
            vec![LlmReason::VectorDrawing]
        );
        assert!(
            llm_reasons(
                READABLE_TEXT,
                0,
                0.0,
                drawing(VECTOR_DRAWING_MIN_PATHS - 1, 0)
            )
            .is_empty()
        );
        // Text drawn as outlines: no text layer, no images, real line work.
        assert_eq!(
            llm_reasons("", 0, 0.0, drawing(OUTLINED_CONTENT_MIN_PATHS, 0)),
            vec![LlmReason::VectorDrawing]
        );
        // A blank page with a border and a rule or two stays unflagged.
        assert!(llm_reasons("", 0, 0.0, drawing(OUTLINED_CONTENT_MIN_PATHS - 1, 0)).is_empty());
    }

    #[test]
    fn an_unloadable_page_is_reported_for_a_vision_pass() {
        let page = unloadable_page(3, 1224, 792);
        assert_eq!(page.llm_reasons, vec![LlmReason::PageUnloadable]);
        assert!(page.llm_recommended());
        assert_eq!(
            (page.width_px, page.height_px, page.image_count),
            (1224, 792, 0)
        );
    }

    #[test]
    fn llm_reason_slugs_are_stable() {
        assert_eq!(LlmReason::NoTextLayer.slug(), "no_text_layer");
        assert_eq!(LlmReason::ImageHeavy.slug(), "image_heavy");
        assert_eq!(LlmReason::UnreadableText.slug(), "unreadable_text");
        assert_eq!(LlmReason::ComplexTable.slug(), "complex_table");
        assert_eq!(LlmReason::VectorDrawing.slug(), "vector_drawing");
        assert_eq!(LlmReason::PageUnloadable.slug(), "page_unloadable");
        assert_eq!(LlmReason::VectorChart.slug(), "vector_chart");
    }
}
