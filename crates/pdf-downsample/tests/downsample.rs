//! `downsample` against PDFs built object by object, so every size and
//! placement below is known exactly.

use std::path::{Path, PathBuf};

use jpeg_encoder::ColorType;
use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream, dictionary};
use pdf_downsample::{DownsampleOptions, Encoding, SkipReason, Written, downsample};
use tempfile::TempDir;
use zune_core::colorspace::ColorSpace;

/// A noisy RGB image: noise does not compress, so a resampled copy is
/// always smaller and `no_gain` never masks the behaviour under test.
fn noise_rgb(width: u32, height: u32) -> Vec<u8> {
    let mut state = 0x2545_f491_u32;
    (0..width * height * 3)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state & 0xff) as u8
        })
        .collect()
}

fn flate_image(width: u32, height: u32, color_space: &str) -> Stream {
    let components = if color_space == "DeviceGray" { 1 } else { 3 };
    let samples: Vec<u8> = noise_rgb(width, height)
        .into_iter()
        .take((width * height * components) as usize)
        .collect();
    let mut stream = Stream::new(
        dictionary! {
            "Type" => "XObject", "Subtype" => "Image",
            "Width" => i64::from(width), "Height" => i64::from(height),
            "ColorSpace" => color_space, "BitsPerComponent" => 8,
        },
        samples,
    );
    stream.compress().unwrap();
    stream
}

fn encode_jpeg(samples: &[u8], width: u32, height: u32, color: ColorType) -> Vec<u8> {
    let mut data = Vec::new();
    jpeg_encoder::Encoder::new(&mut data, 95)
        .encode(samples, width as u16, height as u16, color)
        .unwrap();
    data
}

/// Samples of a JPEG in `space`, exactly as a PDF reader receives them.
fn decode_jpeg(data: &[u8], space: ColorSpace) -> ((usize, usize), Vec<u8>) {
    let options = zune_core::options::DecoderOptions::default().jpeg_set_out_colorspace(space);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(
        zune_core::bytestream::ZCursor::new(data),
        options,
    );
    let pixels = decoder.decode().unwrap();
    (decoder.dimensions().unwrap(), pixels)
}

fn jpeg_image(width: u32, height: u32) -> Stream {
    Stream::new(
        dictionary! {
            "Type" => "XObject", "Subtype" => "Image",
            "Width" => i64::from(width), "Height" => i64::from(height),
            "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8, "Filter" => "DCTDecode",
        },
        encode_jpeg(&noise_rgb(width, height), width, height, ColorType::Rgb),
    )
}

/// One letter page per entry: each page draws the named images at the given
/// size in points, then a line of text in Helvetica.
struct Fixture {
    doc: Document,
    pages: Vec<ObjectId>,
    xobjects: Dictionary,
    font: ObjectId,
}

impl Fixture {
    fn new() -> Self {
        let mut doc = Document::with_version("1.7");
        let font = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
        });
        Self {
            doc,
            pages: Vec::new(),
            xobjects: Dictionary::new(),
            font,
        }
    }

    fn image(&mut self, name: &str, stream: Stream) -> ObjectId {
        let id = self.doc.add_object(stream);
        self.xobjects.set(name, id);
        id
    }

    fn page(&mut self, draws: &[(&str, f32, f32)]) {
        let mut operations = Vec::new();
        for (name, w, h) in draws {
            operations.push(Operation::new("q", vec![]));
            operations.push(Operation::new(
                "cm",
                vec![
                    (*w).into(),
                    0.into(),
                    0.into(),
                    (*h).into(),
                    36.into(),
                    36.into(),
                ],
            ));
            operations.push(Operation::new(
                "Do",
                vec![Object::Name(name.as_bytes().to_vec())],
            ));
            operations.push(Operation::new("Q", vec![]));
        }
        operations.extend([
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 12.into()]),
            Operation::new("Td", vec![72.into(), 700.into()]),
            Operation::new("Tj", vec![Object::string_literal("offices and efficiency")]),
            Operation::new("ET", vec![]),
        ]);
        let content = self.doc.add_object(Stream::new(
            Dictionary::new(),
            Content { operations }.encode().unwrap(),
        ));
        self.pages.push(content);
    }

    fn save(mut self, path: &Path) {
        let pages_id = self.doc.new_object_id();
        let resources = self.doc.add_object(dictionary! {
            "Font" => dictionary! { "F1" => self.font },
            "XObject" => self.xobjects.clone(),
        });
        let kids: Vec<Object> = self
            .pages
            .iter()
            .map(|content| {
                self.doc
                    .add_object(dictionary! {
                        "Type" => "Page", "Parent" => pages_id, "Contents" => *content,
                        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
                    })
                    .into()
            })
            .collect();
        let count = kids.len() as i64;
        self.doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages", "Kids" => kids, "Count" => count, "Resources" => resources,
            }),
        );
        let catalog = self
            .doc
            .add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        self.doc.trailer.set("Root", catalog);
        self.doc.save(path).unwrap();
    }
}

fn paths(dir: &TempDir) -> (PathBuf, PathBuf) {
    (dir.path().join("in.pdf"), dir.path().join("out.pdf"))
}

fn stream(doc: &Document, id: ObjectId) -> &Stream {
    doc.get_object(id).and_then(Object::as_stream).unwrap()
}

/// Content streams and fonts of every page, as raw bytes.
fn text_bearing_bytes(doc: &Document) -> Vec<Vec<u8>> {
    doc.get_pages()
        .values()
        .map(|page| {
            let mut bytes = doc.get_page_content(*page);
            let font = doc
                .get_page_fonts(*page)
                .unwrap()
                .into_values()
                .map(|f| format!("{f:?}"))
                .collect::<String>();
            bytes.extend(font.into_bytes());
            bytes
        })
        .collect()
}

#[test]
fn an_image_drawn_above_the_threshold_is_resampled_to_its_displayed_size() {
    let dir = TempDir::new().unwrap();
    let (input, output) = paths(&dir);
    let mut fixture = Fixture::new();
    // 1200×600 px drawn at 3×1.5 in: 400 dpi, so 600×300 at 200 dpi.
    let id = fixture.image("Im1", flate_image(1200, 600, "DeviceRGB"));
    fixture.page(&[("Im1", 216.0, 108.0)]);
    fixture.save(&input);

    let report = downsample(&input, &output, &DownsampleOptions::new(200)).unwrap();

    assert_eq!(report.written, Written::Downsampled);
    assert_eq!(report.images.downsampled, 1);
    let image = &report.downsampled[0];
    assert_eq!((image.from_px, image.to_px), ([1200, 600], [600, 300]));
    assert_eq!(image.effective_dpi, 400.0);
    assert_eq!(image.encoding, Encoding::Flate);
    assert_eq!(image.pages, vec![1]);
    assert!(report.bytes_after < report.bytes_before);

    let before = Document::load(&input).unwrap();
    let after = Document::load(&output).unwrap();
    let resampled = stream(&after, id);
    assert_eq!(resampled.dict.get(b"Width").unwrap().as_i64().unwrap(), 600);
    assert_eq!(
        resampled.decompressed_content().unwrap().len(),
        600 * 300 * 3
    );
    assert_eq!(text_bearing_bytes(&before), text_bearing_bytes(&after));
}

#[test]
fn an_image_at_or_below_the_threshold_is_left_and_the_input_bytes_are_written() {
    let dir = TempDir::new().unwrap();
    let (input, output) = paths(&dir);
    let mut fixture = Fixture::new();
    // 430 px across 2 in is 215 dpi: over 200, under the 220 threshold.
    fixture.image("Im1", flate_image(430, 430, "DeviceRGB"));
    fixture.page(&[("Im1", 144.0, 144.0)]);
    fixture.save(&input);

    let report = downsample(&input, &output, &DownsampleOptions::new(200)).unwrap();

    assert_eq!(report.written, Written::Original);
    assert_eq!(report.unchanged.get(&SkipReason::BelowThreshold), Some(&1));
    assert_eq!(
        std::fs::read(&input).unwrap(),
        std::fs::read(&output).unwrap()
    );
}

#[test]
fn an_image_drawn_twice_keeps_enough_pixels_for_its_largest_placement() {
    let dir = TempDir::new().unwrap();
    let (input, output) = paths(&dir);
    let mut fixture = Fixture::new();
    fixture.image("Im1", flate_image(2000, 2000, "DeviceGray"));
    fixture.page(&[("Im1", 72.0, 72.0)]);
    fixture.page(&[("Im1", 360.0, 360.0)]);
    fixture.save(&input);

    let report = downsample(&input, &output, &DownsampleOptions::new(200)).unwrap();

    let image = &report.downsampled[0];
    assert_eq!(image.to_px, [1000, 1000], "5 in at 200 dpi, not 1 in");
    assert_eq!(image.pages, vec![1, 2]);
}

#[test]
fn an_image_that_content_which_could_not_be_read_may_draw_is_left() {
    let dir = TempDir::new().unwrap();
    let (input, output) = paths(&dir);
    let mut fixture = Fixture::new();
    fixture.image("Im1", flate_image(2000, 2000, "DeviceGray"));
    // The form's content is in a filter lopdf cannot decode, so whether and
    // how large it draws the image is unknown.
    let form = fixture.doc.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject", "Subtype" => "Form", "Filter" => "JBIG2Decode",
            "BBox" => vec![0.into(), 0.into(), 1.into(), 1.into()],
        },
        b"q 360 0 0 360 36 36 cm /Im1 Do Q".to_vec(),
    ));
    fixture.xobjects.set("Fm1", form);
    fixture.page(&[("Im1", 72.0, 72.0)]);
    fixture.page(&[("Fm1", 1.0, 1.0)]);
    fixture.save(&input);

    let report = downsample(&input, &output, &DownsampleOptions::new(200)).unwrap();

    assert!(report.downsampled.is_empty());
    assert_eq!(report.unchanged.get(&SkipReason::NotPlaced), Some(&1));
}

#[test]
fn a_jpeg_stays_a_jpeg_and_quality_is_applied() {
    let dir = TempDir::new().unwrap();
    let (input, output) = paths(&dir);
    let mut fixture = Fixture::new();
    let id = fixture.image("Im1", jpeg_image(1600, 1600));
    fixture.page(&[("Im1", 144.0, 144.0)]);
    fixture.save(&input);

    let report = downsample(&input, &output, &DownsampleOptions::new(150)).unwrap();

    assert_eq!(report.downsampled[0].encoding, Encoding::Jpeg);
    assert_eq!(report.downsampled[0].to_px, [300, 300]);
    let after = Document::load(&output).unwrap();
    let resampled = stream(&after, id);
    assert_eq!(
        resampled.dict.get(b"Filter").unwrap().as_name().unwrap(),
        b"DCTDecode"
    );
    assert_eq!(
        decode_jpeg(&resampled.content, ColorSpace::RGB).0,
        (300, 300)
    );
}

#[test]
fn images_that_would_need_converting_or_have_no_placement_are_left_with_a_reason() {
    let dir = TempDir::new().unwrap();
    let (input, output) = paths(&dir);
    let mut fixture = Fixture::new();
    let indexed = {
        let mut s = flate_image(1000, 1000, "DeviceGray");
        let palette = Object::string_literal(vec![0u8; 256 * 3]);
        s.dict.set(
            "ColorSpace",
            vec!["Indexed".into(), "DeviceRGB".into(), 255.into(), palette],
        );
        s
    };
    fixture.image("Indexed", indexed);
    let keyed = {
        let mut s = flate_image(1000, 1000, "DeviceRGB");
        s.dict.set(
            "Mask",
            vec![
                0.into(),
                10.into(),
                0.into(),
                10.into(),
                0.into(),
                10.into(),
            ],
        );
        s
    };
    fixture.image("Keyed", keyed);
    fixture.image("Unused", flate_image(1000, 1000, "DeviceRGB"));
    fixture.page(&[("Indexed", 72.0, 72.0), ("Keyed", 72.0, 72.0)]);
    fixture.save(&input);

    let report = downsample(&input, &output, &DownsampleOptions::new(200)).unwrap();

    assert_eq!(report.images.total, 3);
    assert_eq!(report.images.downsampled, 0);
    assert_eq!(report.unchanged.get(&SkipReason::ColorSpace), Some(&1));
    assert_eq!(report.unchanged.get(&SkipReason::ColorKeyMask), Some(&1));
    assert_eq!(report.unchanged.get(&SkipReason::NotPlaced), Some(&1));
}

#[test]
fn a_soft_mask_is_resampled_with_its_image_and_an_image_inside_a_form_is_found() {
    let dir = TempDir::new().unwrap();
    let (input, output) = paths(&dir);
    let mut fixture = Fixture::new();
    let mask = fixture
        .doc
        .add_object(flate_image(1000, 1000, "DeviceGray"));
    let mut image = flate_image(1000, 1000, "DeviceRGB");
    image.dict.set("SMask", mask);
    let image = fixture.doc.add_object(image);
    // The form scales by 2, the page by 72: the image is drawn 2 in square.
    let form_content = Content {
        operations: vec![
            Operation::new(
                "cm",
                vec![72.into(), 0.into(), 0.into(), 72.into(), 0.into(), 0.into()],
            ),
            Operation::new("Do", vec![Object::Name(b"Im".to_vec())]),
        ],
    };
    let form = Stream::new(
        dictionary! {
            "Type" => "XObject", "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 1000.into(), 1000.into()],
            "Matrix" => vec![2.into(), 0.into(), 0.into(), 2.into(), 0.into(), 0.into()],
            "Resources" => dictionary! { "XObject" => dictionary! { "Im" => image } },
        },
        form_content.encode().unwrap(),
    );
    fixture.image("Fm1", form);
    fixture.page(&[("Fm1", 1.0, 1.0)]);
    fixture.save(&input);

    let report = downsample(&input, &output, &DownsampleOptions::new(200)).unwrap();

    assert_eq!(report.images.downsampled, 2, "{report:#?}");
    for resampled in &report.downsampled {
        assert_eq!(resampled.to_px, [400, 400]);
        assert_eq!(resampled.encoding, Encoding::Flate);
    }
    let after = Document::load(&output).unwrap();
    let smask = stream(&after, mask);
    assert_eq!(smask.decompressed_content().unwrap().len(), 400 * 400);
}

/// A four-channel JPEG image as Distiller and InDesign embed CMYK: inverted
/// ink, so the dictionary carries an inverting `/Decode`.
fn cmyk_jpeg_image(data: Vec<u8>, width: u32, height: u32) -> Stream {
    Stream::new(
        dictionary! {
            "Type" => "XObject", "Subtype" => "Image",
            "Width" => i64::from(width), "Height" => i64::from(height),
            "ColorSpace" => "DeviceCMYK", "BitsPerComponent" => 8, "Filter" => "DCTDecode",
            "Decode" => vec![1.into(), 0.into(), 1.into(), 0.into(), 1.into(), 0.into(), 1.into(), 0.into()],
        },
        data,
    )
}

/// A noisy Adobe CMYK (APP14 transform 0) JPEG image.
fn noisy_cmyk_jpeg_image(width: u32, height: u32) -> Stream {
    let ink: Vec<u8> = noise_rgb(width, height)
        .into_iter()
        .chain(noise_rgb(width, height))
        .take((width * height * 4) as usize)
        .collect();
    cmyk_jpeg_image(
        encode_jpeg(&ink, width, height, ColorType::Cmyk),
        width,
        height,
    )
}

#[test]
fn an_adobe_cmyk_jpeg_is_resampled_in_cmyk_with_its_stored_ink_convention_kept() {
    let dir = TempDir::new().unwrap();
    let (input, output) = paths(&dir);
    let mut fixture = Fixture::new();
    // A flat colour, so the resampled samples must equal the source's.
    let ink: Vec<u8> = (0..800 * 800).flat_map(|_| [30u8, 200, 90, 10]).collect();
    let flat = cmyk_jpeg_image(encode_jpeg(&ink, 800, 800, ColorType::Cmyk), 800, 800);
    let flat_id = fixture.image("Flat", flat);
    fixture.image("Noisy", noisy_cmyk_jpeg_image(1200, 1200));
    fixture.page(&[("Flat", 144.0, 144.0), ("Noisy", 144.0, 144.0)]);
    fixture.save(&input);

    let report = downsample(&input, &output, &DownsampleOptions::new(200)).unwrap();

    assert_eq!(report.images.downsampled, 2, "{report:#?}");
    let before = Document::load(&input).unwrap();
    let after = Document::load(&output).unwrap();
    let (_, source) = decode_jpeg(&stream(&before, flat_id).content, ColorSpace::CMYK);
    let resampled = stream(&after, flat_id);
    assert_eq!(
        resampled
            .dict
            .get(b"Decode")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        8,
        "Decode is kept"
    );
    let ((w, h), stored) = decode_jpeg(&resampled.content, ColorSpace::CMYK);
    assert_eq!((w, h), (400, 400));
    for channel in 0..4 {
        let diff = (i32::from(source[channel]) - i32::from(stored[channel])).abs();
        assert!(
            diff <= 2,
            "channel {channel}: {} became {}",
            source[channel],
            stored[channel]
        );
    }
}

#[test]
fn raw_cmyk_is_resampled_losslessly_and_a_cmyk_jpeg_without_an_adobe_marker_is_left() {
    let dir = TempDir::new().unwrap();
    let (input, output) = paths(&dir);
    let mut fixture = Fixture::new();
    let raw = {
        let samples: Vec<u8> = noise_rgb(1000, 1000)
            .into_iter()
            .chain(noise_rgb(1000, 1000))
            .take(4_000_000)
            .collect();
        let mut s = Stream::new(
            dictionary! {
                "Type" => "XObject", "Subtype" => "Image", "Width" => 1000, "Height" => 1000,
                "ColorSpace" => "DeviceCMYK", "BitsPerComponent" => 8,
            },
            samples,
        );
        s.compress().unwrap();
        s
    };
    let raw_id = fixture.image("Raw", raw);
    let unmarked = {
        let mut s = noisy_cmyk_jpeg_image(1000, 1000);
        let data = s.content.clone();
        // Drop the APP14 segment: SOI, then the segments up to it are kept.
        let at = data
            .windows(7)
            .position(|w| w[0] == 0xFF && w[1] == 0xEE && &w[4..7] == b"Ado")
            .unwrap();
        let len = usize::from(u16::from_be_bytes([data[at + 2], data[at + 3]]));
        let stripped = [&data[..at], &data[at + 2 + len..]].concat();
        s.set_content(stripped);
        s
    };
    fixture.image("Unmarked", unmarked);
    fixture.page(&[("Raw", 72.0, 72.0), ("Unmarked", 72.0, 72.0)]);
    fixture.save(&input);

    let report = downsample(&input, &output, &DownsampleOptions::new(200)).unwrap();

    assert_eq!(report.images.downsampled, 1);
    assert_eq!(report.downsampled[0].encoding, Encoding::Flate);
    assert_eq!(
        report.unchanged.get(&SkipReason::UnsupportedCmykJpeg),
        Some(&1)
    );
    let after = Document::load(&output).unwrap();
    assert_eq!(
        stream(&after, raw_id).decompressed_content().unwrap().len(),
        200 * 200 * 4
    );
}

#[test]
fn an_adobe_ycck_jpeg_is_resampled_as_ycck() {
    let dir = TempDir::new().unwrap();
    let (input, output) = paths(&dir);
    let mut fixture = Fixture::new();
    let (w, h) = (1000u32, 1000u32);
    let ycck: Vec<u8> = (0..w * h).flat_map(|_| [120u8, 90, 160, 40]).collect();
    let id = fixture.image(
        "Ycck",
        cmyk_jpeg_image(encode_jpeg(&ycck, w, h, ColorType::Ycck), w, h),
    );
    fixture.page(&[("Ycck", 72.0, 72.0)]);
    fixture.save(&input);

    let report = downsample(&input, &output, &DownsampleOptions::new(200)).unwrap();

    assert_eq!(report.images.downsampled, 1, "{report:#?}");
    let after = Document::load(&output).unwrap();
    let (size, pixels) = decode_jpeg(&stream(&after, id).content, ColorSpace::YCCK);
    assert_eq!(size, (200, 200));
    for (channel, want) in [120u8, 90, 160, 40].into_iter().enumerate() {
        let got = pixels[channel];
        assert!(
            got.abs_diff(want) <= 2,
            "channel {channel}: {want} became {got}"
        );
    }
}

#[test]
fn an_image_and_its_matte_mask_are_resampled_together_or_not_at_all() {
    let dir = TempDir::new().unwrap();
    let (input, output) = paths(&dir);
    let mut fixture = Fixture::new();
    // The mask's parameters are an array, which is left as it is; the image
    // alone would be resampled, and the pair would stop matching.
    let mut mask = flate_image(1000, 1000, "DeviceGray");
    mask.dict.set("Matte", vec![0.into(), 0.into(), 0.into()]);
    mask.dict.set("DecodeParms", vec![Object::Null]);
    let mask = fixture.doc.add_object(mask);
    let mut image = flate_image(1000, 1000, "DeviceRGB");
    image.dict.set("SMask", mask);
    let image = fixture.image("Im1", image);
    fixture.page(&[("Im1", 72.0, 72.0)]);
    fixture.save(&input);

    let report = downsample(&input, &output, &DownsampleOptions::new(200)).unwrap();

    assert_eq!(report.images.downsampled, 0, "{report:#?}");
    assert_eq!(report.unchanged.get(&SkipReason::MattePair), Some(&1));
    assert_eq!(
        report.unchanged.get(&SkipReason::UnsupportedFilter),
        Some(&1)
    );
    let after = Document::load(&output).unwrap();
    assert_eq!(
        stream(&after, image)
            .dict
            .get(b"Width")
            .unwrap()
            .as_i64()
            .unwrap(),
        1000
    );
}

#[test]
fn the_output_may_not_be_the_input() {
    let dir = TempDir::new().unwrap();
    let (input, _) = paths(&dir);
    let mut fixture = Fixture::new();
    fixture.image("Im1", flate_image(1000, 1000, "DeviceRGB"));
    fixture.page(&[("Im1", 72.0, 72.0)]);
    fixture.save(&input);
    let before = std::fs::read(&input).unwrap();

    assert!(downsample(&input, &input, &DownsampleOptions::new(200)).is_err());
    assert_eq!(std::fs::read(&input).unwrap(), before);
}
