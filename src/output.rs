use crate::error::{PdfCutterError, Result};
use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::num::NonZeroU16;
use std::path::{Path, PathBuf};

const MAX_OUTPUT_NAME_ATTEMPTS: u16 = 1024;

pub struct PageResult {
    pub output_path: PathBuf,
    pub width_mm: f32,
    pub height_mm: f32,
    pub size_kb: u64,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum OutputFormat {
    Pdf,
    Png { scale: f32 },
}

impl OutputFormat {
    pub(crate) fn extension(self) -> &'static str {
        match self {
            Self::Pdf => "pdf",
            Self::Png { .. } => "png",
        }
    }

    pub(crate) fn validate(self) -> Result<()> {
        match self {
            Self::Pdf => Ok(()),
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
        page_number: page_number.get(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_output_dir(prefix: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after UNIX_EPOCH")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "pdf_cutter_{prefix}_{}_{}",
            std::process::id(),
            unique
        ));
        fs::create_dir_all(&dir).expect("temporary output dir should be created");
        dir
    }

    #[test]
    fn test_output_stem_sanitizes_untrusted_input_names() {
        assert_eq!(
            output_stem(Path::new(" ./weird \n name?.pdf")),
            "weird_name"
        );
    }

    #[test]
    fn test_reserve_output_file_creates_a_unique_name_when_needed() {
        let output_dir = temp_output_dir("reserve_output");
        let page_number = NonZeroU16::new(1).expect("page number must be non-zero");

        let first = reserve_output_file(&output_dir, "input", page_number, OutputFormat::Pdf)
            .expect("first output file should be reserved");
        let second = reserve_output_file(&output_dir, "input", page_number, OutputFormat::Pdf)
            .expect("second output file should be reserved");

        assert_eq!(
            first.path.file_name().and_then(|name| name.to_str()),
            Some("input_page_1.pdf")
        );
        assert_eq!(
            second.path.file_name().and_then(|name| name.to_str()),
            Some("input_page_1_1.pdf")
        );

        drop(first);
        drop(second);
        let _ = fs::remove_dir_all(output_dir);
    }
}
