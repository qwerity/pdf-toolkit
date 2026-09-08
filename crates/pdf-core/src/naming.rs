//! Choosing and reserving the files a run writes.
//!
//! Reservation is create-new rather than check-then-write: the name is claimed
//! by the same call that opens the file, so two runs sharing an output
//! directory cannot both decide a name is free. That is why every function
//! here hands back an open [`OutputReservation`] instead of a path.
//!
//! The extension is passed as a string rather than an output-format enum
//! because the formats belong to the crates that produce them — text, PDF and
//! PNG all reserve files, and none of them needs to know about the others.

use std::fs::{File, OpenOptions};
use std::num::NonZeroU16;
use std::path::{Path, PathBuf};

use crate::error::{PdfCutterError, Result};
use crate::utils::sanitize_filename_component;

const MAX_OUTPUT_NAME_ATTEMPTS: u16 = 1024;

/// A claimed output path and the handle that claimed it.
pub struct OutputReservation {
    pub path: PathBuf,
    pub file: File,
}

pub fn resolve_output_dir(output_folder: Option<&Path>) -> Result<PathBuf> {
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

/// The output name stem for a source document, with anything awkward in a
/// file name replaced.
pub fn output_stem(input_path: &Path) -> String {
    sanitize_filename_component(input_path.file_stem())
}

pub fn reserve_output_file_with_extension(
    output_dir: &Path,
    stem: &str,
    page_number: NonZeroU16,
    extension: &str,
) -> Result<OutputReservation> {
    let base_name = format!("{stem}_page_{}", page_number.get());

    reserve_output_file_for_base_name(output_dir, &base_name, extension)
}

pub fn reserve_named_output_file_with_extension(
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

pub fn file_size_kb(file: &File, path: &Path) -> Result<u64> {
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
