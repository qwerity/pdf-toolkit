//! Cutting pages out of a document.
//!
//! One entry point, [`extract_pages`], over three shapes a caller might want:
//! each selected page as its own single-page PDF, the whole selection merged
//! into one PDF, or each page rendered to PNG. The rendering itself belongs to
//! `pdf-render`; what this crate adds is the selection, the naming, and
//! reporting per-page failures without abandoning the pages that worked.

pub mod extractor;
pub mod output;

pub use extractor::{ExtractionReport, extract_pages};
pub use output::{OutputFormat, PageResult};
