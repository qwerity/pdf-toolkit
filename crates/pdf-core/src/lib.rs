//! Primitives shared by every crate in the workspace.
//!
//! What belongs here is mechanism with no policy attached: binding to the
//! native library, addressing a page, naming an output file, judging whether a
//! text layer is readable, and the error and warning vocabularies everything
//! reports through. Decisions built on top of these — which pages carry a
//! drawing, when a vision pass is worth its cost, how a page should be cut —
//! live in the crates that make them.
//!
//! The rule that keeps this crate honest: nothing here may depend on another
//! crate in the workspace.

pub mod error;
pub mod naming;
pub mod page;
pub mod page_selection;
pub mod pdfium_runtime;
pub mod text_quality;
pub mod utils;
pub mod warning;

/// The native runtime handle, re-exported so callers need not depend on
/// `pdfium-render` directly just to name the type every entry point takes.
pub use pdfium_render::prelude::Pdfium;

pub use error::{PdfError, Result};
pub use naming::{
    OutputReservation, file_size_kb, output_stem, reserve_named_output_file_with_extension,
    reserve_output_file_with_extension, resolve_output_dir,
};
pub use page::{
    destination_page_index, open_document, page_count_u16, page_dimensions_px, page_index,
};
pub use page_selection::PageSelection;
pub use pdfium_runtime::{create_pdfium, create_pdfium_at};
pub use text_quality::{is_probably_unreadable_text, sanitize_extracted_text};
pub use warning::ExtractionWarning;
