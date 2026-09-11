//! QR decoding over rendered pages.
//!
//! Two tiers, calibrated on the corpus (`docs/annotation-findings.md`): at
//! [`BASE_WIDTH_PX`] every ordinarily-sized code decodes in ~10 ms; codes
//! printed small (a certificate header inside a scanned collage) need
//! [`RETRY_WIDTH_PX`]. The retry render is paid only for pages that plausibly
//! carry a code — scans, and pages whose text names e-cadastre — because the
//! remaining pages are CAD sheets, which never carry one.
//!
//! Detection is restricted to QR: the corpus documents also carry 1D barcodes
//! whose decodes are unreliable at these resolutions, and the annotation
//! schema records QR payloads only.

use std::collections::HashSet;

use pdfium_render::prelude::{PdfBitmapFormat, PdfPage};
use rxing::{BarcodeFormat, DecodeHints};

use pdf_core::error::Result;
use pdf_render::{PngSize, render_page};

/// Every ordinary code decodes here; see the calibration note above.
pub const BASE_WIDTH_PX: u32 = 1400;

/// Resolution that recovered the smallest decodable code in the corpus.
pub const RETRY_WIDTH_PX: u32 = 3000;

/// Decodes the QR payloads on a rendered page, in reading order.
pub fn decode_at(page: &PdfPage<'_>, width_px: u32) -> Result<Vec<String>> {
    let bitmap = render_page(page, PngSize::WidthPx(width_px))?;
    let width = u32::try_from(bitmap.width()).unwrap_or_default();
    let height = u32::try_from(bitmap.height()).unwrap_or_default();
    let format = bitmap
        .format()
        .map_err(|source| pdf_core::error::PdfError::RenderPage { source })?;
    let luma = to_luma(&bitmap.as_raw_bytes(), width, height, format);

    let mut hints = DecodeHints {
        TryHarder: Some(true),
        PossibleFormats: Some(HashSet::from([BarcodeFormat::QR_CODE])),
        ..Default::default()
    };

    let mut payloads: Vec<String> =
        match rxing::helpers::detect_multiple_in_luma_with_hints(luma, width, height, &mut hints) {
            Ok(results) => results.iter().map(|r| r.getText().to_string()).collect(),
            // "Nothing found" is an rxing error, not a failure of the page.
            Err(_) => Vec::new(),
        };
    payloads.dedup();
    Ok(payloads)
}

/// One pass from the renderer's raw pixels to the decoder's luma plane.
///
/// The obvious `as_image().to_luma8()` route materialises three full frames
/// per page (raw BGRA, RGBA, luma) — hundreds of megabytes of transient
/// copies over a document. Rows are addressed by stride because pdfium may
/// pad scanlines.
fn to_luma(raw: &[u8], width: u32, height: u32, format: PdfBitmapFormat) -> Vec<u8> {
    let bytes_per_pixel = match format {
        PdfBitmapFormat::Gray => 1,
        PdfBitmapFormat::BGR => 3,
        PdfBitmapFormat::BGRx | PdfBitmapFormat::BGRA => 4,
    };
    let (width, height) = (width as usize, height as usize);
    let stride = raw.len().checked_div(height).unwrap_or_default();
    let mut luma = Vec::with_capacity(width * height);
    for row in 0..height {
        let line = &raw[row * stride..];
        for column in 0..width {
            let pixel = &line[column * bytes_per_pixel..];
            luma.push(if bytes_per_pixel == 1 {
                pixel[0]
            } else {
                // BT.709, matching the `image` crate's grayscale weights.
                let (b, g, r) = (pixel[0] as u32, pixel[1] as u32, pixel[2] as u32);
                ((r * 2126 + g * 7152 + b * 722 + 5000) / 10000) as u8
            });
        }
    }
    luma
}
