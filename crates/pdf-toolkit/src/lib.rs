//! PDF inspection and extraction, built on [pdfium-render].
//!
//! This crate is the CLI and a facade over the workspace: the operations live
//! in focused crates, and are re-exported here so a caller — or a test — has
//! one import to reach all of them.
//!
//! | crate | what it owns |
//! |---|---|
//! | [`pdf_core`] | the PDFium runtime, page addressing, output naming, errors and warnings |
//! | [`pdf_render`] | rasterising a page to PNG |
//! | [`pdf_text`] | plain and structured text extraction |
//! | [`pdf_inspect`] | per-page metadata and the vision-pass routing signals |
//! | [`pdf_pages`] | cutting pages to single PDFs, one merged PDF, or PNGs |
//!
//! The operations, over a shared [`Pdfium`] runtime obtained once via
//! [`create_pdfium`]:
//!
//! - [`inspect_document`] / [`document_page_count`] — page count and per-page
//!   metadata ([`PageInfo`]): dimensions, image statistics, and whether a
//!   vision-LLM/OCR pass is recommended ([`LlmReason`]).
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
//! whole-document problems are [`error::PdfError`]s.
//!
//! [pdfium-render]: https://crates.io/crates/pdfium-render

pub mod cli;

pub use pdf_core::error;
pub use pdf_core::{ExtractionWarning, PageSelection, Pdfium, create_pdfium, create_pdfium_at};
pub use pdf_inspect::{DocumentInfo, LlmReason, PageInfo, document_page_count, inspect_document};
pub use pdf_pages::{ExtractionReport, OutputFormat, PageResult, extract_pages};
pub use pdf_render::PngSize;
pub use pdf_text::{
    Ocr, OcrOptions, OcrPages, StructuredExtractionReport, StructuredOutputFormat, StructuredPage,
    TextExtractionReport, TextFileResult, TextSpan, extract_structured_text,
    extract_structured_text_with, extract_text,
};
