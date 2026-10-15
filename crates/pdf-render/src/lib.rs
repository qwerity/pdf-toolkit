//! Rasterising a PDF page to PNG or WebP, through PDFium.
//!
//! This is deliberately its own crate rather than a corner of the page cutter,
//! because rendering is the capability with the widest reach: anything that
//! needs to *look* at a page — a vision model, a side-by-side comparison, a
//! thumbnail — needs pixels, and none of that needs the cutter.
//!
//! Sizing is a closed set ([`PngSize`]) rather than a free DPI number so that
//! the caller states the constraint it actually has: a fixed scale factor, a
//! fixed output width, or a fixed *longest* edge — what a landscape sheet
//! needs, and what Poppler's `pdftoppm -scale-to` provides.

mod limits;

use std::io::Write;
use std::path::Path;

use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::error::{EncodingError, ImageFormatHint};
use image::{DynamicImage, ImageEncoder, ImageError, ImageFormat};
use pdfium_render::prelude::{PdfBitmap, PdfColor, PdfPage, PdfRenderConfig};
use webpx::{EncoderConfig, Unstoppable};

use pdf_core::error::{PdfError, Result};
use pdf_core::page::page_dimensions_px;

/// Widths — and long edges — beyond this cannot reach PDFium, whose render
/// target is `i32`.
pub const MAX_IMAGE_WIDTH_PX: u32 = i32::MAX as u32;

/// How large to render a page.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum PngSize {
    /// Multiply the page's own dimensions by this factor.
    Scale(f32),
    /// Produce exactly this width in pixels, preserving the aspect ratio.
    WidthPx(u32),
    /// Produce exactly this many pixels along the page's longer side — the
    /// width of a landscape page, the height of a portrait one — preserving
    /// the aspect ratio. A square page takes it as its width.
    LongEdgePx(u32),
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
            Self::LongEdgePx(edge_px) if (1..=MAX_IMAGE_WIDTH_PX).contains(&edge_px) => Ok(()),
            Self::LongEdgePx(edge_px) => Err(PdfError::InvalidImageLongEdge {
                value: edge_px,
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
        PngSize::LongEdgePx(edge_px) => {
            let (width, height) = (page.width().value, page.height().value);
            if width >= height {
                (edge_px, ((height * edge_px as f32) / width).round() as u32)
            } else {
                (((width * edge_px as f32) / height).round() as u32, edge_px)
            }
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
        PngSize::LongEdgePx(edge_px) => {
            let edge_px = i32::try_from(edge_px)
                .expect("invariant: validate() bounds the long edge by i32::MAX");
            if page.width().value >= page.height().value {
                PdfRenderConfig::new().set_target_width(edge_px)
            } else {
                PdfRenderConfig::new().set_target_height(edge_px)
            }
        }
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
///
/// The encoder's default level is `Fast`, which leaves a text page about twice
/// the size `Default` makes it for ~50 ms more on a 2448×3168 render; `Best`
/// saves a further ~1% at three times the cost. An opaque bitmap — every page
/// [`render_page`] draws onto white — is written without its alpha channel.
pub fn write_png<W: Write + std::io::Seek>(
    bitmap: &PdfBitmap<'_>,
    writer: &mut W,
    path: &Path,
) -> Result<()> {
    let image = opaque_image(bitmap)?;
    PngEncoder::new_with_quality(writer, CompressionType::Default, FilterType::Adaptive)
        .write_image(
            image.as_bytes(),
            image.width(),
            image.height(),
            image.color().into(),
        )
        .map_err(|source| PdfError::SaveImage {
            format: "PNG",
            path: path.to_path_buf(),
            source,
        })
}

/// libwebp's lossless effort preset 1 (`cwebp -lossless -z 1`). On rendered
/// text pages it matched the size of every higher preset, up to 9, at
/// ~50 ms instead of ~6 s for a 2473×3200 page.
const WEBP_LOSSLESS_METHOD: u8 = 1;
const WEBP_LOSSLESS_QUALITY: f32 = 20.0;

/// Lossy settings for pages with raster images. Quality 90 showed no visible
/// difference at 1:1 on maps, scans and photos; method 4 is libwebp's default,
/// and the higher methods cost 1.5–2× the time for a further 1.5–6%.
const WEBP_LOSSY_QUALITY: f32 = 90.0;
const WEBP_LOSSY_METHOD: u8 = 4;

/// Above this many bits per pixel a lossless encoding of a page without
/// raster images is not the text page it was assumed to be — a dense colour
/// drawing, a gradient — so the lossy encoding is tried as well. Rendered
/// text pages measured ~0.3.
const WEBP_LOSSLESS_MAX_BITS_PER_PIXEL: u64 = 1;

/// Encodes a rendered bitmap as a WebP.
///
/// `contains_images` picks the encoding: lossless for a page drawn from text
/// and vector paths only — pixel-exact, and smaller than lossy on rendered
/// text — and lossy for a page with raster images, where lossless is several
/// times larger. A page without images whose lossless encoding still exceeds
/// [`WEBP_LOSSLESS_MAX_BITS_PER_PIXEL`] is also encoded lossy, and the smaller
/// of the two is written.
///
/// `path` names the destination only so a failure can say which file it was.
pub fn write_webp<W: Write>(
    bitmap: &PdfBitmap<'_>,
    writer: &mut W,
    path: &Path,
    contains_images: bool,
) -> Result<()> {
    let image = opaque_image(bitmap)?;
    let save_error = |source| PdfError::SaveImage {
        format: "WebP",
        path: path.to_path_buf(),
        source,
    };
    let webp = encode_webp(&image, contains_images).map_err(save_error)?;
    writer
        .write_all(&webp)
        .map_err(|source| save_error(ImageError::IoError(source)))
}

fn encode_webp(image: &DynamicImage, contains_images: bool) -> image::ImageResult<Vec<u8>> {
    let lossy = || {
        encode_webp_with(
            image,
            &EncoderConfig::new()
                .quality(WEBP_LOSSY_QUALITY)
                .method(WEBP_LOSSY_METHOD),
        )
    };
    if contains_images {
        return lossy();
    }

    let lossless = encode_webp_with(
        image,
        &EncoderConfig::new_lossless()
            .method(WEBP_LOSSLESS_METHOD)
            .quality(WEBP_LOSSLESS_QUALITY),
    )?;
    let pixels = u64::from(image.width()) * u64::from(image.height());
    if (lossless.len() as u64) * 8 <= WEBP_LOSSLESS_MAX_BITS_PER_PIXEL * pixels {
        return Ok(lossless);
    }
    let lossy = lossy()?;
    Ok(if lossy.len() < lossless.len() {
        lossy
    } else {
        lossless
    })
}

fn encode_webp_with(image: &DynamicImage, config: &EncoderConfig) -> image::ImageResult<Vec<u8>> {
    let (width, height) = (image.width(), image.height());
    match image {
        DynamicImage::ImageRgb8(rgb) => config.encode_rgb(rgb.as_raw(), width, height, Unstoppable),
        DynamicImage::ImageRgba8(rgba) => {
            config.encode_rgba(rgba.as_raw(), width, height, Unstoppable)
        }
        other => config.encode_rgba(other.to_rgba8().as_raw(), width, height, Unstoppable),
    }
    .map_err(|error| {
        ImageError::Encoding(EncodingError::new(
            ImageFormatHint::Exact(ImageFormat::WebP),
            error.to_string(),
        ))
    })
}

/// The bitmap as an image, without the alpha channel when it is fully opaque.
fn opaque_image(bitmap: &PdfBitmap<'_>) -> Result<DynamicImage> {
    bitmap
        .as_image()
        .map(without_opaque_alpha)
        .map_err(|source| PdfError::RenderPage { source })
}

/// Drops an alpha channel that carries no information; any other image is
/// returned as it came.
fn without_opaque_alpha(image: DynamicImage) -> DynamicImage {
    match image {
        DynamicImage::ImageRgba8(rgba) if rgba.pixels().all(|pixel| pixel[3] == u8::MAX) => {
            DynamicImage::ImageRgb8(DynamicImage::ImageRgba8(rgba).to_rgb8())
        }
        other => other,
    }
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
            PngSize::LongEdgePx(1),
            PngSize::LongEdgePx(MAX_IMAGE_WIDTH_PX),
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

    #[test]
    fn a_long_edge_outside_pdfiums_render_target_is_rejected() {
        for edge_px in [0, MAX_IMAGE_WIDTH_PX + 1] {
            assert!(
                matches!(
                    PngSize::LongEdgePx(edge_px).validate(),
                    Err(PdfError::InvalidImageLongEdge { .. })
                ),
                "long edge {edge_px} must be rejected"
            );
        }
    }

    #[test]
    fn an_opaque_image_loses_its_alpha_and_a_translucent_one_keeps_it() {
        let opaque = image::RgbaImage::from_pixel(2, 2, image::Rgba([10, 20, 30, 255]));
        let stripped = without_opaque_alpha(DynamicImage::ImageRgba8(opaque));
        assert_eq!(stripped.color(), image::ColorType::Rgb8);
        assert_eq!(
            stripped.as_rgb8().expect("rgb").get_pixel(1, 1).0,
            [10, 20, 30]
        );

        let mut translucent = image::RgbaImage::from_pixel(2, 2, image::Rgba([0, 0, 0, 255]));
        translucent.put_pixel(0, 0, image::Rgba([0, 0, 0, 128]));
        let kept = without_opaque_alpha(DynamicImage::ImageRgba8(translucent));
        assert_eq!(kept.color(), image::ColorType::Rgba8);
    }

    /// The RIFF chunk naming the bitstream: `VP8L` is lossless, `VP8 ` lossy.
    fn webp_bitstream(webp: &[u8]) -> &[u8] {
        assert_eq!(&webp[0..4], b"RIFF");
        assert_eq!(&webp[8..12], b"WEBP");
        &webp[12..16]
    }

    fn text_like_page() -> DynamicImage {
        let mut page = image::RgbImage::from_pixel(400, 300, image::Rgb([255, 255, 255]));
        for y in (20..280).step_by(12) {
            for x in 30..370 {
                page.put_pixel(x, y, image::Rgb([0, 0, 0]));
            }
        }
        DynamicImage::ImageRgb8(page)
    }

    #[test]
    fn a_page_without_images_is_written_lossless() {
        let webp = encode_webp(&text_like_page(), false).expect("encodes");
        assert_eq!(webp_bitstream(&webp), b"VP8L");
    }

    #[test]
    fn a_page_with_images_is_written_lossy() {
        let webp = encode_webp(&text_like_page(), true).expect("encodes");
        assert_eq!(webp_bitstream(&webp), b"VP8 ");
    }

    /// Noise has no structure for lossless coding to find, so it lands far
    /// above the bits-per-pixel bound and the lossy encoding is kept.
    #[test]
    fn a_page_without_images_that_codes_poorly_lossless_falls_back_to_lossy() {
        let mut state: u32 = 0x2545_f491;
        let noise = image::RgbImage::from_fn(300, 300, |_, _| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let [r, g, b, _] = state.to_le_bytes();
            image::Rgb([r, g, b])
        });
        let webp = encode_webp(&DynamicImage::ImageRgb8(noise), false).expect("encodes");
        assert_eq!(webp_bitstream(&webp), b"VP8 ");
    }

    #[test]
    fn a_translucent_image_is_encoded_with_its_alpha() {
        let mut rgba = image::RgbaImage::from_pixel(16, 16, image::Rgba([0, 0, 0, 255]));
        rgba.put_pixel(0, 0, image::Rgba([0, 0, 0, 0]));
        let webp = encode_webp(&DynamicImage::ImageRgba8(rgba), false).expect("encodes");
        assert_eq!(webp_bitstream(&webp), b"VP8L");
    }
}
