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
    let total_pages = pages.len();

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
    let source_page_index = page_number.get() - 1;
    let page = pages
        .get(source_page_index)
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

fn is_probably_unreadable_text(text: &str) -> bool {
    let metrics = analyze_text_quality(text);

    if metrics.visible_char_count() < 80 || metrics.alpha_word_count < 20 {
        return false;
    }

    metrics.noise_ratio() > 0.35
        && metrics.lowercase_ratio() < 0.30
        && metrics.vowel_word_ratio() < 0.45
}

fn analyze_text_quality(text: &str) -> TextQualityMetrics {
    let mut metrics = TextQualityMetrics::default();

    for ch in text.chars() {
        if ch.is_alphabetic() {
            metrics.letter_count += 1;

            if ch.is_lowercase() {
                metrics.lowercase_count += 1;
            }
        } else if ch.is_numeric() {
            metrics.digit_count += 1;
        } else if ch.is_ascii_punctuation() {
            metrics.punctuation_count += 1;
        }
    }

    metrics.alpha_word_count = text
        .split_whitespace()
        .filter(|word| word.chars().any(char::is_alphabetic))
        .count();
    metrics.vowel_word_count = text
        .split_whitespace()
        .filter(|word| word.chars().any(char::is_alphabetic))
        .filter(|word| word.chars().any(is_latin_vowel))
        .count();

    metrics
}

fn is_latin_vowel(ch: char) -> bool {
    matches!(
        ch,
        'a' | 'e' | 'i' | 'o' | 'u' | 'A' | 'E' | 'I' | 'O' | 'U'
    )
}

#[derive(Default)]
struct TextQualityMetrics {
    letter_count: usize,
    lowercase_count: usize,
    digit_count: usize,
    punctuation_count: usize,
    alpha_word_count: usize,
    vowel_word_count: usize,
}

impl TextQualityMetrics {
    fn visible_char_count(&self) -> usize {
        self.letter_count + self.digit_count + self.punctuation_count
    }

    fn noise_ratio(&self) -> f32 {
        let visible_char_count = self.visible_char_count();

        if visible_char_count == 0 {
            0.0
        } else {
            (self.digit_count + self.punctuation_count) as f32 / visible_char_count as f32
        }
    }

    fn lowercase_ratio(&self) -> f32 {
        if self.letter_count == 0 {
            0.0
        } else {
            self.lowercase_count as f32 / self.letter_count as f32
        }
    }

    fn vowel_word_ratio(&self) -> f32 {
        if self.alpha_word_count == 0 {
            0.0
        } else {
            self.vowel_word_count as f32 / self.alpha_word_count as f32
        }
    }
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

    #[test]
    fn normal_prose_is_not_flagged_unreadable() {
        let text =
            "The quick brown fox jumps over the lazy dog near the quiet river bank. ".repeat(3);
        assert!(!is_probably_unreadable_text(&text));
    }

    #[test]
    fn numeric_heavy_but_readable_text_is_not_flagged() {
        let text = "Total revenue for quarter one was 1234 and quarter two reached 5678, \
                    while expenses stayed near 910 leaving margin around 34 percent overall \
                    according to the annual report published today by the finance team.";
        assert!(!is_probably_unreadable_text(text));
    }

    #[test]
    fn garbled_vowelless_noise_is_flagged_unreadable() {
        let text = "X9#Z 7Q$W 3K%T ".repeat(10);
        assert!(is_probably_unreadable_text(&text));
    }

    #[test]
    fn short_text_is_never_flagged() {
        assert!(!is_probably_unreadable_text("9#@!X"));
        assert!(!is_probably_unreadable_text(""));
    }

    #[test]
    fn quality_metrics_ratios_handle_empty_input() {
        let metrics = analyze_text_quality("");
        assert_eq!(metrics.visible_char_count(), 0);
        assert_eq!(metrics.noise_ratio(), 0.0);
        assert_eq!(metrics.lowercase_ratio(), 0.0);
        assert_eq!(metrics.vowel_word_ratio(), 0.0);
    }

    #[test]
    fn quality_metrics_count_categories() {
        let metrics = analyze_text_quality("Ab1! cd");
        assert_eq!(metrics.letter_count, 4);
        assert_eq!(metrics.lowercase_count, 3);
        assert_eq!(metrics.digit_count, 1);
        assert_eq!(metrics.punctuation_count, 1);
        assert_eq!(metrics.alpha_word_count, 2);
        assert_eq!(metrics.vowel_word_count, 1);
    }
}
