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
    /// assert!(matches!(PageSelection::parse("1,3,5-7"), Ok(PageSelection::Specific(_))));
    /// ```
    pub fn parse(input: &str) -> Result<Self> {
        if input.trim().eq_ignore_ascii_case("all") {
            return Ok(Self::All);
        }

        let mut page_numbers = Vec::new();

        for (index, token) in input.split(',').enumerate() {
            extend_page_selection(token.trim(), index + 1, &mut page_numbers)?;
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

    page_numbers.push(parse_page_number(value, position)?);

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
        return Err(PdfCutterError::InvalidPageSelectionRange {
            position,
            value: value.to_string(),
        });
    }

    page_numbers.reserve(usize::from(end_page.get() - start_page.get()) + 1);

    for raw_page in start_page.get()..=end_page.get() {
        page_numbers.push(NonZeroU16::new(raw_page).expect("page ranges must stay non-zero"));
    }

    Ok(())
}

fn parse_page_number(value: &str, position: usize) -> Result<NonZeroU16> {
    let raw_page = value
        .parse::<u16>()
        .map_err(|_| invalid_page_token(position, value))?;

    NonZeroU16::new(raw_page).ok_or_else(|| invalid_page_token(position, value))
}

fn invalid_page_token(position: usize, value: &str) -> PdfCutterError {
    PdfCutterError::InvalidPageSelectionToken {
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
    fn test_parse_page_ranges() {
        assert_eq!(
            PageSelection::parse("1-3, 5-6, 9").expect("parse should succeed"),
            PageSelection::Specific(vec![nz(1), nz(2), nz(3), nz(5), nz(6), nz(9)])
        );
    }

    #[test]
    fn test_parse_rejects_zero() {
        let error = PageSelection::parse("0").expect_err("page 0 must be rejected");

        assert!(error.to_string().contains("positive integer"));
    }

    #[test]
    fn test_parse_rejects_descending_ranges() {
        let error = PageSelection::parse("3-1").expect_err("descending ranges must be rejected");

        assert!(error.to_string().contains("ascending"));
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
