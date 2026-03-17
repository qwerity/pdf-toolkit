use crate::document::open_document;
use crate::error::{PdfCutterError, Result};
use crate::extractor::ExtractionWarning;
use crate::output::{
    OutputReservation, file_size_kb, output_stem, reserve_named_output_file_with_extension,
    reserve_output_file_with_extension, resolve_output_dir,
};
use crate::page_selection::PageSelection;
use pdfium_render::prelude::{PdfPage, PdfPages, Pdfium, PdfiumError};
use std::io::Write;
use std::num::NonZeroU16;
use std::path::{Path, PathBuf};

const TEXT_FILE_EXTENSION: &str = "txt";

/// The result of extracting text into one or more `.txt` files.
pub struct TextExtractionReport {
    pub files: Vec<TextFileResult>,
    pub warnings: Vec<ExtractionWarning>,
}

/// Metadata about a generated text file.
pub struct TextFileResult {
    pub output_path: PathBuf,
    pub page_count: usize,
    pub size_kb: u64,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum TextOutputMode {
    SingleFile,
    PerPage,
}

/// Extracts text from selected pages into one or more `.txt` files.
///
/// Parameters:
/// - `pdfium`: initialized PDFium runtime used to open the source document.
/// - `pdf_path`: path to the input PDF file.
/// - `selection`: page selection to extract.
/// - `concat`: when `true`, selected pages are written to a single text file.
/// - `output_folder`: optional output directory; defaults to the current directory.
///
/// Returns:
/// - `Ok(TextExtractionReport)`: generated text files and any non-fatal page warnings.
///
/// # Examples
///
/// ```no_run
/// use pdf_cutter::{PageSelection, create_pdfium, extract_text};
/// use std::path::Path;
///
/// let pdfium = create_pdfium()?;
/// let report = extract_text(
///     &pdfium,
///     Path::new("input.pdf"),
///     PageSelection::All,
///     false,
///     Some(Path::new("out")),
/// )?;
/// println!("{}", report.files.len());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn extract_text(
    pdfium: &Pdfium,
    pdf_path: &Path,
    selection: PageSelection,
    concat: bool,
    output_folder: Option<&Path>,
) -> Result<TextExtractionReport> {
    let document = open_document(pdfium, pdf_path)?;
    let pages = document.pages();
    let total_pages = pages.len();

    if total_pages == 0 {
        return Err(PdfCutterError::EmptyDocument);
    }

    let output_mode = text_output_mode(&selection, concat);
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

    Ok(TextExtractionReport { files, warnings })
}

fn text_output_mode(selection: &PageSelection, concat: bool) -> TextOutputMode {
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
        let page_text = match load_page_text(pages, page_number) {
            Ok(page_text) => page_text,
            Err(warning) => {
                warnings.push(warning);
                continue;
            }
        };

        if extracted_pages > 0 {
            write_text(&mut reserved_output, "\n\n")?;
        }

        write_text(&mut reserved_output, &page_text)?;
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
        let page_text = match load_page_text(pages, page_number) {
            Ok(page_text) => page_text,
            Err(warning) => {
                warnings.push(warning);
                continue;
            }
        };

        let mut reserved_output =
            reserve_output_file_with_extension(output_dir, stem, page_number, TEXT_FILE_EXTENSION)?;
        write_text(&mut reserved_output, &page_text)?;

        if let Some(result) = finalize_text_output(reserved_output, 1)? {
            results.push(result);
        }
    }

    Ok(results)
}

fn load_page_text(
    pages: &PdfPages<'_>,
    page_number: NonZeroU16,
) -> std::result::Result<String, ExtractionWarning> {
    let source_page_index = page_number.get() - 1;
    let page = pages
        .get(source_page_index)
        .map_err(|error| ExtractionWarning::LoadPage {
            page_number: page_number.get(),
            reason: error.to_string(),
        })?;

    extract_page_text(&page).map_err(|error| ExtractionWarning::LoadPage {
        page_number: page_number.get(),
        reason: error.to_string(),
    })
}

fn extract_page_text(page: &PdfPage<'_>) -> std::result::Result<String, PdfiumError> {
    page.text().map(|page_text| page_text.all())
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
        output_path: path,
        page_count,
        size_kb,
    }))
}

#[cfg(test)]
mod tests {
    use super::{TextOutputMode, text_output_mode};
    use crate::PageSelection;
    use std::num::NonZeroU16;

    #[test]
    fn uses_single_file_for_all_pages() {
        assert_eq!(
            text_output_mode(&PageSelection::All, false),
            TextOutputMode::SingleFile
        );
    }

    #[test]
    fn uses_single_file_when_concat_is_requested() {
        assert_eq!(
            text_output_mode(
                &PageSelection::Specific(vec![
                    NonZeroU16::new(1).expect("non-zero"),
                    NonZeroU16::new(3).expect("non-zero"),
                ]),
                true,
            ),
            TextOutputMode::SingleFile
        );
    }

    #[test]
    fn uses_per_page_files_for_specific_pages_without_concat() {
        assert_eq!(
            text_output_mode(
                &PageSelection::Specific(vec![NonZeroU16::new(2).expect("non-zero")]),
                false,
            ),
            TextOutputMode::PerPage
        );
    }
}
