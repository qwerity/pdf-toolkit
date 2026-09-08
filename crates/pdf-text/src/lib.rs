//! Reading what a page says.
//!
//! Two extractions over the same text layer, for two different questions.
//! [`extract_text`] answers "what words are on this page", and writes them to
//! files. [`extract_structured_text`] answers "how is this page laid out" —
//! positioned spans carrying font size and style, plus a markdown
//! reconstruction in visual reading order that resolves columns, rotated
//! pages and tables whose cells wrap.
//!
//! Both refuse a page whose text layer is statistically garbled rather than
//! emitting nonsense: a broken font mapping produces confident-looking
//! rubbish, and a caller cannot tell that from real text.

pub mod structured_text_extractor;
pub mod text_extractor;

pub use structured_text_extractor::{
    StructuredExtractionReport, StructuredOutputFormat, StructuredPage, TextSpan,
    extract_structured_text, page_has_complex_tables,
};
pub use text_extractor::{TextExtractionReport, TextFileResult, extract_text};
