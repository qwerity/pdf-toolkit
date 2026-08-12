//! Pins a pdfium behavior the text-quality heuristic depends on: characters
//! positioned beyond the page boundary are dropped from text extraction.

mod common;

use common::{Text, pdfium, write_pdf};
use pdf_cutter::{PageSelection, extract_text};

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
