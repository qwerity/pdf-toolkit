//! PDF inspection and extraction built on [pdfium-render].
//!
//! The library exposes four operations over a shared [`Pdfium`] runtime
//! (obtained once via [`create_pdfium`]):
//!
//! - [`inspect_document`] / [`document_page_count`] — page count and
//!   per-page metadata ([`PageInfo`]): dimensions, image statistics, and
//!   whether a vision-LLM/OCR pass is recommended ([`LlmReason`]).
//! - [`extract_pages`] — cut selected pages into single-page PDFs, one
//!   concatenated PDF, or rendered PNGs ([`OutputFormat`], [`PngSize`]).
//! - [`extract_text`] — plain text into `.txt` files with per-page markers.
//! - [`extract_structured_text`] — per-page text spans (font, position,
//!   style) plus a markdown reconstruction in visual reading order, with
//!   table, column, and rotated-page handling.
//!
//! Pages are addressed one-based through [`PageSelection`] (`"all"` or
//! lists/ranges like `"1,3,5-7"`). Per-page failures surface as
//! [`ExtractionWarning`]s in each report rather than aborting the run;
//! whole-document problems are [`error::PdfCutterError`]s.
//!
//! [pdfium-render]: https://crates.io/crates/pdfium-render
//! [`Pdfium`]: pdfium_render::prelude::Pdfium

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
