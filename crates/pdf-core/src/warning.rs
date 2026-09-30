//! Per-page problems that do not abort a run.
//!
//! A page that cannot be read is reported and skipped rather than failing the
//! whole document, so every extractor accumulates these and returns them
//! beside its results. The variants live here because three crates raise them
//! and the CLI renders them from one place.

use std::fmt;

#[derive(Debug, Eq, PartialEq)]
pub enum ExtractionWarning {
    PageOutOfBounds {
        page_number: u16,
        total_pages: u16,
    },
    LoadPage {
        page_number: u16,
        reason: String,
    },
    UnreadableText {
        page_number: u16,
    },
    SavePage {
        page_number: u16,
        reason: String,
    },
    /// OCR was asked for and failed; the page's text layer was used, or the
    /// page skipped when it had none worth reading.
    OcrFailed {
        page_number: u16,
        reason: String,
    },
    /// `inspect` could not load the page. Unlike [`Self::LoadPage`] the page
    /// is not skipped: it is still listed, flagged `page_unloadable`.
    PageUnloadable {
        page_number: u16,
        reason: String,
    },
}

impl ExtractionWarning {
    /// The page a warning is about. Every variant has one, which is what lets
    /// the CLI key its `errors` object by page number.
    pub fn page_number(&self) -> u16 {
        match self {
            Self::PageOutOfBounds { page_number, .. }
            | Self::LoadPage { page_number, .. }
            | Self::UnreadableText { page_number }
            | Self::SavePage { page_number, .. }
            | Self::OcrFailed { page_number, .. }
            | Self::PageUnloadable { page_number, .. } => *page_number,
        }
    }
}

impl fmt::Display for ExtractionWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PageOutOfBounds {
                page_number,
                total_pages,
            } => write!(
                f,
                "page {page_number} is out of bounds (1..={total_pages}). Skipping."
            ),
            Self::LoadPage {
                page_number,
                reason,
            } => write!(f, "could not get page {page_number}: {reason}. Skipping."),
            Self::UnreadableText { page_number } => {
                write!(
                    f,
                    "extracted text for page {page_number} appears corrupted and was skipped."
                )
            }
            Self::SavePage {
                page_number,
                reason,
            } => write!(f, "could not save page {page_number}: {reason}. Skipping."),
            Self::OcrFailed {
                page_number,
                reason,
            } => write!(f, "OCR failed on page {page_number}: {reason}."),
            // pdfium's reason is a multi-line debug dump; one line reads.
            Self::PageUnloadable {
                page_number,
                reason,
            } => write!(
                f,
                "could not load page {page_number}: {}; reported as page_unloadable.",
                reason.split_whitespace().collect::<Vec<_>>().join(" ")
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variant_reports_the_page_it_is_about() {
        let cases = [
            ExtractionWarning::PageOutOfBounds {
                page_number: 7,
                total_pages: 3,
            },
            ExtractionWarning::LoadPage {
                page_number: 7,
                reason: "broken".into(),
            },
            ExtractionWarning::UnreadableText { page_number: 7 },
            ExtractionWarning::SavePage {
                page_number: 7,
                reason: "disk full".into(),
            },
            ExtractionWarning::OcrFailed {
                page_number: 7,
                reason: "model missing".into(),
            },
            ExtractionWarning::PageUnloadable {
                page_number: 7,
                reason: "broken".into(),
            },
        ];

        for warning in cases {
            assert_eq!(warning.page_number(), 7);
            assert!(
                warning.to_string().contains('7'),
                "the message must name the page too: {warning}"
            );
        }
    }

    /// The wording reaches a caller verbatim through the CLI's `errors`
    /// object, so it is contract rather than debug output.
    #[test]
    fn messages_say_what_happened_and_that_the_page_was_skipped() {
        assert_eq!(
            ExtractionWarning::PageOutOfBounds {
                page_number: 9,
                total_pages: 4
            }
            .to_string(),
            "page 9 is out of bounds (1..=4). Skipping."
        );
        assert_eq!(
            ExtractionWarning::UnreadableText { page_number: 2 }.to_string(),
            "extracted text for page 2 appears corrupted and was skipped."
        );
        assert_eq!(
            ExtractionWarning::PageUnloadable {
                page_number: 3,
                reason: "PdfiumLibraryInternalError(\n    Unknown,\n)".into()
            }
            .to_string(),
            "could not load page 3: PdfiumLibraryInternalError( Unknown, ); reported as page_unloadable."
        );
    }
}
