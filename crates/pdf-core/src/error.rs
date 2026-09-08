use pdfium_render::prelude::PdfiumError;
use std::io;
use std::path::PathBuf;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, PdfError>;

/// Errors that abort an operation.
///
/// Per-page problems that do not abort a run are [`crate::warning::ExtractionWarning`]s
/// instead, and are reported beside the results rather than in place of them.
#[derive(Debug, Error)]
pub enum PdfError {
    #[error("input PDF '{path}' does not exist", path = .0.display())]
    InputMissing(PathBuf),
    #[error("input path '{path}' is not a file", path = .0.display())]
    InputNotFile(PathBuf),
    #[error("page selection cannot be empty")]
    EmptyPageSelection,
    #[error("page selection entry #{position} ('{value}') is invalid; expected a positive integer")]
    InvalidPageSelectionToken { position: usize, value: String },
    #[error("page selection entry #{position} ('{value}') is invalid; ranges must be ascending")]
    InvalidPageSelectionRange { position: usize, value: String },
    #[error("image scale must be a finite number greater than zero, got {value}")]
    InvalidImageScale { value: f32 },
    #[error("image width must be an integer in 1..={max}, got {value}")]
    InvalidImageWidth { value: u32, max: u32 },
    #[error("the document contains no pages")]
    EmptyDocument,
    #[error(
        "the document reports an unsupported page count ({actual}); at most {max} pages are supported",
        max = u16::MAX
    )]
    UnsupportedPageCount { actual: i32 },
    #[error(
        "failed to bind to pdfium library; checked the executable directory, system library, ./, ./dist/"
    )]
    PdfiumBinding,
    #[error("failed to open PDF file '{path}': {source}", path = .path.display())]
    OpenPdf {
        path: PathBuf,
        #[source]
        source: PdfiumError,
    },
    #[error("failed to load page {page_number} from '{path}': {source}", path = .path.display())]
    LoadPage {
        path: PathBuf,
        page_number: u16,
        #[source]
        source: PdfiumError,
    },
    #[error("failed to get current working directory: {source}")]
    CurrentDirectory {
        #[source]
        source: io::Error,
    },
    #[error("failed to create output directory '{path}': {source}", path = .path.display())]
    CreateOutputDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to create output file '{path}': {source}", path = .path.display())]
    CreateOutputFile {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not find an available output filename for '{base_name}' in '{dir}'", dir = .dir.display())]
    ExhaustedOutputNames { dir: PathBuf, base_name: String },
    #[error("failed to read metadata for '{path}': {source}", path = .path.display())]
    ReadMetadata {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to create output PDF for '{path}': {source}", path = .path.display())]
    CreateOutputPdf {
        path: PathBuf,
        #[source]
        source: PdfiumError,
    },
    #[error("failed to copy source page {page_number} into '{path}': {source}", path = .path.display())]
    CopyPage {
        page_number: u16,
        path: PathBuf,
        #[source]
        source: PdfiumError,
    },
    #[error("failed to save PDF to '{path}': {source}", path = .path.display())]
    SavePdf {
        path: PathBuf,
        #[source]
        source: PdfiumError,
    },
    #[error("failed to write text to '{path}': {source}", path = .path.display())]
    WriteText {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to render page as an image: {source}")]
    RenderPage {
        #[source]
        source: PdfiumError,
    },
    #[error("failed to save {format} to '{path}': {source}", path = .path.display())]
    SaveImage {
        format: &'static str,
        path: PathBuf,
        #[source]
        source: image::ImageError,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The messages reach a user on stderr and a caller through the `errors`
    /// object, so they are part of the surface rather than debug output. This
    /// pins the vocabulary a reader relies on: what failed, and where.
    #[test]
    fn messages_name_the_thing_that_failed() {
        let cases: Vec<(PdfError, &[&str])> = vec![
            (
                PdfError::InputMissing(PathBuf::from("missing.pdf")),
                &["missing.pdf", "does not exist"],
            ),
            (
                PdfError::InputNotFile(PathBuf::from("some/dir")),
                &["some/dir", "not a file"],
            ),
            (PdfError::EmptyPageSelection, &["cannot be empty"]),
            (
                PdfError::InvalidPageSelectionToken {
                    position: 2,
                    value: "abc".into(),
                },
                &["#2", "abc", "positive integer"],
            ),
            (
                PdfError::InvalidPageSelectionRange {
                    position: 1,
                    value: "9-3".into(),
                },
                &["#1", "9-3", "ascending"],
            ),
            (
                PdfError::InvalidImageScale { value: -1.0 },
                &["greater than zero", "-1"],
            ),
            (
                PdfError::InvalidImageWidth {
                    value: 0,
                    max: 10_000,
                },
                &["1..=10000", "0"],
            ),
            (PdfError::EmptyDocument, &["no pages"]),
            (
                PdfError::UnsupportedPageCount { actual: 99_999 },
                &["99999", "65535"],
            ),
            (
                PdfError::ExhaustedOutputNames {
                    dir: PathBuf::from("out"),
                    base_name: "doc".into(),
                },
                &["doc", "out"],
            ),
        ];

        for (error, expected) in cases {
            let rendered = error.to_string();
            for fragment in expected {
                assert!(
                    rendered.contains(fragment),
                    "expected {fragment:?} in {rendered:?}"
                );
            }
        }
    }

    /// The binding failure is the one a user hits on a machine without the
    /// native library, so it must list every place that was searched.
    #[test]
    fn binding_failure_lists_every_location_searched() {
        let rendered = PdfError::PdfiumBinding.to_string();

        for location in ["executable directory", "system library", "./", "./dist/"] {
            assert!(
                rendered.contains(location),
                "expected {location:?} in {rendered:?}"
            );
        }
    }
}
