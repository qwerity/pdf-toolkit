use crate::error::{PdfCutterError, Result};
use crate::output::PageInfo;
use pdfium_render::prelude::{
    PdfDocument, PdfPage, PdfPageObject, PdfPageObjectCommon, PdfPageObjectsCommon, PdfPages,
    Pdfium,
};
use std::num::NonZeroU16;
use std::path::Path;

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

/// Number of pages addressable by the crate's `u16`-based page model.
/// Documents beyond `u16::MAX` pages (or negative pdfium error counts)
/// are rejected instead of silently truncated.
pub(crate) fn page_count_u16(pages: &PdfPages<'_>) -> Result<u16> {
    let page_count = pages.len();
    u16::try_from(page_count)
        .map_err(|_| PdfCutterError::UnsupportedPageCount { actual: page_count })
}

/// Converts a one-based page number to a pdfium zero-based page index.
pub(crate) fn page_index(page_number: NonZeroU16) -> i32 {
    i32::from(page_number.get()) - 1
}

/// Converts a zero-based destination index (bounded by the crate's `u16`
/// page model) to a pdfium page index.
pub(crate) fn destination_page_index(index: u16) -> i32 {
    i32::from(index)
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
    let page_area = page.width().value * page.height().value;
    let (image_count, image_area) = collect_image_stats(page.objects().iter(), true, page_area);

    let image_coverage = if page_area > 0.0 {
        (image_area / page_area as f64).min(1.0) as f32
    } else {
        0.0
    };

    PageInfo {
        page_number,
        width_px,
        height_px,
        contains_images: image_count > 0,
        image_count,
        image_coverage,
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
