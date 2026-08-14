use crate::error::{PdfCutterError, Result};
use crate::utils::sanitize_filename_component;
use std::fs::{File, OpenOptions};
use std::num::NonZeroU16;
use std::path::{Path, PathBuf};

const MAX_OUTPUT_NAME_ATTEMPTS: u16 = 1024;
const MAX_IMAGE_WIDTH_PX: u32 = i32::MAX as u32;

/// Metadata about a source PDF page.
#[derive(Clone, Debug, PartialEq)]
pub struct PageInfo {
    pub page_number: u16,
    pub width_px: u32,
    pub height_px: u32,
    pub contains_images: bool,
    /// Total number of image objects on the page (including inside Form XObjects).
    pub image_count: u32,
    /// Estimated fraction of page area covered by top-level image objects (0.0–1.0).
    pub image_coverage: f32,
    /// Reasons a vision-capable LLM/OCR pass is likely to recover content
    /// that text extraction cannot. Empty when text extraction suffices.
    pub llm_reasons: Vec<LlmReason>,
}

impl PageInfo {
    /// True when a vision-capable LLM/OCR pass is recommended for this page.
    pub fn llm_recommended(&self) -> bool {
        !self.llm_reasons.is_empty()
    }
}

/// Why a page benefits from a vision-capable LLM/OCR pass on top of (or
/// instead of) text-layer extraction.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LlmReason {
    /// The page has images but (almost) no extractable text — likely a scan.
    NoTextLayer,
    /// A significant share of the page area is covered by embedded images,
    /// whose contents (diagram labels, figure text) are invisible to the
    /// text layer.
    ImageHeavy,
    /// The text layer exists but is statistically garbled (e.g. a broken
    /// CID font mapping); rendered pixels are the only reliable source.
    UnreadableText,
}

impl LlmReason {
    /// Stable machine-readable identifier used in JSON output.
    pub fn slug(self) -> &'static str {
        match self {
            Self::NoTextLayer => "no_text_layer",
            Self::ImageHeavy => "image_heavy",
            Self::UnreadableText => "unreadable_text",
        }
    }

    /// Human-readable explanation.
    pub fn description(self) -> &'static str {
        match self {
            Self::NoTextLayer => "page has images but no usable text layer; likely a scanned page",
            Self::ImageHeavy => {
                "large image coverage; text inside embedded images is not extractable"
            }
            Self::UnreadableText => {
                "text layer appears garbled; rendered pixels are the reliable source"
            }
        }
    }
}

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

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum PngSize {
    Scale(f32),
    WidthPx(u32),
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum OutputFormat {
    Pdf { concat: bool },
    Png { size: PngSize },
}

impl OutputFormat {
    pub(crate) fn extension(self) -> &'static str {
        match self {
            Self::Pdf { .. } => "pdf",
            Self::Png { .. } => "png",
        }
    }

    pub fn validate(self) -> Result<()> {
        match self {
            Self::Pdf { .. } => Ok(()),
            Self::Png {
                size: PngSize::Scale(scale),
            } if scale.is_finite() && scale > 0.0 => Ok(()),
            Self::Png {
                size: PngSize::Scale(scale),
            } => Err(PdfCutterError::InvalidImageScale { value: scale }),
            Self::Png {
                size: PngSize::WidthPx(width_px),
            } if (1..=MAX_IMAGE_WIDTH_PX).contains(&width_px) => Ok(()),
            Self::Png {
                size: PngSize::WidthPx(width_px),
            } => Err(PdfCutterError::InvalidImageWidth {
                value: width_px,
                max: MAX_IMAGE_WIDTH_PX,
            }),
        }
    }
}

pub(crate) struct OutputReservation {
    pub(crate) path: PathBuf,
    pub(crate) file: File,
}

pub(crate) fn resolve_output_dir(output_folder: Option<&Path>) -> Result<PathBuf> {
    match output_folder {
        Some(folder) => {
            std::fs::create_dir_all(folder).map_err(|source| {
                PdfCutterError::CreateOutputDirectory {
                    path: folder.to_path_buf(),
                    source,
                }
            })?;
            Ok(folder.to_path_buf())
        }
        None => {
            std::env::current_dir().map_err(|source| PdfCutterError::CurrentDirectory { source })
        }
    }
}

pub(crate) fn output_stem(input_path: &Path) -> String {
    sanitize_filename_component(input_path.file_stem())
}

pub(crate) fn reserve_output_file(
    output_dir: &Path,
    stem: &str,
    page_number: NonZeroU16,
    output_format: OutputFormat,
) -> Result<OutputReservation> {
    reserve_output_file_with_extension(output_dir, stem, page_number, output_format.extension())
}

pub(crate) fn reserve_named_output_file(
    output_dir: &Path,
    base_name: &str,
    output_format: OutputFormat,
) -> Result<OutputReservation> {
    reserve_named_output_file_with_extension(output_dir, base_name, output_format.extension())
}

pub(crate) fn reserve_output_file_with_extension(
    output_dir: &Path,
    stem: &str,
    page_number: NonZeroU16,
    extension: &str,
) -> Result<OutputReservation> {
    let base_name = format!("{stem}_page_{}", page_number.get());

    reserve_output_file_for_base_name(output_dir, &base_name, extension)
}

pub(crate) fn reserve_named_output_file_with_extension(
    output_dir: &Path,
    base_name: &str,
    extension: &str,
) -> Result<OutputReservation> {
    reserve_output_file_for_base_name(output_dir, base_name, extension)
}

fn reserve_output_file_for_base_name(
    output_dir: &Path,
    base_name: &str,
    extension: &str,
) -> Result<OutputReservation> {
    for attempt in 0..=MAX_OUTPUT_NAME_ATTEMPTS {
        let file_name = match attempt {
            0 => format!("{base_name}.{extension}"),
            _ => format!("{base_name}_{attempt}.{extension}"),
        };
        let path = output_dir.join(file_name);

        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok(OutputReservation { path, file }),
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(PdfCutterError::CreateOutputFile { path, source });
            }
        }
    }

    Err(PdfCutterError::ExhaustedOutputNames {
        dir: output_dir.to_path_buf(),
        base_name: base_name.to_string(),
    })
}

pub(crate) fn file_size_kb(file: &File, path: &Path) -> Result<u64> {
    let metadata = file
        .metadata()
        .map_err(|source| PdfCutterError::ReadMetadata {
            path: path.to_path_buf(),
            source,
        })?;

    Ok(metadata.len().div_ceil(1024))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

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
    }

    #[test]
    fn validate_accepts_pdf_and_valid_png_sizes() {
        assert!(OutputFormat::Pdf { concat: true }.validate().is_ok());
        for size in [
            PngSize::Scale(0.5),
            PngSize::Scale(2.0),
            PngSize::WidthPx(1),
        ] {
            assert!(OutputFormat::Png { size }.validate().is_ok());
        }
        assert!(
            OutputFormat::Png {
                size: PngSize::WidthPx(MAX_IMAGE_WIDTH_PX)
            }
            .validate()
            .is_ok()
        );
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
                    Err(PdfCutterError::InvalidImageScale { .. })
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
                    Err(PdfCutterError::InvalidImageWidth { .. })
                ),
                "width {width_px} must be rejected"
            );
        }
    }

    #[test]
    fn output_stem_sanitizes_file_name() {
        assert_eq!(output_stem(Path::new("/tmp/My Doc (1).pdf")), "My_Doc_1");
        assert_eq!(output_stem(Path::new("/")), "document");
    }

    #[test]
    fn reserve_uses_page_number_in_file_name() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let reservation = reserve_output_file_with_extension(
            dir.path(),
            "doc",
            NonZeroU16::new(3).unwrap(),
            "txt",
        )
        .expect("reserve output file");
        assert_eq!(reservation.path, dir.path().join("doc_page_3.txt"));
        assert!(reservation.path.exists());
    }

    #[test]
    fn reserve_appends_counter_on_collision() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let first = reserve_named_output_file_with_extension(dir.path(), "base", "pdf")
            .expect("first reservation");
        let second = reserve_named_output_file_with_extension(dir.path(), "base", "pdf")
            .expect("second reservation");
        assert_eq!(first.path, dir.path().join("base.pdf"));
        assert_eq!(second.path, dir.path().join("base_1.pdf"));
    }

    #[test]
    fn reserve_fails_when_names_are_exhausted() {
        let dir = tempfile::tempdir().expect("create temp dir");
        File::create(dir.path().join("base.pdf")).expect("create base file");
        for attempt in 1..=MAX_OUTPUT_NAME_ATTEMPTS {
            File::create(dir.path().join(format!("base_{attempt}.pdf")))
                .expect("create collision file");
        }

        let result = reserve_named_output_file_with_extension(dir.path(), "base", "pdf");
        assert!(matches!(
            result,
            Err(PdfCutterError::ExhaustedOutputNames { .. })
        ));
    }

    #[test]
    fn resolve_output_dir_creates_nested_directories() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let nested = dir.path().join("a").join("b");
        let resolved = resolve_output_dir(Some(&nested)).expect("resolve output dir");
        assert_eq!(resolved, nested);
        assert!(nested.is_dir());
    }

    #[test]
    fn resolve_output_dir_defaults_to_current_dir() {
        let resolved = resolve_output_dir(None).expect("resolve output dir");
        assert_eq!(resolved, std::env::current_dir().expect("current dir"));
    }

    #[test]
    fn file_size_kb_rounds_up() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let cases: [(usize, u64); 4] = [(0, 0), (1, 1), (1024, 1), (1025, 2)];

        for (bytes, expected_kb) in cases {
            let path = dir.path().join(format!("file_{bytes}.bin"));
            let mut file = File::create(&path).expect("create file");
            file.write_all(&vec![0u8; bytes]).expect("write bytes");
            assert_eq!(file_size_kb(&file, &path).expect("file size"), expected_kb);
        }
    }
}
