use crate::error::{PdfCutterError, Result};
use crate::output::PageInfo;
use pdfium_render::prelude::{PdfDocument, PdfPage, PdfPageObject, PdfPageObjectsCommon, Pdfium};
use std::path::Path;

/// Detailed metadata for a PDF document.
pub struct DocumentInfo {
    pub page_count: u16,
    pub pages: Vec<PageInfo>,
}

/// Returns the page count for a PDF.
pub fn document_page_count(pdfium: &Pdfium, pdf_path: &Path) -> Result<u16> {
    Ok(open_document(pdfium, pdf_path)?.pages().len())
}

/// Returns page count and per-page metadata for a PDF.
pub fn inspect_document(pdfium: &Pdfium, pdf_path: &Path) -> Result<DocumentInfo> {
    let document = open_document(pdfium, pdf_path)?;
    let pages = document.pages();
    let page_count = pages.len();
    let mut page_info = Vec::with_capacity(usize::from(page_count));

    for page_index in pages.as_range() {
        let page_number = page_index + 1;
        let page = pages
            .get(page_index)
            .map_err(|source| PdfCutterError::LoadPage {
                path: pdf_path.to_path_buf(),
                page_number,
                source,
            })?;

        page_info.push(page_info_for_page(page_number, &page));
    }

    Ok(DocumentInfo {
        page_count,
        pages: page_info,
    })
}

pub(crate) fn open_document<'pdfium>(
    pdfium: &'pdfium Pdfium,
    pdf_path: &Path,
) -> Result<PdfDocument<'pdfium>> {
    pdfium
        .load_pdf_from_file(pdf_path, None)
        .map_err(|source| PdfCutterError::OpenPdf {
            path: pdf_path.to_path_buf(),
            source,
        })
}

pub(crate) fn page_info_for_page(page_number: u16, page: &PdfPage<'_>) -> PageInfo {
    let (width_px, height_px) = page_dimensions_px(page, 1.0);

    PageInfo {
        page_number,
        width_px,
        height_px,
        contains_images: page_contains_images(page),
    }
}

pub(crate) fn page_dimensions_px(page: &PdfPage<'_>, scale: f32) -> (u32, u32) {
    (
        scale_points_to_pixels(page.width().value, scale),
        scale_points_to_pixels(page.height().value, scale),
    )
}

fn scale_points_to_pixels(points: f32, scale: f32) -> u32 {
    (points * scale).round() as u32
}

fn page_contains_images(page: &PdfPage<'_>) -> bool {
    page_object_summaries_contain_images(page.objects().iter().map(summarize_page_object))
}

fn page_object_summaries_contain_images(
    objects: impl IntoIterator<Item = PageObjectSummary>,
) -> bool {
    objects
        .into_iter()
        .any(|object| page_object_summary_contains_images(&object))
}

fn page_object_summary_contains_images(object: &PageObjectSummary) -> bool {
    match object {
        PageObjectSummary::Image => true,
        PageObjectSummary::Other => false,
        PageObjectSummary::Form(children) => {
            page_object_summaries_contain_images(children.iter().cloned())
        }
    }
}

fn summarize_page_object(object: PdfPageObject<'_>) -> PageObjectSummary {
    if object.as_image_object().is_some() {
        return PageObjectSummary::Image;
    }

    if let Some(form) = object.as_x_object_form_object() {
        return PageObjectSummary::Form(form.iter().map(summarize_page_object).collect());
    }

    PageObjectSummary::Other
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum PageObjectSummary {
    Other,
    Image,
    Form(Vec<PageObjectSummary>),
}
