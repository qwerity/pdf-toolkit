//! Shrinks a PDF by resampling its raster images, and nothing else.
//!
//! A general-purpose rewriter (Ghostscript's `pdfwrite`) re-embeds every font
//! on the way through, and the text layer does not survive that intact: on a
//! planning handbook a ligature lost its Unicode mapping and "offices" was
//! extracted as "ofÏces", which a search for "offices" no longer finds. Here
//! only image XObject streams are replaced. Content streams, fonts, their
//! `ToUnicode` maps and the page tree are carried through unchanged, so text
//! extraction reads the same characters from the output as from the input.
//!
//! Resolution is measured against the size each image is drawn at, so a scan and a thumbnail of the same pixels are treated
//! differently, and an image whose display size cannot be established is
//! left alone.

mod codec;
mod placement;
mod report;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result, bail};
use lopdf::{Document, Object, ObjectId, Stream};
use rayon::prelude::*;

pub use report::{DownsampleReport, DownsampledImage, Encoding, ImageCounts, SkipReason, Written};

/// JPEG quality used when none is given, for images the PDF stores as JPEG.
pub const DEFAULT_JPEG_QUALITY: u8 = 85;

/// How far to resample.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DownsampleOptions {
    /// Resolution an image is resampled to, in pixels per displayed inch.
    pub target_dpi: u32,
    /// Only images above this resolution are resampled. Kept above the
    /// target so an image a few percent over it is not re-encoded for a
    /// negligible saving and a generation of JPEG loss.
    pub threshold_dpi: u32,
    /// JPEG quality (1–100) for images that were stored as JPEG.
    pub jpeg_quality: u8,
    /// Estimated bytes available to concurrently decoded/resized samples.
    /// Images exceeding this budget are left unchanged as `too_large`.
    pub working_memory_bytes: u64,
}

impl DownsampleOptions {
    /// `threshold_dpi` defaults to 10 % above the target, as Ghostscript's
    /// `DownsampleThreshold` of 1.1 does.
    pub fn new(target_dpi: u32) -> Self {
        Self {
            target_dpi,
            threshold_dpi: target_dpi.saturating_add(target_dpi.div_ceil(10)),
            jpeg_quality: DEFAULT_JPEG_QUALITY,
            working_memory_bytes: PARALLEL_BYTES_BUDGET,
        }
    }
}

/// Resamples `input`'s images and writes the result to `output`.
///
/// The output is never larger than the input: when rewriting saves nothing,
/// the input's own bytes are written and `written` says so.
///
/// # Errors
/// An unreadable or encrypted input, `output` naming the input, or a write
/// that fails. A single image that cannot be resampled is not an error; it is
/// counted under [`DownsampleReport::unchanged`].
///
/// # Example
/// ```no_run
/// use pdf_downsample::{DownsampleOptions, downsample};
/// let report = downsample("plan.pdf".as_ref(), "plan.200dpi.pdf".as_ref(), &DownsampleOptions::new(200))?;
/// println!("{} -> {} bytes", report.bytes_before, report.bytes_after);
/// # Ok::<(), anyhow::Error>(())
/// ```
pub fn downsample(
    input: &Path,
    output: &Path,
    options: &DownsampleOptions,
) -> Result<DownsampleReport> {
    anyhow::ensure!(
        options.working_memory_bytes > 0,
        "resampling memory budget must be positive"
    );
    refuse_same_file(input, output)?;
    let bytes_before = std::fs::metadata(input)
        .with_context(|| format!("cannot read '{}'", input.display()))?
        .len();
    let mut doc =
        Document::load(input).with_context(|| format!("failed to parse '{}'", input.display()))?;
    if doc.was_encrypted() {
        bail!(
            "'{}' is encrypted; rewriting it would drop its protection",
            input.display()
        );
    }

    let pages = doc.get_pages().len();
    let placements = placement::collect(&doc);
    let ids = image_ids(&doc);
    let mut unchanged = BTreeMap::new();
    let edits = {
        let mut jobs = Vec::new();
        for &id in &ids {
            match placements
                .images
                .get(&id)
                .ok_or(SkipReason::NotPlaced)
                .and_then(|p| plan(&doc, id, p, options))
            {
                Ok(job) => jobs.push(job),
                Err(reason) => *unchanged.entry(reason).or_default() += 1,
            }
        }
        let mut edits = Vec::new();
        for result in process_all(&jobs, options.jpeg_quality, options.working_memory_bytes) {
            match result {
                Ok(edit) => edits.push(edit),
                Err(reason) => *unchanged.entry(reason).or_default() += 1,
            }
        }
        keep_matte_pairs_whole(&doc, &ids, edits, &mut unchanged)
    };
    let mut downsampled: Vec<DownsampledImage> =
        edits.iter().map(|edit| edit.image.clone()).collect();
    for edit in edits {
        apply(&mut doc, edit);
    }

    // The input's own bytes are copied, not re-serialised, whenever rewriting
    // saved nothing, so the output is never larger than the input.
    let rewritten = if downsampled.is_empty() {
        None
    } else {
        Some(save(&mut doc, pages)?).filter(|bytes| (bytes.len() as u64) < bytes_before)
    };
    drop(doc);
    let (written, bytes_after) = match rewritten {
        Some(bytes) => {
            std::fs::write(output, &bytes)
                .with_context(|| format!("cannot write '{}'", output.display()))?;
            (Written::Downsampled, bytes.len() as u64)
        }
        None => {
            std::fs::copy(input, output)
                .with_context(|| format!("cannot write '{}'", output.display()))?;
            // The output holds none of the resampled images.
            if !downsampled.is_empty() {
                *unchanged.entry(SkipReason::NoGain).or_default() += downsampled.len();
                downsampled.clear();
            }
            (Written::Original, bytes_before)
        }
    };

    Ok(DownsampleReport {
        input: input.display().to_string(),
        output: output.display().to_string(),
        pages,
        target_dpi: options.target_dpi,
        threshold_dpi: options.threshold_dpi,
        jpeg_quality: options.jpeg_quality,
        bytes_before,
        bytes_after,
        written,
        images: ImageCounts {
            total: ids.len(),
            downsampled: downsampled.len(),
            unchanged: ids.len() - downsampled.len(),
        },
        downsampled,
        unchanged,
        warnings: placements
            .unreadable_pages
            .iter()
            .map(|page| {
                format!(
                    "page {page}: content stream could not be parsed; its images are left unchanged"
                )
            })
            .collect(),
    })
}

/// Drops any resample that would leave an image and its `/Matte` soft mask at
/// different pixel sizes.
///
/// `/Matte` means the image's colours are pre-blended against a matte colour,
/// which a reader undoes pixel by pixel, so the specification requires the mask
/// to match the image exactly. Both are sized from the same placement, so they
/// only diverge when one of the two is left as it is (no gain, an unsupported
/// filter): then the other is left too, and counted as `matte_pair`.
fn keep_matte_pairs_whole(
    doc: &Document,
    ids: &[ObjectId],
    edits: Vec<Edit>,
    unchanged: &mut BTreeMap<SkipReason, usize>,
) -> Vec<Edit> {
    let resampled: HashMap<ObjectId, [u32; 2]> = edits
        .iter()
        .map(|edit| (edit.id, edit.image.to_px))
        .collect();
    let mut split = HashSet::new();
    for &image in ids {
        if let Some(mask) = matte_mask(doc, image)
            && resampled.get(&image) != resampled.get(&mask)
        {
            split.insert(image);
            split.insert(mask);
        }
    }
    edits
        .into_iter()
        .filter(|edit| {
            let keep = !split.contains(&edit.id);
            if !keep {
                *unchanged.entry(SkipReason::MattePair).or_default() += 1;
            }
            keep
        })
        .collect()
}

/// The soft mask of an image, when that mask declares `/Matte`.
fn matte_mask(doc: &Document, image: ObjectId) -> Option<ObjectId> {
    let stream = doc.get_object(image).and_then(Object::as_stream).ok()?;
    let mask = stream
        .dict
        .get(b"SMask")
        .and_then(Object::as_reference)
        .ok()?;
    let mask_stream = doc.get_object(mask).and_then(Object::as_stream).ok()?;
    mask_stream.dict.has(b"Matte").then_some(mask)
}

/// Every image XObject in the document, placed or not, in object order.
fn image_ids(doc: &Document) -> Vec<ObjectId> {
    doc.objects
        .iter()
        .filter(|(_, object)| {
            object.as_stream().is_ok_and(|s| {
                s.dict.get(b"Subtype").and_then(Object::as_name).ok() == Some(b"Image")
            })
        })
        .map(|(id, _)| *id)
        .collect()
}

/// Working memory the parallel resamples may hold at once.
const PARALLEL_BYTES_BUDGET: u64 = 2 << 30;

/// Working memory one resample needs, per declared sample byte: the decoded
/// samples, the inflated stream they are cut from, and the resized copy,
/// which is never larger than the original.
const WORKING_BYTES_PER_SAMPLE: u64 = 3;

/// An image the dictionary says to resample, and the size to resample it to.
struct Job<'d> {
    id: ObjectId,
    stream: &'d Stream,
    declared: codec::Declared,
    placement: &'d placement::Placement,
    to: [u32; 2],
    effective_dpi: f64,
}

/// A resampled image, ready to replace its stream.
struct Edit {
    id: ObjectId,
    content: Vec<u8>,
    encoding: Encoding,
    image: DownsampledImage,
}

/// Decides from the dictionary alone whether an image is resampled and to
/// what size, so an image that stays as it is is never decoded.
fn plan<'d>(
    doc: &'d Document,
    id: ObjectId,
    placement: &'d placement::Placement,
    options: &DownsampleOptions,
) -> Result<Job<'d>, SkipReason> {
    let stream = doc
        .get_object(id)
        .and_then(Object::as_stream)
        .map_err(|_| SkipReason::DecodeFailed)?;
    let declared = codec::declared(doc, stream)?;
    let (width, height) = (declared.width, declared.height);
    let effective_dpi =
        (f64::from(width) * f64::from(height) / (placement.inches_x * placement.inches_y)).sqrt();
    let target = f64::from(options.target_dpi);
    let to = [
        target_pixels(placement.inches_x, target, width),
        target_pixels(placement.inches_y, target, height),
    ];
    if effective_dpi <= f64::from(options.threshold_dpi) || to == [width, height] {
        return Err(SkipReason::BelowThreshold);
    }
    Ok(Job {
        id,
        stream,
        declared,
        placement,
        to,
        effective_dpi,
    })
}

/// Resamples every job, in parallel, in batches whose decoded size stays
/// within `budget`. Oversized individual jobs are skipped without decoding.
/// Results come back in job order.
fn process_all(jobs: &[Job<'_>], jpeg_quality: u8, budget: u64) -> Vec<Result<Edit, SkipReason>> {
    let mut results = Vec::with_capacity(jobs.len());
    let mut start = 0;
    while start < jobs.len() {
        let cost = |job: &Job<'_>| job.declared.bytes() * WORKING_BYTES_PER_SAMPLE;
        if cost(&jobs[start]) > budget {
            results.push(Err(SkipReason::TooLarge));
            start += 1;
            continue;
        }
        let mut end = start;
        let mut bytes = 0;
        while end < jobs.len() && cost(&jobs[end]) <= budget.saturating_sub(bytes) {
            bytes += cost(&jobs[end]);
            end += 1;
        }
        results.par_extend(
            jobs[start..end]
                .par_iter()
                .map(|job| process(job, jpeg_quality)),
        );
        start = end;
    }
    results
}

/// Decodes, resamples and re-encodes one image, or says why it is left alone.
fn process(job: &Job<'_>, jpeg_quality: u8) -> Result<Edit, SkipReason> {
    let (samples, storage) = codec::decode(job.stream, job.declared)?;
    let bytes_before = job.stream.content.len();
    let [to_w, to_h] = job.to;
    let content = codec::encode(samples, job.declared, storage, to_w, to_h, jpeg_quality)
        .map_err(|_| SkipReason::DecodeFailed)?;
    if content.len() >= bytes_before {
        return Err(SkipReason::NoGain);
    }
    let encoding = storage.encoding();
    Ok(Edit {
        id: job.id,
        image: DownsampledImage {
            object: format!("{} {}", job.id.0, job.id.1),
            pages: job.placement.pages.iter().copied().collect(),
            from_px: [job.declared.width, job.declared.height],
            to_px: job.to,
            effective_dpi: (job.effective_dpi * 10.0).round() / 10.0,
            bytes_before,
            bytes_after: content.len(),
            encoding,
        },
        content,
        encoding,
    })
}

/// Replaces an image stream with its resampled content.
fn apply(doc: &mut Document, edit: Edit) {
    let Ok(Object::Stream(stream)) = doc.get_object_mut(edit.id) else {
        unreachable!("object {:?} was read as a stream when planned", edit.id);
    };
    let [width, height] = edit.image.to_px;
    stream.dict.set("Width", i64::from(width));
    stream.dict.set("Height", i64::from(height));
    stream
        .dict
        .set("Filter", Object::Name(edit.encoding.filter().to_vec()));
    stream.dict.remove(b"DecodeParms");
    stream.set_content(edit.content);
}

/// Pixels needed along one axis, never more than the image already has.
fn target_pixels(inches: f64, dpi: f64, current: u32) -> u32 {
    let wanted = (inches * dpi).ceil();
    if wanted >= f64::from(current) {
        current
    } else {
        (wanted as u32).max(1)
    }
}

/// Serialises the document and proves the result opens with every page.
///
/// A source packed into object streams is written back packed. Written out
/// one object at a time, a Distiller file of 15 000 small objects grew by more
/// than resampling its images saved.
fn save(doc: &mut Document, pages: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let packed = matches!(
        doc.reference_table.cross_reference_type,
        lopdf::xref::XrefType::CrossReferenceStream
    );
    if packed {
        doc.save_modern(&mut bytes)
    } else {
        doc.save_to(&mut bytes)
    }
    .context("failed to serialise the resampled document")?;
    let reopened = Document::load_mem(&bytes).context("the resampled document does not parse")?;
    if reopened.get_pages().len() != pages {
        bail!(
            "the resampled document has {} pages, expected {pages}",
            reopened.get_pages().len()
        );
    }
    Ok(bytes)
}

fn refuse_same_file(input: &Path, output: &Path) -> Result<()> {
    if let (Ok(a), Ok(b)) = (input.canonicalize(), output.canonicalize())
        && a == b
    {
        bail!(
            "output '{}' is the input; write to a different file",
            output.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod budget_tests {
    use super::*;
    use lopdf::dictionary;

    #[test]
    fn oversized_job_is_skipped_without_decoding_invalid_stream() {
        let doc = Document::new();
        let stream = Stream::new(
            lopdf::dictionary! {
                "Width" => 1000, "Height" => 1000, "BitsPerComponent" => 8,
                "ColorSpace" => "DeviceRGB", "Filter" => "DCTDecode",
            },
            Vec::new(),
        );
        let declared = codec::declared(&doc, &stream).expect("valid declaration");
        let placement = placement::Placement::default();
        let job = Job {
            id: (1, 0),
            stream: &stream,
            declared,
            placement: &placement,
            to: [100, 100],
            effective_dpi: 1000.0,
        };
        let results = process_all(&[job], 85, 1);
        assert!(matches!(results.as_slice(), [Err(SkipReason::TooLarge)]));
        assert!(process_all(&[], 85, 1).is_empty());
    }
}
