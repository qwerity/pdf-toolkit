//! What a document's pages contain, before anything is extracted from them.
//!
//! [`inspect_document`] is the cheap first pass: page dimensions, image
//! statistics, and — the part that decides what a pipeline does next —
//! [`LlmReason`]s saying where the text layer will not be enough and rendered
//! pixels are the only reliable source.
//!
//! That routing signal is the point. Deterministic extraction is exact and
//! free; a vision model is neither. Naming the pages that actually need one,
//! and why, is what keeps a hybrid pipeline from paying for the whole
//! document.
//!
//! This crate depends on `pdf-text` because one of those reasons cannot be
//! judged any other way: `complex_table` asks whether the page holds a table
//! whose cells wrap across three or more columns, and answering it means
//! running the structured reconstruction.

mod chart;
pub mod document;
pub mod page_info;

pub use document::{
    DocumentInfo, document_page_count, inspect_document, page_info_and_text, page_info_for_page,
};
pub use page_info::{LlmReason, PageInfo};
