//! Per-page metadata, and the reasons a page needs more than its text layer.

/// Metadata about a source PDF page.
#[derive(Clone, Debug, PartialEq)]
pub struct PageInfo {
    pub page_number: u16,
    pub width_px: u32,
    pub height_px: u32,
    pub contains_images: bool,
    /// Total number of image objects on the page (including inside Form XObjects).
    pub image_count: u32,
    /// Estimated fraction of page area covered by top-level image objects (0.0–1.0).
    pub image_coverage: f32,
    /// Reasons a vision-capable LLM/OCR pass is likely to recover content
    /// that text extraction cannot. Empty when text extraction suffices.
    pub llm_reasons: Vec<LlmReason>,
}

impl PageInfo {
    /// True when a vision-capable LLM/OCR pass is recommended for this page.
    pub fn llm_recommended(&self) -> bool {
        !self.llm_reasons.is_empty()
    }
}

/// Why a page benefits from a vision-capable LLM/OCR pass on top of (or
/// instead of) text-layer extraction.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LlmReason {
    /// The page has images but (almost) no extractable text — likely a scan.
    NoTextLayer,
    /// A significant share of the page area is covered by embedded images,
    /// whose contents (diagram labels, figure text) are invisible to the
    /// text layer.
    ImageHeavy,
    /// The text layer exists but is statistically garbled (e.g. a broken
    /// CID font mapping); rendered pixels are the only reliable source.
    UnreadableText,
    /// The page contains a table with wrapped or merged cells across several
    /// columns; extracted cell-to-column association is approximate and a
    /// vision pass can verify or reconstruct the exact structure.
    ComplexTable,
}

impl LlmReason {
    /// Stable machine-readable identifier used in JSON output.
    pub fn slug(self) -> &'static str {
        match self {
            Self::NoTextLayer => "no_text_layer",
            Self::ImageHeavy => "image_heavy",
            Self::UnreadableText => "unreadable_text",
            Self::ComplexTable => "complex_table",
        }
    }

    /// Human-readable explanation.
    pub fn description(self) -> &'static str {
        match self {
            Self::NoTextLayer => "page has images but no usable text layer; likely a scanned page",
            Self::ImageHeavy => {
                "large image coverage; text inside embedded images is not extractable"
            }
            Self::UnreadableText => {
                "text layer appears garbled; rendered pixels are the reliable source"
            }
            Self::ComplexTable => {
                "table with wrapped or merged cells; extracted cell-to-column \
                 association is approximate"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_with_no_reasons_needs_no_vision_pass() {
        let page = PageInfo {
            page_number: 1,
            width_px: 595,
            height_px: 842,
            contains_images: false,
            image_count: 0,
            image_coverage: 0.0,
            llm_reasons: Vec::new(),
        };

        assert!(!page.llm_recommended());
    }

    /// The slugs are what a caller branches on, so they are contract and must
    /// not drift with the variant names.
    #[test]
    fn every_reason_has_a_stable_slug_and_an_explanation() {
        let cases = [
            (LlmReason::NoTextLayer, "no_text_layer"),
            (LlmReason::ImageHeavy, "image_heavy"),
            (LlmReason::UnreadableText, "unreadable_text"),
            (LlmReason::ComplexTable, "complex_table"),
        ];

        for (reason, slug) in cases {
            assert_eq!(reason.slug(), slug);
            assert!(
                !reason.description().is_empty(),
                "{slug} must explain itself"
            );
        }
    }
}
