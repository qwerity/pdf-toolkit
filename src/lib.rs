pub mod cli;
mod document;
pub mod error;
mod extractor;
mod output;
mod page_selection;
mod pdfium_runtime;
pub mod structured_text_extractor;
mod text_extractor;
mod text_quality;
mod utils;

pub use document::document_page_count;
pub use document::{DocumentInfo, inspect_document};
pub use extractor::{ExtractionReport, ExtractionWarning, extract_pages};
pub use output::{LlmReason, OutputFormat, PageInfo, PageResult, PngSize};
pub use page_selection::PageSelection;
pub use pdfium_runtime::create_pdfium;
pub use structured_text_extractor::{
    StructuredExtractionReport, StructuredOutputFormat, StructuredPage, TextSpan,
    extract_structured_text,
};
pub use text_extractor::{TextExtractionReport, TextFileResult, extract_text};
