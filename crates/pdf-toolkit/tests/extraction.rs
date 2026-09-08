mod common;

use common::{
    Text, minimal_empty_pdf, pdfium, write_garbled_pdf, write_pdf, write_pdf_with_image,
    write_rotated_pdf, write_simple_pdf,
};
use pdf_toolkit::error::PdfCutterError;
use pdf_toolkit::{
    ExtractionWarning, LlmReason, OutputFormat, PageSelection, PngSize, StructuredOutputFormat,
    document_page_count, extract_pages, extract_structured_text, extract_text, inspect_document,
};
use std::fs;
use std::path::{Path, PathBuf};

const A4_WIDTH_PX: u32 = 595;
const A4_HEIGHT_PX: u32 = 842;

fn selection(input: &str) -> PageSelection {
    PageSelection::parse(input).expect("test page selection must parse")
}

fn file_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .expect("read output dir")
        .map(|entry| {
            entry
                .expect("dir entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

fn fixture_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(name)
}

// ── Document inspection ────────────────────────────────────────────

#[test]
fn page_count_reports_total_pages() {
    let pdfium = pdfium();
    let dir = tempfile::tempdir().expect("temp dir");
    let input = fixture_path(dir.path(), "fixture.pdf");
    write_simple_pdf(&pdfium, &input, 3);

    assert_eq!(document_page_count(&pdfium, &input).expect("page count"), 3);
}

#[test]
fn inspect_document_reports_dimensions_and_image_stats() {
    let pdfium = pdfium();
    let dir = tempfile::tempdir().expect("temp dir");
    let input = fixture_path(dir.path(), "fixture.pdf");
    write_simple_pdf(&pdfium, &input, 2);

    let info = inspect_document(&pdfium, &input).expect("inspect document");
    assert_eq!(info.page_count, 2);
    assert_eq!(info.pages.len(), 2);

    for (index, page) in info.pages.iter().enumerate() {
        assert_eq!(page.page_number, index as u16 + 1);
        assert!(page.width_px.abs_diff(A4_WIDTH_PX) <= 1);
        assert!(page.height_px.abs_diff(A4_HEIGHT_PX) <= 1);
        assert!(!page.contains_images);
        assert_eq!(page.image_count, 0);
        assert_eq!(page.image_coverage, 0.0);
    }
}

#[test]
fn inspect_document_counts_embedded_images() {
    let pdfium = pdfium();
    let dir = tempfile::tempdir().expect("temp dir");
    let input = fixture_path(dir.path(), "with_image.pdf");
    write_pdf_with_image(&pdfium, &input, 200.0);

    let info = inspect_document(&pdfium, &input).expect("inspect document");
    let page = &info.pages[0];
    assert!(page.contains_images);
    assert_eq!(page.image_count, 1);
    assert!(
        page.image_coverage > 0.0 && page.image_coverage <= 1.0,
        "unexpected coverage {}",
        page.image_coverage
    );
}

#[test]
fn inspect_document_recommends_llm_for_image_only_pages() {
    let pdfium = pdfium();
    let dir = tempfile::tempdir().expect("temp dir");
    let input = fixture_path(dir.path(), "image_heavy.pdf");
    // 400pt image on A4 ≈ 32% coverage, no text at all.
    write_pdf_with_image(&pdfium, &input, 400.0);

    let info = inspect_document(&pdfium, &input).expect("inspect document");
    let page = &info.pages[0];
    assert!(page.llm_recommended());
    assert!(page.llm_reasons.contains(&LlmReason::NoTextLayer));
    assert!(page.llm_reasons.contains(&LlmReason::ImageHeavy));
}

#[test]
fn inspect_document_recommends_llm_for_garbled_pages() {
    let pdfium = pdfium();
    let dir = tempfile::tempdir().expect("temp dir");
    let input = fixture_path(dir.path(), "garbled.pdf");
    write_garbled_pdf(&pdfium, &input);

    let info = inspect_document(&pdfium, &input).expect("inspect document");
    assert_eq!(info.pages[0].llm_reasons, vec![LlmReason::UnreadableText]);
}

#[test]
fn inspect_document_flags_complex_tables() {
    let pdfium = pdfium();
    let dir = tempfile::tempdir().expect("temp dir");

    // Three columns with a wrapped cell: structure is approximate.
    let wrapped = fixture_path(dir.path(), "wrapped_table.pdf");
    write_pdf(
        &pdfium,
        &wrapped,
        &[vec![
            Text::plain(72.0, 700.0, 12.0, "A1"),
            Text::plain(200.0, 700.0, 12.0, "Agriculture uses"),
            Text::plain(430.0, 700.0, 12.0, "5 acres"),
            Text::plain(200.0, 685.0, 12.0, "One family dwellings"),
            Text::plain(72.0, 670.0, 12.0, "A2"),
            Text::plain(200.0, 670.0, 12.0, "More farming"),
            Text::plain(430.0, 670.0, 12.0, "2 acres"),
        ]],
    );
    let info = inspect_document(&pdfium, &wrapped).expect("inspect document");
    assert_eq!(info.pages[0].llm_reasons, vec![LlmReason::ComplexTable]);

    // A regular grid without wrapped rows extracts reliably: not flagged.
    let simple = fixture_path(dir.path(), "simple_table.pdf");
    write_pdf(
        &pdfium,
        &simple,
        &[vec![
            Text::plain(72.0, 700.0, 12.0, "Name"),
            Text::plain(250.0, 700.0, 12.0, "Size"),
            Text::plain(430.0, 700.0, 12.0, "Width"),
            Text::plain(72.0, 680.0, 12.0, "Alpha"),
            Text::plain(250.0, 680.0, 12.0, "1"),
            Text::plain(430.0, 680.0, 12.0, "2"),
            Text::plain(72.0, 660.0, 12.0, "Beta"),
            Text::plain(250.0, 660.0, 12.0, "3"),
            Text::plain(430.0, 660.0, 12.0, "4"),
        ]],
    );
    let info = inspect_document(&pdfium, &simple).expect("inspect document");
    assert!(info.pages[0].llm_reasons.is_empty());
}

#[test]
fn inspect_document_does_not_recommend_llm_for_text_pages() {
    let pdfium = pdfium();
    let dir = tempfile::tempdir().expect("temp dir");
    let input = fixture_path(dir.path(), "fixture.pdf");
    write_simple_pdf(&pdfium, &input, 1);

    let info = inspect_document(&pdfium, &input).expect("inspect document");
    assert!(!info.pages[0].llm_recommended());
    assert!(info.pages[0].llm_reasons.is_empty());
}

#[test]
fn opening_missing_or_corrupt_files_fails() {
    let pdfium = pdfium();
    let dir = tempfile::tempdir().expect("temp dir");

    let missing = fixture_path(dir.path(), "missing.pdf");
    assert!(matches!(
        document_page_count(&pdfium, &missing),
        Err(PdfCutterError::OpenPdf { .. })
    ));

    let corrupt = fixture_path(dir.path(), "corrupt.pdf");
    fs::write(&corrupt, b"this is not a pdf").expect("write corrupt file");
    assert!(matches!(
        document_page_count(&pdfium, &corrupt),
        Err(PdfCutterError::OpenPdf { .. })
    ));
}

#[test]
fn zero_page_document_is_rejected_by_extraction() {
    let pdfium = pdfium();
    let dir = tempfile::tempdir().expect("temp dir");
    let input = fixture_path(dir.path(), "empty.pdf");
    fs::write(&input, minimal_empty_pdf()).expect("write empty pdf");

    assert_eq!(document_page_count(&pdfium, &input).expect("page count"), 0);

    let out_dir = dir.path().join("out");
    assert!(matches!(
        extract_pages(
            &pdfium,
            &input,
            PageSelection::All,
            OutputFormat::Pdf { concat: false },
            Some(&out_dir),
        ),
        Err(PdfCutterError::EmptyDocument)
    ));
    assert!(matches!(
        extract_text(&pdfium, &input, PageSelection::All, false, Some(&out_dir)),
        Err(PdfCutterError::EmptyDocument)
    ));
    assert!(matches!(
        extract_structured_text(
            &pdfium,
            &input,
            PageSelection::All,
            StructuredOutputFormat::Full
        ),
        Err(PdfCutterError::EmptyDocument)
    ));
}

// ── Page extraction: PDF ───────────────────────────────────────────

#[test]
fn extracts_each_selected_page_as_pdf() {
    let pdfium = pdfium();
    let dir = tempfile::tempdir().expect("temp dir");
    let input = fixture_path(dir.path(), "fixture.pdf");
    write_simple_pdf(&pdfium, &input, 3);
    let out_dir = dir.path().join("out");

    let report = extract_pages(
        &pdfium,
        &input,
        PageSelection::All,
        OutputFormat::Pdf { concat: false },
        Some(&out_dir),
    )
    .expect("extract pages");

    assert!(report.warnings.is_empty());
    assert_eq!(report.pages.len(), 3);
    assert_eq!(report.source_pages.len(), 3);
    assert_eq!(
        file_names(&out_dir),
        vec![
            "fixture_page_1.pdf",
            "fixture_page_2.pdf",
            "fixture_page_3.pdf"
        ]
    );

    for (index, page) in report.pages.iter().enumerate() {
        assert_eq!(page.page_number, Some(index as u16 + 1));
        assert_eq!(page.page_count, 1);
        assert!(page.size_kb >= 1);
        assert!(!page.contains_images);
        assert_eq!(
            document_page_count(&pdfium, &page.output_path).expect("reload output"),
            1
        );
    }
}

#[test]
fn warns_on_out_of_bounds_pages() {
    let pdfium = pdfium();
    let dir = tempfile::tempdir().expect("temp dir");
    let input = fixture_path(dir.path(), "fixture.pdf");
    write_simple_pdf(&pdfium, &input, 3);
    let out_dir = dir.path().join("out");

    let report = extract_pages(
        &pdfium,
        &input,
        selection("1,99"),
        OutputFormat::Pdf { concat: false },
        Some(&out_dir),
    )
    .expect("extract pages");

    assert_eq!(report.pages.len(), 1);
    assert_eq!(
        report.warnings,
        vec![ExtractionWarning::PageOutOfBounds {
            page_number: 99,
            total_pages: 3,
        }]
    );
}

#[test]
fn dedups_duplicate_page_selection() {
    let pdfium = pdfium();
    let dir = tempfile::tempdir().expect("temp dir");
    let input = fixture_path(dir.path(), "fixture.pdf");
    write_simple_pdf(&pdfium, &input, 3);
    let out_dir = dir.path().join("out");

    let report = extract_pages(
        &pdfium,
        &input,
        selection("2,2"),
        OutputFormat::Pdf { concat: false },
        Some(&out_dir),
    )
    .expect("extract pages");

    assert_eq!(report.pages.len(), 1);
    assert_eq!(file_names(&out_dir), vec!["fixture_page_2.pdf"]);
}

#[test]
fn concat_merges_selected_pages_into_one_pdf() {
    let pdfium = pdfium();
    let dir = tempfile::tempdir().expect("temp dir");
    let input = fixture_path(dir.path(), "fixture.pdf");
    write_simple_pdf(&pdfium, &input, 3);
    let out_dir = dir.path().join("out");

    let report = extract_pages(
        &pdfium,
        &input,
        selection("1,3"),
        OutputFormat::Pdf { concat: true },
        Some(&out_dir),
    )
    .expect("extract pages");

    assert_eq!(report.pages.len(), 1);
    let merged = &report.pages[0];
    assert_eq!(merged.page_number, None);
    assert_eq!(merged.page_count, 2);
    assert_eq!(file_names(&out_dir), vec!["fixture_concat.pdf"]);
    assert_eq!(
        document_page_count(&pdfium, &merged.output_path).expect("reload output"),
        2
    );

    let source_numbers: Vec<u16> = report
        .source_pages
        .iter()
        .map(|page| page.page_number)
        .collect();
    assert_eq!(source_numbers, vec![1, 3]);
}

#[test]
fn concat_with_only_out_of_bounds_pages_creates_nothing() {
    let pdfium = pdfium();
    let dir = tempfile::tempdir().expect("temp dir");
    let input = fixture_path(dir.path(), "fixture.pdf");
    write_simple_pdf(&pdfium, &input, 2);
    let out_dir = dir.path().join("out");

    let report = extract_pages(
        &pdfium,
        &input,
        selection("99"),
        OutputFormat::Pdf { concat: true },
        Some(&out_dir),
    )
    .expect("extract pages");

    assert!(report.pages.is_empty());
    assert!(report.source_pages.is_empty());
    assert_eq!(report.warnings.len(), 1);
    assert!(file_names(&out_dir).is_empty());
}

#[test]
fn collision_appends_counter_to_output_names() {
    let pdfium = pdfium();
    let dir = tempfile::tempdir().expect("temp dir");
    let input = fixture_path(dir.path(), "fixture.pdf");
    write_simple_pdf(&pdfium, &input, 1);
    let out_dir = dir.path().join("out");

    for _ in 0..2 {
        extract_pages(
            &pdfium,
            &input,
            selection("1"),
            OutputFormat::Pdf { concat: false },
            Some(&out_dir),
        )
        .expect("extract pages");
    }

    assert_eq!(
        file_names(&out_dir),
        vec!["fixture_page_1.pdf", "fixture_page_1_1.pdf"]
    );
}

#[test]
fn sanitizes_output_stem_from_input_name() {
    let pdfium = pdfium();
    let dir = tempfile::tempdir().expect("temp dir");
    let input = fixture_path(dir.path(), "My Doc (1).pdf");
    write_simple_pdf(&pdfium, &input, 1);
    let out_dir = dir.path().join("out");

    extract_pages(
        &pdfium,
        &input,
        PageSelection::All,
        OutputFormat::Pdf { concat: false },
        Some(&out_dir),
    )
    .expect("extract pages");

    assert_eq!(file_names(&out_dir), vec!["My_Doc_1_page_1.pdf"]);
}

// ── Page extraction: PNG ───────────────────────────────────────────

#[test]
fn renders_png_at_scale() {
    let pdfium = pdfium();
    let dir = tempfile::tempdir().expect("temp dir");
    let input = fixture_path(dir.path(), "fixture.pdf");
    write_simple_pdf(&pdfium, &input, 1);
    let out_dir = dir.path().join("out");

    let report = extract_pages(
        &pdfium,
        &input,
        PageSelection::All,
        OutputFormat::Png {
            size: PngSize::Scale(1.0),
        },
        Some(&out_dir),
    )
    .expect("extract pages");

    assert_eq!(file_names(&out_dir), vec!["fixture_page_1.png"]);
    let page = &report.pages[0];
    assert!(page.width_px.abs_diff(A4_WIDTH_PX) <= 1);
    assert!(page.height_px.abs_diff(A4_HEIGHT_PX) <= 1);

    let (png_width, png_height) =
        image::image_dimensions(&page.output_path).expect("decode png dimensions");
    assert!(png_width.abs_diff(page.width_px) <= 1);
    assert!(png_height.abs_diff(page.height_px) <= 1);
}

#[test]
fn renders_png_at_target_width_preserving_aspect_ratio() {
    let pdfium = pdfium();
    let dir = tempfile::tempdir().expect("temp dir");
    let input = fixture_path(dir.path(), "fixture.pdf");
    write_simple_pdf(&pdfium, &input, 1);
    let out_dir = dir.path().join("out");

    let report = extract_pages(
        &pdfium,
        &input,
        PageSelection::All,
        OutputFormat::Png {
            size: PngSize::WidthPx(200),
        },
        Some(&out_dir),
    )
    .expect("extract pages");

    let page = &report.pages[0];
    assert_eq!(page.width_px, 200);
    let expected_height = (A4_HEIGHT_PX as f32 * 200.0 / A4_WIDTH_PX as f32).round() as u32;
    assert!(page.height_px.abs_diff(expected_height) <= 1);

    let (png_width, png_height) =
        image::image_dimensions(&page.output_path).expect("decode png dimensions");
    assert!(png_width.abs_diff(200) <= 1);
    assert!(png_height.abs_diff(expected_height) <= 1);
}

#[test]
fn extract_pages_rejects_invalid_formats_without_panicking() {
    let pdfium = pdfium();
    let dir = tempfile::tempdir().expect("temp dir");
    let input = fixture_path(dir.path(), "fixture.pdf");
    write_simple_pdf(&pdfium, &input, 1);
    let out_dir = dir.path().join("out");

    let oversized = extract_pages(
        &pdfium,
        &input,
        PageSelection::All,
        OutputFormat::Png {
            size: PngSize::WidthPx(i32::MAX as u32 + 1),
        },
        Some(&out_dir),
    );
    assert!(matches!(
        oversized,
        Err(PdfCutterError::InvalidImageWidth { .. })
    ));

    let bad_scale = extract_pages(
        &pdfium,
        &input,
        PageSelection::All,
        OutputFormat::Png {
            size: PngSize::Scale(f32::NAN),
        },
        Some(&out_dir),
    );
    assert!(matches!(
        bad_scale,
        Err(PdfCutterError::InvalidImageScale { .. })
    ));
}

// ── Text extraction ────────────────────────────────────────────────

#[test]
fn extracts_all_pages_into_single_text_file() {
    let pdfium = pdfium();
    let dir = tempfile::tempdir().expect("temp dir");
    let input = fixture_path(dir.path(), "fixture.pdf");
    write_simple_pdf(&pdfium, &input, 3);
    let out_dir = dir.path().join("out");

    let report = extract_text(&pdfium, &input, PageSelection::All, false, Some(&out_dir))
        .expect("extract text");

    assert_eq!(report.selected_page_count, 3);
    assert_eq!(report.files.len(), 1);
    assert_eq!(report.files[0].page_count, 3);
    assert_eq!(file_names(&out_dir), vec!["fixture_text.txt"]);

    let content = fs::read_to_string(&report.files[0].output_path).expect("read text output");
    for page_number in 1..=3 {
        assert!(content.contains(&format!("____Page{page_number}____")));
        assert!(content.contains(&format!("page number {page_number}.")));
    }
}

#[test]
fn concat_text_uses_concat_file_name() {
    let pdfium = pdfium();
    let dir = tempfile::tempdir().expect("temp dir");
    let input = fixture_path(dir.path(), "fixture.pdf");
    write_simple_pdf(&pdfium, &input, 2);
    let out_dir = dir.path().join("out");

    let report = extract_text(&pdfium, &input, selection("1,2"), true, Some(&out_dir))
        .expect("extract text");

    assert_eq!(report.files.len(), 1);
    assert_eq!(report.files[0].page_number, None);
    assert_eq!(file_names(&out_dir), vec!["fixture_concat.txt"]);
}

#[test]
fn specific_pages_write_per_page_text_files() {
    let pdfium = pdfium();
    let dir = tempfile::tempdir().expect("temp dir");
    let input = fixture_path(dir.path(), "fixture.pdf");
    write_simple_pdf(&pdfium, &input, 3);
    let out_dir = dir.path().join("out");

    let report = extract_text(&pdfium, &input, selection("2,3"), false, Some(&out_dir))
        .expect("extract text");

    assert_eq!(report.files.len(), 2);
    assert_eq!(report.files[0].page_number, Some(2));
    assert_eq!(report.files[1].page_number, Some(3));
    assert_eq!(
        file_names(&out_dir),
        vec!["fixture_page_2.txt", "fixture_page_3.txt"]
    );

    let content = fs::read_to_string(&report.files[0].output_path).expect("read text output");
    assert!(content.contains("____Page2____"));
    assert!(content.contains("page number 2."));
    assert!(!content.contains("page number 3."));
}

#[test]
fn out_of_bounds_text_selection_produces_no_files() {
    let pdfium = pdfium();
    let dir = tempfile::tempdir().expect("temp dir");
    let input = fixture_path(dir.path(), "fixture.pdf");
    write_simple_pdf(&pdfium, &input, 2);
    let out_dir = dir.path().join("out");

    let report = extract_text(&pdfium, &input, selection("99"), false, Some(&out_dir))
        .expect("extract text");

    assert!(report.files.is_empty());
    assert_eq!(
        report.warnings,
        vec![ExtractionWarning::PageOutOfBounds {
            page_number: 99,
            total_pages: 2,
        }]
    );
    assert!(file_names(&out_dir).is_empty());
}

#[test]
fn garbled_page_is_skipped_with_unreadable_warning() {
    let pdfium = pdfium();
    let dir = tempfile::tempdir().expect("temp dir");
    let input = fixture_path(dir.path(), "garbled.pdf");
    write_garbled_pdf(&pdfium, &input);
    let out_dir = dir.path().join("out");

    let report = extract_text(&pdfium, &input, PageSelection::All, false, Some(&out_dir))
        .expect("extract text");

    assert!(report.files.is_empty());
    assert_eq!(
        report.warnings,
        vec![ExtractionWarning::UnreadableText { page_number: 1 }]
    );
    assert!(file_names(&out_dir).is_empty());
}

// ── Structured extraction ──────────────────────────────────────────

#[test]
fn structured_full_format_includes_spans() {
    let pdfium = pdfium();
    let dir = tempfile::tempdir().expect("temp dir");
    let input = fixture_path(dir.path(), "fixture.pdf");
    write_simple_pdf(&pdfium, &input, 2);

    let report = extract_structured_text(
        &pdfium,
        &input,
        PageSelection::All,
        StructuredOutputFormat::Full,
    )
    .expect("extract structured text");

    assert_eq!(report.selected_page_count, 2);
    assert_eq!(report.pages.len(), 2);

    for (index, page) in report.pages.iter().enumerate() {
        assert_eq!(page.page, index as u16 + 1);
        assert!(!page.spans.is_empty(), "page {} must have spans", page.page);
        assert!(!page.markdown.is_empty());

        let span = &page.spans[0];
        assert!((span.font_size - 12.0).abs() < 1.0);
        assert!(span.x > 0.0 && span.y > 0.0);
        assert!(span.width > 0.0 && span.height > 0.0);
        assert!(!span.font_name.is_empty());
    }
}

#[test]
fn structured_markdown_only_omits_spans() {
    let pdfium = pdfium();
    let dir = tempfile::tempdir().expect("temp dir");
    let input = fixture_path(dir.path(), "fixture.pdf");
    write_simple_pdf(&pdfium, &input, 1);

    let report = extract_structured_text(
        &pdfium,
        &input,
        PageSelection::All,
        StructuredOutputFormat::MarkdownOnly,
    )
    .expect("extract structured text");

    assert_eq!(report.pages.len(), 1);
    assert!(report.pages[0].spans.is_empty());
    assert!(report.pages[0].markdown.contains("page number 1"));
}

#[test]
fn structured_detects_heading_list_and_bold() {
    let pdfium = pdfium();
    let dir = tempfile::tempdir().expect("temp dir");
    let input = fixture_path(dir.path(), "layout.pdf");
    write_pdf(
        &pdfium,
        &input,
        &[vec![
            Text::plain(72.0, 770.0, 24.0, "Document Title"),
            Text::plain(
                72.0,
                720.0,
                12.0,
                "This opening paragraph carries enough body text",
            ),
            Text::plain(
                72.0,
                705.0,
                12.0,
                "to establish the dominant font size of the page.",
            ),
            Text::plain(72.0, 675.0, 12.0, "Normal"),
            Text::bold(120.0, 675.0, 12.0, "Bolded"),
            Text::plain(72.0, 645.0, 12.0, "\u{2022} First item"),
            Text::plain(72.0, 630.0, 12.0, "\u{2022} Second item"),
        ]],
    );

    let report = extract_structured_text(
        &pdfium,
        &input,
        PageSelection::All,
        StructuredOutputFormat::Full,
    )
    .expect("extract structured text");

    let markdown = &report.pages[0].markdown;
    assert!(
        markdown.contains("# Document Title"),
        "missing heading in: {markdown}"
    );
    assert!(
        markdown.contains("**Bolded**"),
        "missing bold text in: {markdown}"
    );
    assert!(
        markdown.contains("- First item") && markdown.contains("- Second item"),
        "missing list items in: {markdown}"
    );

    // Pdfium reports the standard Helvetica-Bold font with weight 0 and no
    // bold reenforcement; boldness is detected from the font name instead.
    let bold_span = report.pages[0]
        .spans
        .iter()
        .find(|span| span.text.contains("Bolded"))
        .expect("bold span must exist");
    assert!(bold_span.bold);
    assert!(
        !bold_span.text.contains("Normal"),
        "bold span must not merge into regular text"
    );
}

#[test]
fn structured_detects_table_columns() {
    let pdfium = pdfium();
    let dir = tempfile::tempdir().expect("temp dir");
    let input = fixture_path(dir.path(), "table.pdf");
    write_pdf(
        &pdfium,
        &input,
        &[vec![
            Text::plain(72.0, 700.0, 12.0, "Name"),
            Text::plain(300.0, 700.0, 12.0, "Value"),
            Text::plain(72.0, 680.0, 12.0, "Alpha"),
            Text::plain(300.0, 680.0, 12.0, "1"),
            Text::plain(72.0, 660.0, 12.0, "Beta"),
            Text::plain(300.0, 660.0, 12.0, "2"),
        ]],
    );

    let report = extract_structured_text(
        &pdfium,
        &input,
        PageSelection::All,
        StructuredOutputFormat::Full,
    )
    .expect("extract structured text");

    let markdown = &report.pages[0].markdown;
    assert!(
        markdown.contains("| --- |"),
        "missing table separator: {markdown}"
    );
    assert!(
        markdown.contains("Alpha") && markdown.contains("Beta"),
        "missing table rows: {markdown}"
    );
}

#[test]
fn structured_skips_garbled_pages_with_warning() {
    let pdfium = pdfium();
    let dir = tempfile::tempdir().expect("temp dir");
    let input = fixture_path(dir.path(), "garbled.pdf");
    write_garbled_pdf(&pdfium, &input);

    let report = extract_structured_text(
        &pdfium,
        &input,
        PageSelection::All,
        StructuredOutputFormat::Full,
    )
    .expect("extract structured text");

    assert!(report.pages.is_empty());
    assert_eq!(
        report.warnings,
        vec![ExtractionWarning::UnreadableText { page_number: 1 }]
    );
}

#[test]
fn structured_orders_lines_visually_not_by_content_stream() {
    let pdfium = pdfium();
    let dir = tempfile::tempdir().expect("temp dir");
    let input = fixture_path(dir.path(), "reversed.pdf");
    // Written bottom-up: the content stream order is the reverse of the
    // visual order.
    write_pdf(
        &pdfium,
        &input,
        &[vec![
            Text::plain(72.0, 640.0, 12.0, "gamma bottom line text"),
            Text::plain(72.0, 680.0, 12.0, "beta middle line text"),
            Text::plain(72.0, 720.0, 12.0, "alpha top line text"),
        ]],
    );

    let report = extract_structured_text(
        &pdfium,
        &input,
        PageSelection::All,
        StructuredOutputFormat::MarkdownOnly,
    )
    .expect("extract structured text");

    let markdown = &report.pages[0].markdown;
    let alpha = markdown.find("alpha").expect("alpha present");
    let beta = markdown.find("beta").expect("beta present");
    let gamma = markdown.find("gamma").expect("gamma present");
    assert!(
        alpha < beta && beta < gamma,
        "lines must appear in visual order: {markdown}"
    );
}

#[test]
fn structured_reads_two_column_pages_column_by_column() {
    let pdfium = pdfium();
    let dir = tempfile::tempdir().expect("temp dir");
    let input = fixture_path(dir.path(), "columns.pdf");

    let mut items = Vec::new();
    for row in 0..5 {
        items.push(Text::plain(
            60.0,
            700.0 - row as f32 * 16.0,
            12.0,
            &format!("alpha line {row} text"),
        ));
        items.push(Text::plain(
            340.0,
            692.0 - row as f32 * 16.0,
            12.0,
            &format!("beta line {row} text"),
        ));
    }
    write_pdf(&pdfium, &input, &[items]);

    let report = extract_structured_text(
        &pdfium,
        &input,
        PageSelection::All,
        StructuredOutputFormat::MarkdownOnly,
    )
    .expect("extract structured text");

    let markdown = &report.pages[0].markdown;
    let last_alpha = markdown
        .rfind("alpha line 4")
        .expect("alpha line 4 present");
    let first_beta = markdown.find("beta line 0").expect("beta line 0 present");
    assert!(
        last_alpha < first_beta,
        "left column must fully precede right column: {markdown}"
    );
}

#[test]
fn structured_reads_rotated_pages_in_visual_order() {
    let pdfium = pdfium();
    let dir = tempfile::tempdir().expect("temp dir");
    let input = fixture_path(dir.path(), "rotated.pdf");
    // On a 90°-rotated page, smaller page-space x means higher on the
    // displayed page.
    write_rotated_pdf(
        &pdfium,
        &input,
        &[
            Text::plain(200.0, 100.0, 12.0, "beta rotated lower line"),
            Text::plain(100.0, 100.0, 12.0, "alpha rotated upper line"),
        ],
    );

    let report = extract_structured_text(
        &pdfium,
        &input,
        PageSelection::All,
        StructuredOutputFormat::MarkdownOnly,
    )
    .expect("extract structured text");

    let markdown = &report.pages[0].markdown;
    let alpha = markdown
        .find("alpha rotated")
        .unwrap_or_else(|| panic!("alpha missing in: {markdown:?}"));
    let beta = markdown
        .find("beta rotated")
        .unwrap_or_else(|| panic!("beta missing in: {markdown:?}"));
    assert!(
        alpha < beta,
        "rotated page must read top-to-bottom visually: {markdown}"
    );
}

#[test]
fn structured_merges_wrapped_table_rows() {
    let pdfium = pdfium();
    let dir = tempfile::tempdir().expect("temp dir");
    let input = fixture_path(dir.path(), "wrapped_table.pdf");
    write_pdf(
        &pdfium,
        &input,
        &[vec![
            Text::plain(72.0, 700.0, 12.0, "A1"),
            Text::plain(200.0, 700.0, 12.0, "Agriculture uses"),
            Text::plain(430.0, 700.0, 12.0, "5 acres"),
            Text::plain(200.0, 685.0, 12.0, "One family dwellings"),
            Text::plain(72.0, 670.0, 12.0, "A2"),
            Text::plain(200.0, 670.0, 12.0, "More farming"),
            Text::plain(430.0, 670.0, 12.0, "2 acres"),
        ]],
    );

    let report = extract_structured_text(
        &pdfium,
        &input,
        PageSelection::All,
        StructuredOutputFormat::MarkdownOnly,
    )
    .expect("extract structured text");

    let markdown = &report.pages[0].markdown;
    assert!(
        markdown.contains("| A1 | Agriculture uses One family dwellings | 5 acres |"),
        "wrapped cell must merge into its row: {markdown}"
    );
    assert!(
        markdown.contains("| A2 | More farming | 2 acres |"),
        "following row must stay intact: {markdown}"
    );
}

#[test]
fn structured_selection_reports_out_of_bounds() {
    let pdfium = pdfium();
    let dir = tempfile::tempdir().expect("temp dir");
    let input = fixture_path(dir.path(), "fixture.pdf");
    write_simple_pdf(&pdfium, &input, 2);

    let report = extract_structured_text(
        &pdfium,
        &input,
        selection("2,9"),
        StructuredOutputFormat::Full,
    )
    .expect("extract structured text");

    assert_eq!(report.selected_page_count, 1);
    assert_eq!(report.pages.len(), 1);
    assert_eq!(report.pages[0].page, 2);
    assert_eq!(
        report.warnings,
        vec![ExtractionWarning::PageOutOfBounds {
            page_number: 9,
            total_pages: 2,
        }]
    );
}
