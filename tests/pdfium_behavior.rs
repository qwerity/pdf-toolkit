//! Pins a pdfium behavior the text-quality heuristic depends on: characters
//! positioned beyond the page boundary are dropped from text extraction.

mod common;

use common::{Text, pdfium, write_pdf};
use pdf_cutter::{PageSelection, extract_text};

/// Pins the pdfium behavior the extractor's font-size fallback depends on:
/// for text drawn with a rotated matrix, `scaled_font_size` collapses to
/// (approximately) zero, while `unscaled_font_size` keeps the /Tf size.
#[test]
fn rotated_text_reports_zero_scaled_font_size() {
    let pdfium = pdfium();
    let dir = tempfile::tempdir().expect("temp dir");
    let input = dir.path().join("rotated.pdf");
    crate::common::write_rotated_pdf(
        &pdfium,
        &input,
        &[Text::plain(100.0, 100.0, 12.0, "alpha rotated")],
    );

    let document = pdfium.load_pdf_from_file(&input, None).expect("load");
    let pages = document.pages();
    let page = pages.get(0).expect("page");
    let text = page.text().expect("text");
    let first = text.chars().first().expect("first char");

    assert!(
        first.scaled_font_size().value <= 0.0,
        "expected collapsed scaled size, got {}",
        first.scaled_font_size().value
    );
    assert!(
        (first.unscaled_font_size().value - 12.0).abs() < 0.5,
        "expected /Tf size ~12, got {}",
        first.unscaled_font_size().value
    );
}

#[test]
fn characters_beyond_page_edge_are_not_extracted() {
    let pdfium = pdfium();
    let dir = tempfile::tempdir().expect("temp dir");
    let input = dir.path().join("overflow.pdf");
    // ~1050pt of text on a 595pt-wide A4 page: roughly half must be clipped.
    let overflowing = "X9#Z 7Q$W 3K%T ".repeat(10);
    write_pdf(
        &pdfium,
        &input,
        &[vec![Text::plain(72.0, 720.0, 12.0, &overflowing)]],
    );

    let out_dir = dir.path().join("out");
    let report = extract_text(&pdfium, &input, PageSelection::All, false, Some(&out_dir))
        .expect("extract text");

    assert_eq!(report.files.len(), 1);
    let content = std::fs::read_to_string(&report.files[0].output_path).expect("read output");
    let extracted_units = content.matches("X9#Z").count();
    assert!(
        extracted_units < 10,
        "expected off-page text to be clipped, got {extracted_units} units"
    );
}
