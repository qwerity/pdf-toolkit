//! The `downsample` result, as printed on stdout.

use std::collections::BTreeMap;

use serde::Serialize;

/// What one run did to one document.
#[derive(Debug, Clone, Serialize)]
pub struct DownsampleReport {
    pub input: String,
    pub output: String,
    pub pages: usize,
    pub target_dpi: u32,
    pub threshold_dpi: u32,
    pub jpeg_quality: u8,
    pub bytes_before: u64,
    pub bytes_after: u64,
    /// Which file was written: the rewritten document, or the input's own
    /// bytes when rewriting saved nothing.
    pub written: Written,
    pub images: ImageCounts,
    /// Per-image detail of what was resampled, in object order.
    pub downsampled: Vec<DownsampledImage>,
    /// Why the other images were left as they are; absent reasons are zero.
    pub unchanged: BTreeMap<SkipReason, usize>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Written {
    Downsampled,
    Original,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct ImageCounts {
    pub total: usize,
    pub downsampled: usize,
    pub unchanged: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct DownsampledImage {
    /// `"<object> <generation>"`.
    pub object: String,
    pub pages: Vec<u32>,
    pub from_px: [u32; 2],
    pub to_px: [u32; 2],
    /// Pixels over the largest area the image is drawn at.
    pub effective_dpi: f64,
    pub bytes_before: usize,
    pub bytes_after: usize,
    pub encoding: Encoding,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Encoding {
    Jpeg,
    Flate,
}

/// Why an image was not resampled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// Already at or below the threshold resolution.
    BelowThreshold,
    /// Never painted by a page's content, so its display size is unknown.
    NotPlaced,
    /// Resampled, but the result was not smaller.
    NoGain,
    /// A 1-bit stencil mask.
    ImageMask,
    /// A colour-key `/Mask` array, which lossy samples would break.
    ColorKeyMask,
    /// Not 8 bits per component.
    BitsPerComponent,
    /// Indexed, separation, DeviceN, Lab, or unreadable.
    ColorSpace,
    /// JPEG 2000, JBIG2, CCITT, a filter chain around a JPEG, or a JPEG whose
    /// `/ColorTransform` overrides its channel interpretation.
    UnsupportedFilter,
    /// A four-channel JPEG without an Adobe CMYK or YCCK marker, whose ink
    /// direction readers disagree about.
    UnsupportedCmykJpeg,
    /// Decoded samples too large to hold in memory.
    TooLarge,
    /// The stream did not decode to the size its dictionary declares.
    DecodeFailed,
    /// Half of an image and `/Matte` soft mask pair whose other half is left
    /// as it is: the two must stay the same pixel size.
    MattePair,
}
