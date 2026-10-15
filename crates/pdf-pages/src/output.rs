//! What a cut produces, and the format it was asked for.

use std::path::PathBuf;

use pdf_core::error::Result;
use pdf_render::PngSize;

/// Metadata about a generated page file.
pub struct PageResult {
    pub page_number: Option<u16>,
    pub output_path: PathBuf,
    pub page_count: usize,
    pub width_px: u32,
    pub height_px: u32,
    pub contains_images: bool,
    pub size_kb: u64,
}

/// What a caller asked to be produced.
///
/// `concat` rides on the PDF variant rather than standing beside the format
/// because merging is only meaningful for PDF: a set of pages becomes one
/// document, whereas images stay one file per page whatever is asked.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum OutputFormat {
    Pdf {
        concat: bool,
    },
    Png {
        size: PngSize,
    },
    /// Lossless for a page without raster images, lossy otherwise; see
    /// [`pdf_render::write_webp`].
    Webp {
        size: PngSize,
    },
}

impl OutputFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Pdf { .. } => "pdf",
            Self::Png { .. } => "png",
            Self::Webp { .. } => "webp",
        }
    }

    /// Rejects a request that cannot be rendered. PDF output has nothing to
    /// check; an image defers to the renderer, which owns the bounds it can
    /// meet.
    pub fn validate(self) -> Result<()> {
        match self {
            Self::Pdf { .. } => Ok(()),
            Self::Png { size } | Self::Webp { size } => size.validate(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdf_core::error::PdfError;
    use pdf_render::MAX_IMAGE_WIDTH_PX;

    #[test]
    fn extension_matches_format() {
        assert_eq!(OutputFormat::Pdf { concat: false }.extension(), "pdf");
        assert_eq!(OutputFormat::Pdf { concat: true }.extension(), "pdf");
        assert_eq!(
            OutputFormat::Png {
                size: PngSize::Scale(1.0)
            }
            .extension(),
            "png"
        );
        assert_eq!(
            OutputFormat::Webp {
                size: PngSize::Scale(1.0)
            }
            .extension(),
            "webp"
        );
    }

    #[test]
    fn validate_accepts_pdf_and_valid_png_sizes() {
        assert!(OutputFormat::Pdf { concat: true }.validate().is_ok());
        for size in [
            PngSize::Scale(0.5),
            PngSize::Scale(2.0),
            PngSize::WidthPx(1),
            PngSize::WidthPx(MAX_IMAGE_WIDTH_PX),
            PngSize::LongEdgePx(1),
        ] {
            assert!(OutputFormat::Png { size }.validate().is_ok());
            assert!(OutputFormat::Webp { size }.validate().is_ok());
        }
    }

    #[test]
    fn validate_rejects_bad_png_scales() {
        for scale in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert!(
                matches!(
                    OutputFormat::Png {
                        size: PngSize::Scale(scale)
                    }
                    .validate(),
                    Err(PdfError::InvalidImageScale { .. })
                ),
                "scale {scale} must be rejected"
            );
        }
    }

    #[test]
    fn validate_rejects_bad_png_widths() {
        for width_px in [0, MAX_IMAGE_WIDTH_PX + 1] {
            assert!(
                matches!(
                    OutputFormat::Png {
                        size: PngSize::WidthPx(width_px)
                    }
                    .validate(),
                    Err(PdfError::InvalidImageWidth { .. })
                ),
                "width {width_px} must be rejected"
            );
        }
    }
}
