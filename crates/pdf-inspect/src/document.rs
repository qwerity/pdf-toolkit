use crate::page_info::{LlmReason, PageInfo};
use pdf_core::error::{PdfError, Result};
use pdf_core::page::{
    image_stats, open_document, page_count_u16, page_dimensions_px, page_index, page_text,
};
use pdf_core::text_quality::is_probably_unreadable_text;
use pdf_text::page_has_complex_tables;
use pdfium_render::prelude::{PdfPage, Pdfium};
use std::num::NonZeroU16;
use std::path::Path;

/// Image coverage (fraction of page area) above which a page is flagged for
/// a vision-LLM pass: text inside embedded images is invisible to the text
/// layer.
const LLM_IMAGE_COVERAGE_THRESHOLD: f32 = 0.2;

/// Pages with fewer visible text characters than this — while carrying
/// images — are treated as having no usable text layer (scanned pages).
const LLM_MIN_TEXT_CHARS: usize = 20;

/// Detailed metadata for a PDF document.
pub struct DocumentInfo {
    pub page_count: u16,
    pub pages: Vec<PageInfo>,
}

/// Returns the page count for a PDF.
pub fn document_page_count(pdfium: &Pdfium, pdf_path: &Path) -> Result<u16> {
    page_count_u16(open_document(pdfium, pdf_path)?.pages())
}

/// Returns page count and per-page metadata for a PDF.
pub fn inspect_document(pdfium: &Pdfium, pdf_path: &Path) -> Result<DocumentInfo> {
    let document = open_document(pdfium, pdf_path)?;
    let pages = document.pages();
    let page_count = page_count_u16(pages)?;
    let mut page_info = Vec::with_capacity(usize::from(page_count));

    for page_number in (1..=page_count).filter_map(NonZeroU16::new) {
        let page = pages
            .get(page_index(page_number))
            .map_err(|source| PdfError::LoadPage {
                path: pdf_path.to_path_buf(),
                page_number: page_number.get(),
                source,
            })?;

        page_info.push(page_info_for_page(page_number.get(), &page));
    }

    Ok(DocumentInfo {
        page_count,
        pages: page_info,
    })
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

    let text = page
        .text()
        .map(|text| page_text(page, &text))
        .unwrap_or_default();

    let mut llm_reasons = llm_reasons(&text, image_count, image_coverage);
    // Only readable pages can be judged for table complexity; an unreadable
    // or empty text layer is already flagged by the reasons above.
    if !llm_reasons.contains(&LlmReason::UnreadableText) && page_has_complex_tables(page) {
        llm_reasons.push(LlmReason::ComplexTable);
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
/// that text extraction cannot, from the page's text layer and image stats.
fn llm_reasons(text: &str, image_count: u32, image_coverage: f32) -> Vec<LlmReason> {
    let mut reasons = Vec::new();
    let visible_chars = text.chars().filter(|ch| !ch.is_whitespace()).count();

    if visible_chars < LLM_MIN_TEXT_CHARS && image_count > 0 {
        reasons.push(LlmReason::NoTextLayer);
    }
    if image_coverage >= LLM_IMAGE_COVERAGE_THRESHOLD {
        reasons.push(LlmReason::ImageHeavy);
    }
    if is_probably_unreadable_text(text) {
        reasons.push(LlmReason::UnreadableText);
    }

    reasons
}

#[cfg(test)]
mod tests {
    use super::*;

    const READABLE_TEXT: &str = "This page carries a perfectly ordinary paragraph of extractable text \
         with plenty of readable words in it.";

    #[test]
    fn text_pages_are_not_flagged_for_llm() {
        assert!(llm_reasons(READABLE_TEXT, 0, 0.0).is_empty());
        assert!(llm_reasons(READABLE_TEXT, 2, 0.1).is_empty());
    }

    #[test]
    fn scanned_pages_have_no_text_layer() {
        assert_eq!(
            llm_reasons("", 1, 0.9),
            vec![LlmReason::NoTextLayer, LlmReason::ImageHeavy]
        );
        // Images plus a trivial caption still count as no usable text layer.
        assert!(llm_reasons("p. 4", 1, 0.05).contains(&LlmReason::NoTextLayer));
        // No images and no text: an empty page, not a scan.
        assert!(llm_reasons("", 0, 0.0).is_empty());
    }

    #[test]
    fn image_heavy_pages_are_flagged() {
        let reasons = llm_reasons(READABLE_TEXT, 2, 0.39);
        assert_eq!(reasons, vec![LlmReason::ImageHeavy]);
        assert!(llm_reasons(READABLE_TEXT, 2, 0.19).is_empty());
    }

    #[test]
    fn garbled_text_layers_are_flagged() {
        let garbled = "X9#Z 7Q$W 3K%T ".repeat(10);
        assert_eq!(
            llm_reasons(&garbled, 0, 0.0),
            vec![LlmReason::UnreadableText]
        );
    }

    #[test]
    fn llm_reason_slugs_are_stable() {
        assert_eq!(LlmReason::NoTextLayer.slug(), "no_text_layer");
        assert_eq!(LlmReason::ImageHeavy.slug(), "image_heavy");
        assert_eq!(LlmReason::UnreadableText.slug(), "unreadable_text");
        assert_eq!(LlmReason::ComplexTable.slug(), "complex_table");
    }
}
