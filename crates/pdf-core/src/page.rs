//! Page plumbing shared by everything that opens a document.
//!
//! These are the conversions between this workspace's page model — one-based
//! and `u16`-bounded — and pdfium's zero-based `i32` indices, plus the two
//! calls every crate makes before it can do anything else. They live here
//! rather than beside the code that inspects a page because the extractors,
//! the cutter and the renderer all need them, and routing that through the
//! inspection crate would make document analysis a dependency of reading a
//! page's size.

use std::num::NonZeroU16;
use std::path::Path;

use pdfium_render::prelude::{
    PdfDocument, PdfPage, PdfPageObject, PdfPageObjectCommon, PdfPageObjectsCommon, PdfPageText,
    PdfPages, PdfRect, Pdfium,
};

use crate::error::{PdfError, Result};
use crate::text_quality::sanitize_extracted_text;

/// The part of the page a viewer shows, in the page's own (unrotated) user
/// space: the crop box, else the media box, else `0..width` × `0..height`.
///
/// A page box need not start at the origin — CAD exports centre it,
/// `[-595 -421 595 421]` — and every position pdfium reports is in that same
/// user space. `PdfPage::page_size()` is always anchored at the origin, so a
/// test against it drops everything left of or below `(0, 0)`: three
/// quarters of such a page.
pub fn visible_rect(page: &PdfPage<'_>) -> PdfRect {
    let boundaries = page.boundaries();
    match boundaries.crop().or_else(|_| boundaries.media()) {
        Ok(found) => {
            let (l, r) = (found.bounds.left().value, found.bounds.right().value);
            let (b, t) = (found.bounds.bottom().value, found.bounds.top().value);
            PdfRect::new_from_values(b.min(t), l.min(r), b.max(t), l.max(r))
        }
        Err(_) => page.page_size(),
    }
}

/// The page's text layer: everything inside [`visible_rect`], in document
/// order, sanitized — the one definition every extractor and the routing
/// signals read, so they judge the same text.
///
/// Use this, never `PdfPageText::all()` (clippy forbids it): that bounds the
/// text by `page_size()`, which silently truncates a page whose box is not
/// at the origin (see [`visible_rect`]).
pub fn visible_text(page: &PdfPage<'_>, text: &PdfPageText<'_>) -> String {
    sanitize_extracted_text(&text.inside_rect(visible_rect(page)))
}

/// Number of pages addressable by the workspace's `u16`-based page model.
/// Documents beyond `u16::MAX` pages (or negative pdfium error counts)
/// are rejected instead of silently truncated.
pub fn page_count_u16(pages: &PdfPages<'_>) -> Result<u16> {
    let page_count = pages.len();
    u16::try_from(page_count).map_err(|_| PdfError::UnsupportedPageCount { actual: page_count })
}

/// Converts a one-based page number to a pdfium zero-based page index.
pub fn page_index(page_number: NonZeroU16) -> i32 {
    i32::from(page_number.get()) - 1
}

/// Converts a zero-based destination index (bounded by the workspace's `u16`
/// page model) to a pdfium page index.
pub fn destination_page_index(index: u16) -> i32 {
    i32::from(index)
}

pub fn open_document<'pdfium>(
    pdfium: &'pdfium Pdfium,
    pdf_path: &Path,
) -> Result<PdfDocument<'pdfium>> {
    pdfium
        .load_pdf_from_file(pdf_path, None)
        .map_err(|source| PdfError::OpenPdf {
            path: pdf_path.to_path_buf(),
            source,
        })
}

/// A page's size in whole pixels at `scale`.
pub fn page_dimensions_px(page: &PdfPage<'_>, scale: f32) -> (u32, u32) {
    (
        scale_points_to_pixels(page.width().value, scale),
        scale_points_to_pixels(page.height().value, scale),
    )
}

fn scale_points_to_pixels(points: f32, scale: f32) -> u32 {
    (points * scale).round() as u32
}

/// The page's images: how many there are (inside Form XObjects too), and the
/// share of the page they cover, 0 to 1. Shared by page inspection's routing
/// and by structured extraction's test for a scanned page, so both judge a
/// page by one measure.
pub fn image_stats(page: &PdfPage<'_>) -> (u32, f32) {
    let page_area = page.width().value * page.height().value;
    let (count, area) = collect_image_stats(page.objects().iter(), true, page_area);
    let coverage = if page_area > 0.0 {
        (area / page_area as f64).min(1.0) as f32
    } else {
        0.0
    };
    (count, coverage)
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

    fn page(number: u16) -> NonZeroU16 {
        NonZeroU16::new(number).expect("non-zero page number")
    }

    #[test]
    fn one_based_page_numbers_become_zero_based_indices() {
        assert_eq!(page_index(page(1)), 0);
        assert_eq!(page_index(page(17)), 16);
        assert_eq!(page_index(page(u16::MAX)), i32::from(u16::MAX) - 1);
    }

    #[test]
    fn destination_indices_are_already_zero_based() {
        assert_eq!(destination_page_index(0), 0);
        assert_eq!(destination_page_index(41), 41);
    }

    #[test]
    fn pixel_dimensions_round_rather_than_truncate() {
        assert_eq!(scale_points_to_pixels(595.276, 1.0), 595);
        assert_eq!(scale_points_to_pixels(595.276, 2.0), 1191);
        assert_eq!(scale_points_to_pixels(841.89, 2.0), 1684);
    }
}
