pub mod cli;
mod document;
pub mod error;
mod extractor;
mod output;
mod page_selection;
mod pdfium_runtime;

pub use document::document_page_count;
pub use extractor::{ExtractionReport, ExtractionWarning, extract_pages};
pub use output::{OutputFormat, PageResult};
pub use page_selection::PageSelection;
pub use pdfium_runtime::create_pdfium;
