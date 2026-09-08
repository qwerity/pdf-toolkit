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

use pdfium_render::prelude::{PdfDocument, PdfPage, PdfPages, Pdfium};

use crate::error::{PdfError, Result};

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
