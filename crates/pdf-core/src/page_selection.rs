use crate::error::{PdfError, Result};
use std::num::NonZeroU16;

/// Bounds eager expansion, including duplicates, before allocating.
pub const MAX_SELECTION_ENTRIES: usize = u16::MAX as usize;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PageSelection {
    All,
    Specific(Vec<NonZeroU16>),
}

#[derive(Debug, Eq, PartialEq)]
pub struct ResolvedPageSelection {
    pub page_numbers: Vec<NonZeroU16>,
    pub out_of_bounds: Vec<NonZeroU16>,
}

impl PageSelection {
    /// Parses the `--pages` CLI argument into a validated page selection.
    pub fn parse(input: &str) -> Result<Self> {
        if input.trim().eq_ignore_ascii_case("all") {
            return Ok(Self::All);
        }

        let mut page_numbers = Vec::new();

        for (index, token) in input.split(',').enumerate() {
            extend_page_selection(token.trim(), index + 1, &mut page_numbers)?;
        }

        if page_numbers.is_empty() {
            return Err(PdfError::EmptyPageSelection);
        }

        Ok(Self::Specific(page_numbers))
    }

    pub fn resolve(&self, total_pages: u16) -> ResolvedPageSelection {
        match self {
            Self::All => ResolvedPageSelection {
                page_numbers: (1..=total_pages).filter_map(NonZeroU16::new).collect(),
                out_of_bounds: Vec::new(),
            },
            Self::Specific(page_numbers) => resolve_specific_selection(page_numbers, total_pages),
        }
    }
}

fn extend_page_selection(
    value: &str,
    position: usize,
    page_numbers: &mut Vec<NonZeroU16>,
) -> Result<()> {
    if value.is_empty() {
        return Err(invalid_page_token(position, value));
    }

    if let Some((start, end)) = value.split_once('-') {
        return extend_page_range(value, start.trim(), end.trim(), position, page_numbers);
    }

    let page = parse_page_number(value, position)?;
    reserve_selection(page_numbers, 1)?;
    page_numbers.push(page);

    Ok(())
}

fn extend_page_range(
    value: &str,
    start: &str,
    end: &str,
    position: usize,
    page_numbers: &mut Vec<NonZeroU16>,
) -> Result<()> {
    let start_page = parse_page_number(start, position)?;
    let end_page = parse_page_number(end, position)?;

    if start_page > end_page {
        return Err(PdfError::InvalidPageSelectionRange {
            position,
            value: value.to_string(),
        });
    }

    reserve_selection(
        page_numbers,
        usize::from(end_page.get() - start_page.get()) + 1,
    )?;

    for raw_page in start_page.get()..=end_page.get() {
        page_numbers.push(NonZeroU16::new(raw_page).expect("page ranges must stay non-zero"));
    }

    Ok(())
}

fn reserve_selection(pages: &mut Vec<NonZeroU16>, additional: usize) -> Result<()> {
    if additional > MAX_SELECTION_ENTRIES.saturating_sub(pages.len()) {
        return Err(PdfError::PageSelectionTooLarge {
            max: MAX_SELECTION_ENTRIES,
        });
    }
    pages.reserve(additional);
    Ok(())
}

fn parse_page_number(value: &str, position: usize) -> Result<NonZeroU16> {
    let raw_page = value
        .parse::<u16>()
        .map_err(|_| invalid_page_token(position, value))?;

    NonZeroU16::new(raw_page).ok_or_else(|| invalid_page_token(position, value))
}

fn invalid_page_token(position: usize, value: &str) -> PdfError {
    PdfError::InvalidPageSelectionToken {
        position,
        value: value.to_string(),
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

    fn page(n: u16) -> NonZeroU16 {
        NonZeroU16::new(n).expect("test page numbers must be non-zero")
    }

    fn pages(numbers: &[u16]) -> Vec<NonZeroU16> {
        numbers.iter().copied().map(page).collect()
    }

    #[test]
    fn expansion_is_bounded_for_ranges_and_single_pages() {
        assert!(PageSelection::parse("1-65535").is_ok());
        for spec in ["1-65535,1", "1-65535,1-65535"] {
            assert!(matches!(
                PageSelection::parse(spec),
                Err(PdfError::PageSelectionTooLarge { .. })
            ));
        }
        let repeated = vec!["1-65535"; 256].join(",");
        assert!(matches!(
            PageSelection::parse(&repeated),
            Err(PdfError::PageSelectionTooLarge { .. })
        ));
    }

    #[test]
    fn parse_all_is_case_insensitive_and_trimmed() {
        assert_eq!(PageSelection::parse("all").unwrap(), PageSelection::All);
        assert_eq!(PageSelection::parse("ALL").unwrap(), PageSelection::All);
        assert_eq!(PageSelection::parse(" All ").unwrap(), PageSelection::All);
    }

    #[test]
    fn parse_single_page() {
        assert_eq!(
            PageSelection::parse("5").unwrap(),
            PageSelection::Specific(pages(&[5]))
        );
    }

    #[test]
    fn parse_list_and_ranges_with_whitespace() {
        assert_eq!(
            PageSelection::parse("1, 3 ,5-7").unwrap(),
            PageSelection::Specific(pages(&[1, 3, 5, 6, 7]))
        );
    }

    #[test]
    fn parse_single_element_range() {
        assert_eq!(
            PageSelection::parse("4-4").unwrap(),
            PageSelection::Specific(pages(&[4]))
        );
    }

    #[test]
    fn parse_preserves_duplicates_and_order() {
        assert_eq!(
            PageSelection::parse("3,1,3").unwrap(),
            PageSelection::Specific(pages(&[3, 1, 3]))
        );
    }

    #[test]
    fn parse_rejects_empty_and_blank_tokens() {
        assert!(matches!(
            PageSelection::parse(""),
            Err(PdfError::InvalidPageSelectionToken { position: 1, .. })
        ));
        assert!(matches!(
            PageSelection::parse("1,,2"),
            Err(PdfError::InvalidPageSelectionToken { position: 2, .. })
        ));
    }

    #[test]
    fn parse_rejects_zero_and_non_numeric_tokens() {
        for input in ["0", "abc", "1.5", "1,x"] {
            assert!(
                matches!(
                    PageSelection::parse(input),
                    Err(PdfError::InvalidPageSelectionToken { .. })
                ),
                "input {input:?} must be rejected"
            );
        }
    }

    #[test]
    fn parse_rejects_pages_beyond_u16() {
        assert!(matches!(
            PageSelection::parse("70000"),
            Err(PdfError::InvalidPageSelectionToken { .. })
        ));
    }

    #[test]
    fn parse_rejects_descending_range() {
        assert!(matches!(
            PageSelection::parse("5-3"),
            Err(PdfError::InvalidPageSelectionRange { position: 1, .. })
        ));
    }

    #[test]
    fn parse_rejects_open_ended_ranges() {
        for input in ["1-", "-3", "-"] {
            assert!(
                matches!(
                    PageSelection::parse(input),
                    Err(PdfError::InvalidPageSelectionToken { .. })
                ),
                "input {input:?} must be rejected"
            );
        }
    }

    #[test]
    fn resolve_all_lists_every_page() {
        let resolved = PageSelection::All.resolve(3);
        assert_eq!(resolved.page_numbers, pages(&[1, 2, 3]));
        assert!(resolved.out_of_bounds.is_empty());
    }

    #[test]
    fn resolve_all_on_zero_pages_is_empty() {
        let resolved = PageSelection::All.resolve(0);
        assert!(resolved.page_numbers.is_empty());
        assert!(resolved.out_of_bounds.is_empty());
    }

    #[test]
    fn resolve_specific_dedups_and_flags_out_of_bounds() {
        let selection = PageSelection::Specific(pages(&[2, 2, 9, 1]));
        let resolved = selection.resolve(3);
        assert_eq!(resolved.page_numbers, pages(&[2, 1]));
        assert_eq!(resolved.out_of_bounds, pages(&[9]));
    }

    #[test]
    fn resolve_specific_keeps_duplicate_out_of_bounds() {
        let selection = PageSelection::Specific(pages(&[9, 9]));
        let resolved = selection.resolve(3);
        assert!(resolved.page_numbers.is_empty());
        assert_eq!(resolved.out_of_bounds, pages(&[9, 9]));
    }

    #[test]
    fn resolve_specific_accepts_boundary_page() {
        let resolved = PageSelection::Specific(pages(&[3])).resolve(3);
        assert_eq!(resolved.page_numbers, pages(&[3]));
        assert!(resolved.out_of_bounds.is_empty());
    }
}
