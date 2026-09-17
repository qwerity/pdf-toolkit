//! Rasterising a PDF page to PNG, through PDFium.
//!
//! This is deliberately its own crate rather than a corner of the page cutter,
//! because rendering is the capability with the widest reach: anything that
//! needs to *look* at a page — a vision model, a side-by-side comparison, a
//! thumbnail — needs pixels, and none of that needs the cutter.
//!
//! Sizing is a closed set ([`PngSize`]) rather than a free DPI number so that
//! the caller states the constraint it actually has: a fixed scale factor, or
//! a fixed output width. A renderer bounding the *longest* edge — what a
//! landscape sheet needs, and what Poppler's `pdftoppm -scale-to` provides —
//! is the natural third variant, and adding it means one arm in each `match`
//! below.

mod limits;

use std::io::Write;
use std::path::Path;

use pdfium_render::prelude::{PdfBitmap, PdfColor, PdfPage, PdfRenderConfig};

use pdf_core::error::{PdfError, Result};
use pdf_core::page::page_dimensions_px;

/// Widths beyond this cannot reach PDFium, whose render target is `i32`.
pub const MAX_IMAGE_WIDTH_PX: u32 = i32::MAX as u32;

/// How large to render a page.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum PngSize {
    /// Multiply the page's own dimensions by this factor.
    Scale(f32),
    /// Produce exactly this width in pixels, preserving the aspect ratio.
    WidthPx(u32),
}

impl PngSize {
    /// Rejects sizes PDFium cannot render, so a library caller that bypasses
    /// the CLI's own parsing gets an error rather than a panic at render time.
    pub fn validate(self) -> Result<()> {
        match self {
            Self::Scale(scale) if scale.is_finite() && scale > 0.0 => Ok(()),
            Self::Scale(scale) => Err(PdfError::InvalidImageScale { value: scale }),
            Self::WidthPx(width_px) if (1..=MAX_IMAGE_WIDTH_PX).contains(&width_px) => Ok(()),
            Self::WidthPx(width_px) => Err(PdfError::InvalidImageWidth {
                value: width_px,
                max: MAX_IMAGE_WIDTH_PX,
            }),
        }
    }
}

/// The pixel dimensions [`render_page`] will produce for this page and size.
///
/// Computed rather than measured, so a caller can report or lay out an image
/// without paying to rasterise it.
pub fn dimensions_px(page: &PdfPage<'_>, size: PngSize) -> (u32, u32) {
    match size {
        PngSize::Scale(scale) => page_dimensions_px(page, scale),
        PngSize::WidthPx(width_px) => {
            let height_px =
                ((page.height().value * width_px as f32) / page.width().value).round() as u32;

            (width_px, height_px)
        }
    }
}

/// Rasterises a page onto a white background.
///
/// The background is set explicitly because PDFium renders an unpainted page
/// transparent, and a transparent PNG of a line drawing is unreadable wherever
/// it is later composited onto something dark.
pub fn render_page<'page>(page: &'page PdfPage<'page>, size: PngSize) -> Result<PdfBitmap<'page>> {
    size.validate()?;
    limits::check(page.width().value, page.height().value, size)?;

    let render_config = match size {
        PngSize::Scale(scale) => PdfRenderConfig::new().scale_page_by_factor(scale),
        PngSize::WidthPx(width_px) => PdfRenderConfig::new().set_target_width(
            i32::try_from(width_px).expect("invariant: validate() bounds width by i32::MAX"),
        ),
    }
    .set_clear_color(PdfColor::WHITE);

    page.render_with_config(&render_config)
        .map_err(|source| PdfError::RenderPage { source })
}

/// Encodes a rendered bitmap as a PNG.
///
/// `path` names the destination only so a failure can say which file it was;
/// the bytes go to `writer`, which is how the caller can hand over a handle it
/// has already reserved rather than racing to create the file again.
pub fn write_png<W: Write + std::io::Seek>(
    bitmap: &PdfBitmap<'_>,
    writer: &mut W,
    path: &Path,
) -> Result<()> {
    bitmap
        .as_image()
        .map_err(|source| PdfError::RenderPage { source })?
        .write_to(writer, image::ImageFormat::Png)
        .map_err(|source| PdfError::SaveImage {
            format: "PNG",
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_sizes_are_accepted() {
        for size in [
            PngSize::Scale(0.5),
            PngSize::Scale(2.0),
            PngSize::WidthPx(1),
            PngSize::WidthPx(MAX_IMAGE_WIDTH_PX),
        ] {
            assert!(size.validate().is_ok(), "{size:?} must be accepted");
        }
    }

    #[test]
    fn a_scale_that_is_not_a_positive_finite_number_is_rejected() {
        for scale in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert!(
                matches!(
                    PngSize::Scale(scale).validate(),
                    Err(PdfError::InvalidImageScale { .. })
                ),
                "scale {scale} must be rejected"
            );
        }
    }

    #[test]
    fn a_width_outside_pdfiums_render_target_is_rejected() {
        for width_px in [0, MAX_IMAGE_WIDTH_PX + 1] {
            assert!(
                matches!(
                    PngSize::WidthPx(width_px).validate(),
                    Err(PdfError::InvalidImageWidth { .. })
                ),
                "width {width_px} must be rejected"
            );
        }
    }
}
