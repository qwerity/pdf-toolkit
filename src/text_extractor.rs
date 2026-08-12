use crate::document::{open_document, page_count_u16, page_index};
use crate::error::{PdfCutterError, Result};
use crate::extractor::ExtractionWarning;
use crate::output::{
    OutputReservation, file_size_kb, output_stem, reserve_named_output_file_with_extension,
    reserve_output_file_with_extension, resolve_output_dir,
};
use crate::page_selection::PageSelection;
use crate::text_quality::is_probably_unreadable_text;
use pdfium_render::prelude::{PdfPage, PdfPages, Pdfium, PdfiumError};
use std::io::Write;
use std::num::NonZeroU16;
use std::path::{Path, PathBuf};

const TEXT_FILE_EXTENSION: &str = "txt";

/// Text extraction result.
pub struct TextExtractionReport {
    pub files: Vec<TextFileResult>,
    pub selected_page_count: usize,
    pub warnings: Vec<ExtractionWarning>,
}

/// Metadata for a generated text file.
pub struct TextFileResult {
    pub page_number: Option<u16>,
    pub output_path: PathBuf,
    pub page_count: usize,
    pub size_kb: u64,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum TextOutputMode {
    SingleFile,
    PerPage,
}

/// Extracts selected pages into `.txt` file(s).
pub fn extract_text(
    pdfium: &Pdfium,
    pdf_path: &Path,
    selection: PageSelection,
    concat: bool,
    output_folder: Option<&Path>,
) -> Result<TextExtractionReport> {
    let document = open_document(pdfium, pdf_path)?;
    let pages = document.pages();
    let total_pages = page_count_u16(pages)?;

    if total_pages == 0 {
        return Err(PdfCutterError::EmptyDocument);
    }

    let output_mode = output_mode(&selection, concat);
    let resolved_selection = selection.resolve(total_pages);
    let output_dir = resolve_output_dir(output_folder)?;
    let stem = output_stem(pdf_path);
    let mut warnings = resolved_selection
        .out_of_bounds
        .into_iter()
        .map(|page_number| ExtractionWarning::PageOutOfBounds {
            page_number: page_number.get(),
            total_pages,
        })
        .collect::<Vec<_>>();
    let page_numbers = resolved_selection.page_numbers;
    let selected_page_count = page_numbers.len();

    let files = match output_mode {
        TextOutputMode::SingleFile => {
            let base_name = if concat {
                format!("{stem}_concat")
            } else {
                format!("{stem}_text")
            };

            save_text_to_single_file(pages, &page_numbers, &output_dir, &base_name, &mut warnings)?
                .into_iter()
                .collect()
        }
        TextOutputMode::PerPage => {
            save_text_per_page(pages, &page_numbers, &output_dir, &stem, &mut warnings)?
        }
    };

    Ok(TextExtractionReport {
        files,
        selected_page_count,
        warnings,
    })
}

fn output_mode(selection: &PageSelection, concat: bool) -> TextOutputMode {
    if concat || matches!(selection, PageSelection::All) {
        TextOutputMode::SingleFile
    } else {
        TextOutputMode::PerPage
    }
}

fn save_text_to_single_file(
    pages: &PdfPages<'_>,
    page_numbers: &[NonZeroU16],
    output_dir: &Path,
    base_name: &str,
    warnings: &mut Vec<ExtractionWarning>,
) -> Result<Option<TextFileResult>> {
    if page_numbers.is_empty() {
        return Ok(None);
    }

    let mut reserved_output =
        reserve_named_output_file_with_extension(output_dir, base_name, TEXT_FILE_EXTENSION)?;
    let mut extracted_pages = 0usize;

    for &page_number in page_numbers {
        let page_text = match read_page_text(pages, page_number) {
            Ok(page_text) => page_text,
            Err(warning) => {
                warnings.push(warning);
                continue;
            }
        };

        write_page_block(
            &mut reserved_output,
            page_number,
            &page_text,
            extracted_pages > 0,
        )?;
        extracted_pages += 1;
    }

    finalize_text_output(reserved_output, extracted_pages)
}

fn save_text_per_page(
    pages: &PdfPages<'_>,
    page_numbers: &[NonZeroU16],
    output_dir: &Path,
    stem: &str,
    warnings: &mut Vec<ExtractionWarning>,
) -> Result<Vec<TextFileResult>> {
    let mut results = Vec::with_capacity(page_numbers.len());

    for &page_number in page_numbers {
        let page_text = match read_page_text(pages, page_number) {
            Ok(page_text) => page_text,
            Err(warning) => {
                warnings.push(warning);
                continue;
            }
        };

        let mut reserved_output =
            reserve_output_file_with_extension(output_dir, stem, page_number, TEXT_FILE_EXTENSION)?;
        write_page_block(&mut reserved_output, page_number, &page_text, false)?;

        if let Some(result) = finalize_text_output(reserved_output, 1)? {
            results.push(TextFileResult {
                page_number: Some(page_number.get()),
                ..result
            });
        }
    }

    Ok(results)
}

fn read_page_text(
    pages: &PdfPages<'_>,
    page_number: NonZeroU16,
) -> std::result::Result<String, ExtractionWarning> {
    let page = pages
        .get(page_index(page_number))
        .map_err(|error| ExtractionWarning::LoadPage {
            page_number: page_number.get(),
            reason: error.to_string(),
        })?;

    let text = page_text(&page).map_err(|error| ExtractionWarning::LoadPage {
        page_number: page_number.get(),
        reason: error.to_string(),
    })?;

    if is_probably_unreadable_text(&text) {
        Err(ExtractionWarning::UnreadableText {
            page_number: page_number.get(),
        })
    } else {
        Ok(text)
    }
}

fn page_text(page: &PdfPage<'_>) -> std::result::Result<String, PdfiumError> {
    page.text()
        .map(|page_text| sanitize_extracted_text(&page_text.all()))
}

fn write_page_block(
    reserved_output: &mut OutputReservation,
    page_number: NonZeroU16,
    page_text: &str,
    separated: bool,
) -> Result<()> {
    if separated {
        write_text(reserved_output, "\n\n")?;
    }

    write_text(reserved_output, &page_marker(page_number))?;
    write_text(reserved_output, "\n\n")?;
    write_text(reserved_output, page_text)
}

fn page_marker(page_number: NonZeroU16) -> String {
    format!("____Page{}____", page_number.get())
}

fn sanitize_extracted_text(text: &str) -> String {
    let filtered = text
        .chars()
        .filter(|ch| is_allowed_text_char(*ch))
        .collect::<String>();

    filtered.trim_end().to_string()
}

fn is_allowed_text_char(ch: char) -> bool {
    !ch.is_control() || matches!(ch, '\n' | '\r' | '\t')
}

fn write_text(reserved_output: &mut OutputReservation, text: &str) -> Result<()> {
    reserved_output
        .file
        .write_all(text.as_bytes())
        .map_err(|source| PdfCutterError::WriteText {
            path: reserved_output.path.clone(),
            source,
        })
}

fn finalize_text_output(
    reserved_output: OutputReservation,
    page_count: usize,
) -> Result<Option<TextFileResult>> {
    let OutputReservation { path, file } = reserved_output;

    if page_count == 0 {
        drop(file);
        let _ = std::fs::remove_file(&path);
        return Ok(None);
    }

    let size_kb = file_size_kb(&file, &path)?;

    Ok(Some(TextFileResult {
        page_number: None,
        output_path: path,
        page_count,
        size_kb,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_removes_disallowed_control_characters() {
        assert_eq!(sanitize_extracted_text("a\u{0}b\u{7}c"), "abc");
    }

    #[test]
    fn sanitize_keeps_newlines_tabs_and_carriage_returns() {
        assert_eq!(sanitize_extracted_text("a\tb\nc\rd"), "a\tb\nc\rd");
    }

    #[test]
    fn sanitize_trims_trailing_whitespace_only() {
        assert_eq!(sanitize_extracted_text("  a b \n\t "), "  a b");
    }

    #[test]
    fn page_marker_format_is_stable() {
        assert_eq!(page_marker(NonZeroU16::new(7).unwrap()), "____Page7____");
    }

    #[test]
    fn output_mode_matrix() {
        let specific = PageSelection::Specific(vec![NonZeroU16::new(1).unwrap()]);
        assert_eq!(
            output_mode(&PageSelection::All, false),
            TextOutputMode::SingleFile
        );
        assert_eq!(
            output_mode(&PageSelection::All, true),
            TextOutputMode::SingleFile
        );
        assert_eq!(output_mode(&specific, false), TextOutputMode::PerPage);
        assert_eq!(output_mode(&specific, true), TextOutputMode::SingleFile);
    }
}
