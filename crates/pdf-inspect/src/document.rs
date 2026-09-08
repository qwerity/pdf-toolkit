use crate::page_info::{LlmReason, PageInfo};
use pdf_core::error::{PdfCutterError, Result};
use pdf_core::page::{open_document, page_count_u16, page_dimensions_px, page_index};
use pdf_core::text_quality::{is_probably_unreadable_text, sanitize_extracted_text};
use pdf_text::page_has_complex_tables;
use pdfium_render::prelude::{
    PdfPage, PdfPageObject, PdfPageObjectCommon, PdfPageObjectsCommon, Pdfium,
};
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
        let page =
            pages
                .get(page_index(page_number))
                .map_err(|source| PdfCutterError::LoadPage {
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
    let (width_px, height_px) = page_dimensions_px(page, 1.0);
    let page_area = page.width().value * page.height().value;
    let (image_count, image_area) = collect_image_stats(page.objects().iter(), true, page_area);

    let image_coverage = if page_area > 0.0 {
        (image_area / page_area as f64).min(1.0) as f32
    } else {
        0.0
    };

    let text = page
        .text()
        .map(|text| sanitize_extracted_text(&text.all()))
        .unwrap_or_default();

    let mut llm_reasons = llm_reasons(&text, image_count, image_coverage);
    // Only readable pages can be judged for table complexity; an unreadable
    // or empty text layer is already flagged by the reasons above.
    if !llm_reasons.contains(&LlmReason::UnreadableText) && page_has_complex_tables(page) {
        llm_reasons.push(LlmReason::ComplexTable);
    }

    PageInfo {
        page_number,
        width_px,
        height_px,
        contains_images: image_count > 0,
        image_count,
        image_coverage,
        llm_reasons,
    }
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

/// Recursively count image objects and accumulate their bounding-box area.
///
/// `include_bounds` is true for top-level page objects (where bounds are in
/// page-space) and false for objects inside Form XObjects (whose bounds are in
/// the form's local coordinate space, making page-space coverage unreliable).
fn collect_image_stats<'a>(
    objects: impl IntoIterator<Item = PdfPageObject<'a>>,
    include_bounds: bool,
    page_area: f32,
) -> (u32, f64) {
    let mut count: u32 = 0;
    let mut area: f64 = 0.0;

    for object in objects {
        if object.as_image_object().is_some() {
            count += 1;
            if include_bounds && let Ok(bounds) = object.bounds() {
                let w = (bounds.right().value - bounds.left().value).abs() as f64;
                let h = (bounds.top().value - bounds.bottom().value).abs() as f64;
                // Guard against degenerate/huge values (e.g. malformed XObjects).
                let max_dim = page_area.sqrt() as f64 * 4.0;
                if w < max_dim && h < max_dim {
                    area += w * h;
                }
            }
        } else if let Some(form) = object.as_x_object_form_object() {
            // Recurse into Form XObjects for the count, but skip bounds
            // (they are in the form's local coordinate space).
            let (nested_count, _) = collect_image_stats(form.iter(), false, page_area);
            count += nested_count;
        }
    }

    (count, area)
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
