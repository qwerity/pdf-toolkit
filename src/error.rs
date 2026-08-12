use pdfium_render::prelude::PdfiumError;
use std::io;
use std::path::PathBuf;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, PdfCutterError>;

/// Errors returned by the reusable extraction library.
#[derive(Debug, Error)]
pub enum PdfCutterError {
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
