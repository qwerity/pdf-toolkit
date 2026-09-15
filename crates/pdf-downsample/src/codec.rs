//! Reading an image XObject's samples and writing resampled ones back.
//!
//! Only what can be round-tripped without changing what the page shows is
//! accepted: 8-bit grey, RGB or CMYK samples, stored losslessly (any filter
//! lopdf decodes, predictors included) or as a JPEG. Everything else —
//! palettes, spot colours, Lab, 1-bit stencils, JPEG 2000, JBIG2, colour-key
//! masks — is left as it is and reported with the reason, because resampling
//! it would mean converting it, and a conversion is a visible change.
//!
//! Samples are resampled as they are stored, in the colour space the PDF
//! already declares. That is what keeps CMYK exact: a four-channel buffer is
//! resized with alpha handling off, since premultiplying by a "K as alpha"
//! channel would shift every colour.

use std::io::Write;

use fast_image_resize::images::Image;
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};
use flate2::Compression;
use flate2::write::ZlibEncoder;
use jpeg_encoder::{ColorType as JpegColor, Encoder as JpegEncoder};
use lopdf::{Document, Object, Stream};
use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace as ZuneColorSpace;
use zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

use crate::report::{Encoding, SkipReason};

/// Decoded images above this many bytes are skipped rather than risk memory.
const MAX_DECODED_BYTES: u64 = 1 << 30;

/// Room for padding past the declared samples before a stream counts as a bomb.
const DECODE_SLACK_BYTES: u64 = 1 << 20;

/// How an image's samples were stored, which decides how they are written back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Storage {
    Jpeg(JpegLayout),
    Lossless,
}

impl Storage {
    pub(crate) fn encoding(self) -> Encoding {
        match self {
            Self::Jpeg(_) => Encoding::Jpeg,
            Self::Lossless => Encoding::Flate,
        }
    }
}

impl Encoding {
    /// The `/Filter` an image stream written this way declares.
    pub(crate) fn filter(self) -> &'static [u8] {
        match self {
            Self::Jpeg => b"DCTDecode",
            Self::Flate => b"FlateDecode",
        }
    }
}

/// The channel layout inside a JPEG, written back exactly as it was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JpegLayout {
    Luma,
    Rgb,
    /// Adobe APP14 transform 0: CMYK stored inverted.
    AdobeCmyk,
    /// Adobe APP14 transform 2: inverted CMY carried as YCC, plus inverted K.
    /// Photoshop writes CMYK this way.
    AdobeYcck,
}

/// What an image dictionary declares, read without decoding any samples.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Declared {
    pub width: u32,
    pub height: u32,
    channels: usize,
}

/// Checks from the dictionary alone whether the image can be resampled, so an
/// image that will be left as it is never costs a decode.
pub(crate) fn declared(doc: &Document, stream: &Stream) -> Result<Declared, SkipReason> {
    let dict = &stream.dict;
    if dict
        .get(b"ImageMask")
        .and_then(Object::as_bool)
        .unwrap_or(false)
    {
        return Err(SkipReason::ImageMask);
    }
    if matches!(dict.get(b"Mask"), Ok(Object::Array(_))) {
        return Err(SkipReason::ColorKeyMask);
    }
    if dict.get(b"BitsPerComponent").and_then(Object::as_i64).ok() != Some(8) {
        return Err(SkipReason::BitsPerComponent);
    }
    // lopdf applies a predictor only from a direct dictionary. Given an array
    // (one entry per filter) or a reference it inflates without undoing the
    // predictor, and the "samples" it returns are the right length and wrong.
    if matches!(dict.get(b"DecodeParms"), Ok(p) if !matches!(p, Object::Dictionary(_))) {
        return Err(SkipReason::UnsupportedFilter);
    }
    let declared = Declared {
        width: positive(dict.get(b"Width"))?,
        height: positive(dict.get(b"Height"))?,
        channels: components(doc, dict.get(b"ColorSpace").ok())?,
    };
    if declared.bytes() > MAX_DECODED_BYTES {
        return Err(SkipReason::TooLarge);
    }
    Ok(declared)
}

impl Declared {
    pub(crate) fn bytes(self) -> u64 {
        u64::from(self.width) * u64::from(self.height) * self.channels as u64
    }
}

/// Decodes the samples of an image [`declared`] accepted, interleaved, holding
/// the values the PDF stores (for an Adobe CMYK JPEG, the inverted ones).
pub(crate) fn decode(
    stream: &Stream,
    declared: Declared,
) -> Result<(Vec<u8>, Storage), SkipReason> {
    let filters = filters(stream);
    match filters.as_slice() {
        [filter] if filter == b"DCTDecode" => {
            // `/ColorTransform` overrides how readers interpret the JPEG's
            // channels, and the re-encoded stream is written without it.
            if stream
                .dict
                .get(b"DecodeParms")
                .and_then(Object::as_dict)
                .is_ok_and(|parms| parms.has(b"ColorTransform"))
            {
                return Err(SkipReason::UnsupportedFilter);
            }
            let (samples, layout) = decode_jpeg(&stream.content, declared)?;
            Ok((samples, Storage::Jpeg(layout)))
        }
        _ if filters.iter().any(|f| !LOSSLESS.contains(&f.as_slice())) => {
            Err(SkipReason::UnsupportedFilter)
        }
        _ => {
            // The limit applies per filter layer, and a Flate layer under a PNG
            // predictor inflates to one filter-type byte per row more than the
            // samples; producers also pad. Trailing data is ignored, as the
            // PDF specification says it must be; short data is a broken image.
            let expected = declared.bytes();
            let limit = expected + u64::from(declared.height) + DECODE_SLACK_BYTES;
            let mut raw = stream
                .decompressed_content_with_limit(limit as usize)
                .map_err(|_| SkipReason::DecodeFailed)?;
            if (raw.len() as u64) < expected {
                return Err(SkipReason::DecodeFailed);
            }
            raw.truncate(expected as usize);
            Ok((raw, Storage::Lossless))
        }
    }
}

/// Filters lopdf decodes itself, all lossless.
const LOSSLESS: [&[u8]; 5] = [
    b"FlateDecode",
    b"LZWDecode",
    b"ASCII85Decode",
    b"ASCIIHexDecode",
    b"RunLengthDecode",
];

/// Resamples to `width`×`height` and encodes in the stored form: a JPEG stays
/// a JPEG, and anything stored losslessly — line art, screenshots, soft
/// masks — stays lossless, so resampling is the only loss introduced.
///
/// `width` and `height` never exceed the decoded image, which the JPEG decoder
/// caps at 65 535 px a side, so they fit the encoder's `u16`.
pub(crate) fn encode(
    samples: Vec<u8>,
    declared: Declared,
    storage: Storage,
    width: u32,
    height: u32,
    jpeg_quality: u8,
) -> std::io::Result<Vec<u8>> {
    let mut raw = resize(samples, declared, width, height)?;
    match storage {
        Storage::Jpeg(layout) => {
            let color = match layout {
                JpegLayout::Luma => JpegColor::Luma,
                JpegLayout::Rgb => JpegColor::Rgb,
                JpegLayout::AdobeCmyk => {
                    // The encoder inverts CMYK on the way in, as Adobe files
                    // store it; the samples already hold the stored (inverted)
                    // values, so undo that here and the file ends up with the
                    // samples the source had.
                    raw.iter_mut().for_each(|v| *v = 255 - *v);
                    JpegColor::Cmyk
                }
                // YCCK goes in and comes out untouched on both sides.
                JpegLayout::AdobeYcck => JpegColor::Ycck,
            };
            let mut out = Vec::new();
            JpegEncoder::new(&mut out, jpeg_quality)
                .encode(&raw, width as u16, height as u16, color)
                .map_err(std::io::Error::other)?;
            Ok(out)
        }
        Storage::Lossless => {
            let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
            encoder.write_all(&raw)?;
            encoder.finish()
        }
    }
}

/// Interleaved samples resized with Catmull-Rom, the bicubic kernel: sharper
/// than a triangle filter on text inside scans, without Lanczos's ringing
/// around hard edges.
fn resize(
    samples: Vec<u8>,
    declared: Declared,
    width: u32,
    height: u32,
) -> std::io::Result<Vec<u8>> {
    let pixel_type = match declared.channels {
        1 => PixelType::U8,
        3 => PixelType::U8x3,
        _ => PixelType::U8x4,
    };
    let src = Image::from_vec_u8(declared.width, declared.height, samples, pixel_type)
        .map_err(std::io::Error::other)?;
    let mut dst = Image::new(width, height, pixel_type);
    let options = ResizeOptions::new()
        .resize_alg(ResizeAlg::Convolution(FilterType::CatmullRom))
        // The fourth channel is K, not alpha.
        .use_alpha(false);
    Resizer::new()
        .resize(&src, &mut dst, &options)
        .map_err(std::io::Error::other)?;
    Ok(dst.into_vec())
}

fn decode_jpeg(data: &[u8], declared: Declared) -> Result<(Vec<u8>, JpegLayout), SkipReason> {
    // Four channels are taken only with an Adobe marker, whose convention the
    // encoder reproduces. A plain four-channel JPEG has no agreed ink
    // direction, and readers disagree about it, so re-encoding one could
    // invert its colours in some of them.
    let (out_space, layout) = match (declared.channels, adobe_transform(data)) {
        (1, _) => (ZuneColorSpace::Luma, JpegLayout::Luma),
        (3, _) => (ZuneColorSpace::RGB, JpegLayout::Rgb),
        (_, Some(0)) => (ZuneColorSpace::CMYK, JpegLayout::AdobeCmyk),
        (_, Some(2)) => (ZuneColorSpace::YCCK, JpegLayout::AdobeYcck),
        _ => return Err(SkipReason::UnsupportedCmykJpeg),
    };
    let options = DecoderOptions::default()
        .set_max_width(u16::MAX as usize)
        .set_max_height(u16::MAX as usize)
        .jpeg_set_out_colorspace(out_space);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(data), options);
    let pixels = decoder.decode().map_err(|_| SkipReason::DecodeFailed)?;
    if decoder.dimensions() != Some((declared.width as usize, declared.height as usize))
        || decoder.output_colorspace() != Some(out_space)
        || pixels.len() as u64 != declared.bytes()
    {
        return Err(SkipReason::DecodeFailed);
    }
    Ok((pixels, layout))
}

/// The colour transform an Adobe APP14 segment declares, if the JPEG has one.
fn adobe_transform(data: &[u8]) -> Option<u8> {
    let mut i = 2;
    while i + 4 <= data.len() && data[i] == 0xFF {
        let marker = data[i + 1];
        // Start of scan: no more header segments follow.
        if marker == 0xDA {
            return None;
        }
        let length = usize::from(u16::from_be_bytes([data[i + 2], data[i + 3]]));
        let segment = data.get(i + 4..i + 2 + length)?;
        if marker == 0xEE && segment.len() >= 12 && segment.starts_with(b"Adobe") {
            return Some(segment[11]);
        }
        i += 2 + length;
    }
    None
}

/// Components per sample for the colour spaces that resample without
/// conversion. A soft mask declares none and is always one grey channel.
fn components(doc: &Document, space: Option<&Object>) -> Result<usize, SkipReason> {
    let Some(space) = space else { return Ok(1) };
    let space = match space {
        Object::Reference(id) => doc.get_object(*id).map_err(|_| SkipReason::ColorSpace)?,
        other => other,
    };
    match space {
        Object::Name(name) => match name.as_slice() {
            b"DeviceGray" | b"CalGray" | b"G" => Ok(1),
            b"DeviceRGB" | b"CalRGB" | b"RGB" => Ok(3),
            b"DeviceCMYK" | b"CMYK" => Ok(4),
            _ => Err(SkipReason::ColorSpace),
        },
        Object::Array(parts) => match parts.first().and_then(|p| p.as_name().ok()) {
            Some(b"CalGray") => Ok(1),
            Some(b"CalRGB") => Ok(3),
            Some(b"ICCBased") => {
                let profile = parts
                    .get(1)
                    .and_then(|p| p.as_reference().ok())
                    .and_then(|id| doc.get_object(id).and_then(Object::as_stream).ok())
                    .ok_or(SkipReason::ColorSpace)?;
                match profile.dict.get(b"N").and_then(Object::as_i64) {
                    Ok(n @ (1 | 3 | 4)) => Ok(n as usize),
                    _ => Err(SkipReason::ColorSpace),
                }
            }
            _ => Err(SkipReason::ColorSpace),
        },
        _ => Err(SkipReason::ColorSpace),
    }
}

fn filters(stream: &Stream) -> Vec<Vec<u8>> {
    match stream.dict.get(b"Filter") {
        Ok(Object::Name(name)) => vec![name.clone()],
        Ok(Object::Array(items)) => items
            .iter()
            .filter_map(|item| item.as_name().ok().map(<[u8]>::to_vec))
            .collect(),
        _ => Vec::new(),
    }
}

fn positive(value: lopdf::Result<&Object>) -> Result<u32, SkipReason> {
    value
        .and_then(Object::as_i64)
        .ok()
        .and_then(|v| u32::try_from(v).ok())
        .filter(|v| *v > 0)
        .ok_or(SkipReason::DecodeFailed)
}
