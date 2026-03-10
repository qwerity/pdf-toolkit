use crate::error::{PdfCutterError, Result};
use std::num::NonZeroU16;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PageSelection {
    All,
    Specific(Vec<NonZeroU16>),
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ResolvedPageSelection {
    pub(crate) page_numbers: Vec<NonZeroU16>,
    pub(crate) out_of_bounds: Vec<NonZeroU16>,
}

impl PageSelection {
    /// Parses the `--pages` CLI argument into a validated page selection.
    ///
    /// # Examples
    ///
    /// ```
    /// use pdf_cutter::PageSelection;
    ///
    /// assert!(matches!(PageSelection::parse("all"), Ok(PageSelection::All)));
    /// assert!(matches!(PageSelection::parse("1,3,5"), Ok(PageSelection::Specific(_))));
    /// ```
    pub fn parse(input: &str) -> Result<Self> {
        if input.trim().eq_ignore_ascii_case("all") {
            return Ok(Self::All);
        }

        let mut page_numbers = Vec::new();

        for (index, token) in input.split(',').enumerate() {
            let value = token.trim();

            if value.is_empty() {
                return Err(PdfCutterError::InvalidPageSelectionToken {
                    position: index + 1,
                    value: value.to_string(),
                });
            }

            let raw_page =
                value
                    .parse::<u16>()
                    .map_err(|_| PdfCutterError::InvalidPageSelectionToken {
                        position: index + 1,
                        value: value.to_string(),
                    })?;

            let Some(page_number) = NonZeroU16::new(raw_page) else {
                return Err(PdfCutterError::InvalidPageSelectionToken {
                    position: index + 1,
                    value: value.to_string(),
                });
            };

            page_numbers.push(page_number);
        }

        if page_numbers.is_empty() {
            return Err(PdfCutterError::EmptyPageSelection);
        }

        Ok(Self::Specific(page_numbers))
    }

    pub(crate) fn resolve(&self, total_pages: u16) -> ResolvedPageSelection {
        match self {
            Self::All => ResolvedPageSelection {
                page_numbers: (1..=total_pages).filter_map(NonZeroU16::new).collect(),
                out_of_bounds: Vec::new(),
            },
            Self::Specific(page_numbers) => resolve_specific_selection(page_numbers, total_pages),
        }
    }
}

fn resolve_specific_selection(
    page_numbers: &[NonZeroU16],
    total_pages: u16,
) -> ResolvedPageSelection {
    let mut seen = vec![false; usize::from(total_pages) + 1];
    let mut resolved = Vec::with_capacity(page_numbers.len());
    let mut out_of_bounds = Vec::new();

    for &page_number in page_numbers {
        let raw_page = page_number.get();

        if raw_page > total_pages {
            out_of_bounds.push(page_number);
            continue;
        }

        let seen_slot = &mut seen[usize::from(raw_page)];

        if !*seen_slot {
            *seen_slot = true;
            resolved.push(page_number);
        }
    }

    ResolvedPageSelection {
        page_numbers: resolved,
        out_of_bounds,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nz(value: u16) -> NonZeroU16 {
        NonZeroU16::new(value).expect("test page numbers must be non-zero")
    }

    #[test]
    fn test_parse_all_selection() {
        assert_eq!(
            PageSelection::parse("all").expect("parse should succeed"),
            PageSelection::All
        );
    }

    #[test]
    fn test_parse_specific_selection() {
        assert_eq!(
            PageSelection::parse("1, 3, 5").expect("parse should succeed"),
            PageSelection::Specific(vec![nz(1), nz(3), nz(5)])
        );
    }

    #[test]
    fn test_parse_rejects_zero() {
        let error = PageSelection::parse("0").expect_err("page 0 must be rejected");

        assert!(error.to_string().contains("positive integer"));
    }

    #[test]
    fn test_resolve_deduplicates_and_tracks_out_of_bounds_pages() {
        let selection = PageSelection::Specific(vec![nz(1), nz(3), nz(3), nz(9)]);

        assert_eq!(
            selection.resolve(5),
            ResolvedPageSelection {
                page_numbers: vec![nz(1), nz(3)],
                out_of_bounds: vec![nz(9)],
            }
        );
    }
}
