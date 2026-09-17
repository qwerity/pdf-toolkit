//! Reject oversized renders before PDFium allocates a bitmap.
use super::PngSize;
use pdf_core::error::{PdfError, Result};

const VARIABLE: &str = "PDF_TOOLKIT_MAX_RENDER_PIXELS";

pub(super) fn check(width: f32, height: f32, size: PngSize) -> Result<()> {
    let raw = std::env::var(VARIABLE).map_err(|error| match error {
        std::env::VarError::NotPresent => None,
        std::env::VarError::NotUnicode(value) => Some(value.to_string_lossy().into_owned()),
    });
    let limit = match raw {
        Ok(raw) => parse_limit(Some(&raw))?,
        Err(None) => u64::MAX,
        Err(Some(value)) => {
            return Err(PdfError::InvalidResourceLimit {
                name: VARIABLE,
                value,
            });
        }
    };
    check_dimensions(width, height, size, limit)
}

fn parse_limit(raw: Option<&str>) -> Result<u64> {
    match raw {
        None => Ok(u64::MAX),
        Some(raw) => raw
            .parse::<u64>()
            .ok()
            .filter(|value| *value > 0)
            .ok_or_else(|| PdfError::InvalidResourceLimit {
                name: VARIABLE,
                value: raw.into(),
            }),
    }
}

fn check_dimensions(width: f32, height: f32, size: PngSize, limit: u64) -> Result<()> {
    let (width, height) = (f64::from(width), f64::from(height));
    let scale = match size {
        PngSize::Scale(scale) => f64::from(scale),
        PngSize::WidthPx(target) => f64::from(target) / width,
    };
    let (pixels_x, pixels_y) = ((width * scale).ceil(), (height * scale).ceil());
    if !pixels_x.is_finite()
        || !pixels_y.is_finite()
        || pixels_x < 1.0
        || pixels_y < 1.0
        || pixels_x > f64::from(i32::MAX)
        || pixels_y > f64::from(i32::MAX)
        || pixels_x * pixels_y > limit as f64
    {
        return Err(PdfError::RenderTooLarge { max_pixels: limit });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_both_dimensions_and_scale_before_allocation() {
        assert!(check_dimensions(100.0, 200.0, PngSize::WidthPx(100), 20_000).is_ok());
        assert!(check_dimensions(100.0, 200.0, PngSize::WidthPx(100), 19_999).is_err());
        assert!(check_dimensions(1.0, 1_000_000.0, PngSize::WidthPx(100), 32_000_000).is_err());
        assert!(check_dimensions(100.0, 100.0, PngSize::Scale(f32::MAX), u64::MAX).is_err());
        assert!(check_dimensions(0.0, 100.0, PngSize::WidthPx(100), u64::MAX).is_err());
        assert!(check_dimensions(f32::NAN, 100.0, PngSize::Scale(1.0), u64::MAX).is_err());
    }

    #[test]
    fn invalid_configuration_is_not_silently_ignored() {
        assert_eq!(parse_limit(None).expect("unlimited"), u64::MAX);
        assert_eq!(parse_limit(Some("100")).expect("limit"), 100);
        for raw in ["0", "-1", "abc", "18446744073709551616"] {
            assert!(parse_limit(Some(raw)).is_err());
        }
    }
}
