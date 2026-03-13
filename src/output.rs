use crate::error::{PdfCutterError, Result};
use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::num::NonZeroU16;
use std::path::{Path, PathBuf};

const MAX_OUTPUT_NAME_ATTEMPTS: u16 = 1024;

pub struct PageResult {
    pub output_path: PathBuf,
    pub page_count: usize,
    pub width_mm: f32,
    pub height_mm: f32,
    pub size_kb: u64,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum OutputFormat {
    Pdf { concat: bool },
    Png { scale: f32 },
}

impl OutputFormat {
    pub(crate) fn extension(self) -> &'static str {
        match self {
            Self::Pdf { .. } => "pdf",
            Self::Png { .. } => "png",
        }
    }

    pub(crate) fn validate(self) -> Result<()> {
        match self {
            Self::Pdf { .. } => Ok(()),
            Self::Png { scale } if scale.is_finite() && scale > 0.0 => Ok(()),
            Self::Png { scale } => Err(PdfCutterError::InvalidImageScale { value: scale }),
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
    let base_name = format!("{stem}_page_{}", page_number.get());

    reserve_output_file_for_base_name(output_dir, &base_name, output_format)
}

pub(crate) fn reserve_named_output_file(
    output_dir: &Path,
    base_name: &str,
    output_format: OutputFormat,
) -> Result<OutputReservation> {
    reserve_output_file_for_base_name(output_dir, base_name, output_format)
}

fn reserve_output_file_for_base_name(
    output_dir: &Path,
    base_name: &str,
    output_format: OutputFormat,
) -> Result<OutputReservation> {
    for attempt in 0..=MAX_OUTPUT_NAME_ATTEMPTS {
        let file_name = match attempt {
            0 => format!("{base_name}.{}", output_format.extension()),
            _ => format!("{base_name}_{attempt}.{}", output_format.extension()),
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

fn sanitize_filename_component(stem: Option<&OsStr>) -> String {
    let raw_stem = stem.and_then(OsStr::to_str).unwrap_or_default();
    let mut sanitized = String::with_capacity(raw_stem.len());
    let mut last_was_replacement = false;

    for ch in raw_stem.chars() {
        match ch {
            'A'..='Z' | 'a'..='z' | '0'..='9' => {
                sanitized.push(ch);
                last_was_replacement = false;
            }
            '.' | '_' | '-' if !sanitized.is_empty() => {
                sanitized.push(ch);
                last_was_replacement = false;
            }
            _ if !last_was_replacement => {
                sanitized.push('_');
                last_was_replacement = true;
            }
            _ => {}
        }
    }

    let trimmed = sanitized.trim_matches(|ch| matches!(ch, '.' | '_' | '-'));

    if trimmed.is_empty() {
        "document".to_string()
    } else {
        trimmed.to_string()
    }
}
