//! Local OCR of page images with RapidOCR's ONNX models (PaddleOCR's
//! detection, text-line orientation and recognition networks), run in process
//! through ONNX Runtime.
//!
//! OCR is for pages whose text layer cannot be read: a scan with no text, or
//! with a scanner's own hidden OCR layer that garbles digits. It never runs
//! unless a caller asks for it, and it never reaches the network: the models
//! live in a local directory, filled once by [`fetch_models`] (checksummed)
//! or provisioned by deployment.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use image::RgbImage;
use rapidocr_core::RapidOcr;
use rapidocr_core::config::InferenceOptions;
use rapidocr_core::model::{ModelCache, ModelDownloadMode, ModelSetSpec, model_set_by_name};

/// The model set used: PP-OCRv5's English mobile networks, small enough for
/// CPU and trained on the Latin script zoning documents are printed in.
pub const MODEL_SET: &str = "ppocrv5-en-mobile";

/// Environment variable naming the models directory when no path is given.
pub const MODELS_ENV: &str = "PDF_TOOLKIT_OCR_MODELS";

/// Directory beside the executable a release bundle ships the models in,
/// found the way PDFium is found beside it.
pub const BUNDLED_DIR: &str = "ocr-models";

/// Environment variable setting how many threads OCR uses.
pub const THREADS_ENV: &str = "PDF_TOOLKIT_OCR_THREADS";

/// Threads OCR uses by default. Past about this many the networks gain
/// little, a machine's efficiency cores only slow them, and a batch running
/// several extractions at once needs cores left for the others.
const DEFAULT_THREADS: usize = 8;

/// One line of text found in an image.
#[derive(Clone, Debug, PartialEq)]
pub struct OcrLine {
    /// The recognised text.
    pub text: String,
    /// The line's box in image pixels: left, top, right, bottom.
    pub bounds: [f32; 4],
}

/// A loaded OCR pipeline. Loading the ONNX sessions is the expensive part,
/// so one engine serves every page of a run.
pub struct Ocr {
    runner: RapidOcr,
    max_side: u32,
}

impl Ocr {
    /// Loads the pipeline from `models` (a directory holding [`MODEL_SET`]'s
    /// files), verifying each file's checksum.
    ///
    /// # Errors
    /// When a model file is missing or corrupt, or ONNX Runtime cannot load it.
    pub fn load(models: &Path) -> Result<Self> {
        let set = model_set()?;
        let cache = ModelCache::new(models);
        cache
            .ensure_model_set(set, ModelDownloadMode::Never)
            .with_context(|| {
                format!(
                    "OCR models missing or corrupt in {}; fetch them with `pdf-toolkit ocr-models {}`",
                    models.display(),
                    models.display()
                )
            })?;
        // ONNX Runtime runs one thread unless told otherwise.
        let threads = std::env::var(THREADS_ENV)
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|&n| n > 0)
            .unwrap_or_else(|| {
                std::thread::available_parallelism()
                    .map_or(1, usize::from)
                    .min(DEFAULT_THREADS)
            });
        let config = cache
            .config_for(set)
            .with_inference_options(InferenceOptions {
                intra_threads: threads,
                ..InferenceOptions::default()
            });
        let max_side = config.max_side_len;
        let runner = RapidOcr::new(config).context("loading the OCR models")?;
        Ok(Self { runner, max_side })
    }

    /// The longest side, in pixels, the engine reads an image at: a larger
    /// image is shrunk to it first, so rendering larger only wastes work.
    pub fn max_side(&self) -> u32 {
        self.max_side
    }

    /// The lines of text in `image`, top to bottom.
    ///
    /// # Errors
    /// When ONNX Runtime fails on the image.
    pub fn read(&mut self, image: &RgbImage) -> Result<Vec<OcrLine>> {
        let output = self.runner.run_image(image).context("running OCR")?;
        Ok(output
            .lines
            .into_iter()
            .filter(|line| !line.text.trim().is_empty())
            .map(|line| OcrLine {
                bounds: line.bbox.points.iter().fold(
                    [f32::MAX, f32::MAX, f32::MIN, f32::MIN],
                    |[l, t, r, b], p| [l.min(p[0]), t.min(p[1]), r.max(p[0]), b.max(p[1])],
                ),
                text: line.text,
            })
            .collect())
    }
}

/// The models directory: `explicit` when given, else [`MODELS_ENV`] when
/// set and not empty, else [`BUNDLED_DIR`] beside the executable.
pub fn models_dir(explicit: Option<&Path>) -> Option<PathBuf> {
    explicit
        .map(Path::to_path_buf)
        .or_else(|| {
            std::env::var_os(MODELS_ENV)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        })
        .or_else(bundled_dir)
}

fn bundled_dir() -> Option<PathBuf> {
    let dir = std::env::current_exe().ok()?.parent()?.join(BUNDLED_DIR);
    dir.is_dir().then_some(dir)
}

/// Checks that `models` holds every file of [`MODEL_SET`], without loading
/// them: a quick check before a run whose pages may need OCR.
///
/// # Errors
/// When a file is missing.
pub fn check_models(models: &Path) -> Result<()> {
    let missing = ModelCache::new(models).missing_assets(model_set()?);
    anyhow::ensure!(
        missing.is_empty(),
        "OCR models missing in {} ({}); fetch them with `pdf-toolkit ocr-models {}`",
        models.display(),
        missing
            .iter()
            .map(|a| a.filename)
            .collect::<Vec<_>>()
            .join(", "),
        models.display()
    );
    Ok(())
}

/// Downloads any of [`MODEL_SET`]'s files missing from `models` and verifies
/// every file's checksum. The one networked step, run once per machine.
///
/// # Errors
/// When a download fails or a file does not match its checksum.
pub fn fetch_models(models: &Path) -> Result<Vec<String>> {
    let set = model_set()?;
    fs::create_dir_all(models).with_context(|| format!("creating {}", models.display()))?;
    let cache = ModelCache::new(models);
    let mut fetched = Vec::new();
    for asset in cache.missing_assets(set) {
        let target = cache.asset_path(asset);
        let partial = target.with_extension("part");
        let mut response = ureq::get(asset.url)
            .call()
            .with_context(|| format!("downloading {}", asset.url))?;
        let mut file = fs::File::create(&partial)
            .with_context(|| format!("creating {}", partial.display()))?;
        io::copy(&mut response.body_mut().as_reader(), &mut file)
            .with_context(|| format!("writing {}", partial.display()))?;
        fs::rename(&partial, &target).with_context(|| format!("saving {}", target.display()))?;
        fetched.push(asset.filename.to_string());
    }
    cache
        .ensure_model_set(set, ModelDownloadMode::Never)
        .context("verifying the OCR models")?;
    Ok(fetched)
}

fn model_set() -> Result<&'static ModelSetSpec> {
    model_set_by_name(MODEL_SET)
        .with_context(|| format!("RapidOCR has no model set named {MODEL_SET}"))
}
