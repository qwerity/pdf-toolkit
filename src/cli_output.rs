use anyhow::{Context, Result, bail};
use pdf_cutter::cli::Args;
use pdf_cutter::{
    DocumentInfo, ExtractionReport, ExtractionWarning, OutputFormat, PageInfo, PngSize,
    StructuredExtractionReport, TextExtractionReport,
};
use serde_json::{Map, Value, json};

pub fn print_document_info(document_info: &DocumentInfo) -> Result<()> {
    print_json(document_info_json(document_info))
}

pub fn print_structured_output(report: &StructuredExtractionReport) -> Result<()> {
    print_warnings(&report.warnings);

    let output = structured_output_json(report);
    print_json(output)
}

pub fn print_text_output(args: &Args, report: &TextExtractionReport) -> Result<()> {
    if report.files.is_empty() {
        if !report.warnings.is_empty() {
            return print_json(text_output_json(args, report)?);
        }

        bail!(
            "no text was extracted; check that the requested pages are within range and the output directory is writable"
        );
    }

    print_warnings(&report.warnings);
    print_json(text_output_json(args, report)?)
}

pub fn print_page_output(
    args: &Args,
    output_format: OutputFormat,
    report: &ExtractionReport,
) -> Result<()> {
    print_warnings(&report.warnings);

    if report.pages.is_empty() {
        bail!(
            "no pages were extracted; check that the requested pages are within range and the output directory is writable"
        );
    }

    print_json(page_output_json(args, output_format, report)?)
}

fn page_output_json(
    args: &Args,
    output_format: OutputFormat,
    report: &ExtractionReport,
) -> Result<Value> {
    let mut output = Map::new();
    add_png_settings(&mut output, output_format);

    if args.concat {
        let file = report
            .pages
            .first()
            .context("concat output must include a generated file")?;

        output.insert(
            "concat".to_string(),
            json!({
                "path": file.output_path.display().to_string(),
                "pages": args.pages.raw(),
                "size_kb": file.size_kb,
            }),
        );
        add_page_info(&mut output, "page_info", &report.source_pages);

        return Ok(Value::Object(output));
    }

    let pages = report
        .pages
        .iter()
        .map(|page| {
            let page_number = page
                .page_number
                .context("page extraction results must include the source page number")?;

            Ok((
                page_number.to_string(),
                json!({
                    "path": page.output_path.display().to_string(),
                    "size_kb": page.size_kb,
                    "width_px": page.width_px,
                    "height_px": page.height_px,
                    "contains_images": page.contains_images,
                }),
            ))
        })
        .collect::<Result<Map<String, Value>>>()?;

    if !pages.is_empty() {
        output.insert("pages".to_string(), Value::Object(pages));
    }

    Ok(Value::Object(output))
}

fn text_output_json(args: &Args, report: &TextExtractionReport) -> Result<Value> {
    let mut output = Map::new();
    add_errors(&mut output, &report.warnings);

    if is_aggregate_text_output(report) {
        let file = report
            .files
            .first()
            .context("concat text output must include a generated file")?;

        output.insert(
            "concat".to_string(),
            json!({
                "path": file.output_path.display().to_string(),
                "pages": args.pages.raw(),
                "size_kb": file.size_kb,
            }),
        );

        return Ok(Value::Object(output));
    }

    let pages = report
        .files
        .iter()
        .map(|file| {
            let page_number = file
                .page_number
                .context("text extraction results must include the source page number")?;

            Ok((
                page_number.to_string(),
                json!({
                    "path": file.output_path.display().to_string(),
                    "size_kb": file.size_kb,
                }),
            ))
        })
        .collect::<Result<Map<String, Value>>>()?;

    if !pages.is_empty() {
        output.insert("pages".to_string(), Value::Object(pages));
    }

    Ok(Value::Object(output))
}

fn structured_output_json(report: &StructuredExtractionReport) -> Value {
    let mut output = Map::new();
    add_errors(&mut output, &report.warnings);

    let pages: Vec<Value> = report
        .pages
        .iter()
        .map(|page| {
            let mut page_obj = Map::new();
            page_obj.insert("page".to_string(), json!(page.page));
            page_obj.insert("markdown".to_string(), json!(page.markdown));

            if !page.spans.is_empty() {
                let spans: Vec<Value> = page
                    .spans
                    .iter()
                    .map(|span| {
                        json!({
                            "text": span.text,
                            "font_size": (span.font_size * 100.0).round() / 100.0,
                            "font_name": span.font_name,
                            "bold": span.bold,
                            "italic": span.italic,
                            "x": (span.x * 10.0).round() / 10.0,
                            "y": (span.y * 10.0).round() / 10.0,
                            "width": (span.width * 10.0).round() / 10.0,
                            "height": (span.height * 10.0).round() / 10.0,
                        })
                    })
                    .collect();
                page_obj.insert("spans".to_string(), Value::Array(spans));
            }

            Value::Object(page_obj)
        })
        .collect();

    output.insert("pages".to_string(), Value::Array(pages));
    output.insert(
        "selected_page_count".to_string(),
        json!(report.selected_page_count),
    );

    Value::Object(output)
}

fn document_info_json(document_info: &DocumentInfo) -> Value {
    let mut output = Map::new();

    output.insert("page_count".to_string(), json!(document_info.page_count));
    add_page_info(&mut output, "pages", &document_info.pages);

    Value::Object(output)
}

fn is_aggregate_text_output(report: &TextExtractionReport) -> bool {
    report
        .files
        .first()
        .is_some_and(|file| file.page_number.is_none())
}

fn add_png_settings(output: &mut Map<String, Value>, output_format: OutputFormat) {
    if let OutputFormat::Png { size } = output_format {
        match size {
            PngSize::Scale(scale) => {
                output.insert("scale".to_string(), json!(scale));
            }
            PngSize::WidthPx(width_px) => {
                output.insert("scale_width_px".to_string(), json!(width_px));
            }
        }
    }
}

fn add_page_info(output: &mut Map<String, Value>, key: &str, page_info: &[PageInfo]) {
    if page_info.is_empty() {
        return;
    }

    output.insert(key.to_string(), page_info_map(page_info));
}

fn page_info_map(page_info: &[PageInfo]) -> Value {
    Value::Object(
        page_info
            .iter()
            .map(|page| (page.page_number.to_string(), single_page_info_json(page)))
            .collect(),
    )
}

fn single_page_info_json(page_info: &PageInfo) -> Value {
    json!({
        "width_px": page_info.width_px,
        "height_px": page_info.height_px,
        "contains_images": page_info.contains_images,
        "image_count": page_info.image_count,
        "image_coverage": page_info.image_coverage,
    })
}

fn add_errors(output: &mut Map<String, Value>, warnings: &[ExtractionWarning]) {
    let errors = warnings
        .iter()
        .map(|warning| {
            (
                warning_page(warning).to_string(),
                Value::String(warning.to_string()),
            )
        })
        .collect::<Map<String, Value>>();

    if !errors.is_empty() {
        output.insert("errors".to_string(), Value::Object(errors));
    }
}

fn warning_page(warning: &ExtractionWarning) -> u16 {
    match warning {
        ExtractionWarning::PageOutOfBounds { page_number, .. }
        | ExtractionWarning::LoadPage { page_number, .. }
        | ExtractionWarning::UnreadableText { page_number }
        | ExtractionWarning::SavePage { page_number, .. } => *page_number,
    }
}

fn print_warnings(warnings: &[impl std::fmt::Display]) {
    for warning in warnings {
        eprintln!("Warning: {warning}");
    }
}

fn print_json(value: Value) -> Result<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(&value).context("failed to serialize JSON output")?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdf_cutter::{PageResult, StructuredPage, TextFileResult, TextSpan};
    use std::path::PathBuf;

    fn args(extra: &[&str]) -> Args {
        Args::try_from_flags(extra).expect("arguments must parse")
    }

    fn page_result(page_number: Option<u16>) -> PageResult {
        PageResult {
            page_number,
            output_path: PathBuf::from("/out/file.pdf"),
            page_count: 1,
            width_px: 595,
            height_px: 842,
            contains_images: false,
            size_kb: 12,
        }
    }

    fn page_info(page_number: u16) -> PageInfo {
        PageInfo {
            page_number,
            width_px: 595,
            height_px: 842,
            contains_images: true,
            image_count: 2,
            image_coverage: 0.5,
        }
    }

    fn text_file_result(page_number: Option<u16>) -> TextFileResult {
        TextFileResult {
            page_number,
            output_path: PathBuf::from("/out/file.txt"),
            page_count: 1,
            size_kb: 3,
        }
    }

    #[test]
    fn page_output_json_lists_pages_by_number() {
        let report = ExtractionReport {
            pages: vec![page_result(Some(1)), page_result(Some(3))],
            source_pages: Vec::new(),
            warnings: Vec::new(),
        };
        let value = page_output_json(
            &args(&["-f", "pdf"]),
            OutputFormat::Pdf { concat: false },
            &report,
        )
        .expect("json output");

        assert!(value.get("concat").is_none());
        assert_eq!(value["pages"]["1"]["size_kb"], 12);
        assert_eq!(value["pages"]["3"]["width_px"], 595);
    }

    #[test]
    fn page_output_json_includes_png_settings() {
        let report = ExtractionReport {
            pages: vec![page_result(Some(1))],
            source_pages: Vec::new(),
            warnings: Vec::new(),
        };
        let scale = page_output_json(
            &args(&["-f", "png"]),
            OutputFormat::Png {
                size: PngSize::Scale(1.5),
            },
            &report,
        )
        .expect("json output");
        assert_eq!(scale["scale"], 1.5);

        let width = page_output_json(
            &args(&["-f", "png"]),
            OutputFormat::Png {
                size: PngSize::WidthPx(640),
            },
            &report,
        )
        .expect("json output");
        assert_eq!(width["scale_width_px"], 640);
    }

    #[test]
    fn page_output_json_concat_reports_source_pages() {
        let report = ExtractionReport {
            pages: vec![page_result(None)],
            source_pages: vec![page_info(1), page_info(2)],
            warnings: Vec::new(),
        };
        let value = page_output_json(
            &args(&["-f", "pdf", "-c", "-p", "1-2"]),
            OutputFormat::Pdf { concat: true },
            &report,
        )
        .expect("json output");

        assert_eq!(value["concat"]["pages"], "1-2");
        assert_eq!(value["concat"]["size_kb"], 12);
        assert_eq!(value["page_info"]["2"]["image_count"], 2);
        assert!(value.get("pages").is_none());
    }

    #[test]
    fn page_output_json_concat_without_files_is_an_error() {
        let report = ExtractionReport {
            pages: Vec::new(),
            source_pages: Vec::new(),
            warnings: Vec::new(),
        };
        assert!(
            page_output_json(
                &args(&["-f", "pdf", "-c"]),
                OutputFormat::Pdf { concat: true },
                &report
            )
            .is_err()
        );
    }

    #[test]
    fn page_output_json_requires_page_numbers_per_page() {
        let report = ExtractionReport {
            pages: vec![page_result(None)],
            source_pages: Vec::new(),
            warnings: Vec::new(),
        };
        assert!(
            page_output_json(
                &args(&["-f", "pdf"]),
                OutputFormat::Pdf { concat: false },
                &report
            )
            .is_err()
        );
    }

    #[test]
    fn text_output_json_aggregate_uses_concat_key() {
        let report = TextExtractionReport {
            files: vec![text_file_result(None)],
            selected_page_count: 2,
            warnings: Vec::new(),
        };
        let value = text_output_json(&args(&["-e", "-p", "1-2"]), &report).expect("json output");
        assert_eq!(value["concat"]["pages"], "1-2");
        assert!(value.get("pages").is_none());
    }

    #[test]
    fn text_output_json_per_page_lists_files() {
        let report = TextExtractionReport {
            files: vec![text_file_result(Some(2))],
            selected_page_count: 1,
            warnings: Vec::new(),
        };
        let value = text_output_json(&args(&["-e", "-p", "2"]), &report).expect("json output");
        assert_eq!(value["pages"]["2"]["size_kb"], 3);
    }

    #[test]
    fn text_output_json_includes_warnings_as_errors() {
        let report = TextExtractionReport {
            files: Vec::new(),
            selected_page_count: 0,
            warnings: vec![ExtractionWarning::PageOutOfBounds {
                page_number: 99,
                total_pages: 3,
            }],
        };
        let value = text_output_json(&args(&["-e", "-p", "99"]), &report).expect("json output");
        let message = value["errors"]["99"].as_str().expect("error message");
        assert!(message.contains("out of bounds"));
    }

    #[test]
    fn structured_output_json_rounds_and_skips_empty_spans() {
        let report = StructuredExtractionReport {
            pages: vec![
                StructuredPage {
                    page: 1,
                    spans: vec![TextSpan {
                        text: "Hello".to_string(),
                        font_size: 12.3456,
                        font_name: "Helvetica".to_string(),
                        bold: true,
                        italic: false,
                        x: 10.44,
                        y: 700.06,
                        width: 33.33,
                        height: 12.01,
                    }],
                    markdown: "Hello".to_string(),
                },
                StructuredPage {
                    page: 2,
                    spans: Vec::new(),
                    markdown: "World".to_string(),
                },
            ],
            selected_page_count: 2,
            warnings: Vec::new(),
        };

        let value = structured_output_json(&report);
        assert_eq!(value["selected_page_count"], 2);
        let font_size = value["pages"][0]["spans"][0]["font_size"]
            .as_f64()
            .expect("font_size must be a number");
        assert!((font_size - 12.35).abs() < 0.001, "got {font_size}");
        let x = value["pages"][0]["spans"][0]["x"]
            .as_f64()
            .expect("x must be a number");
        assert!((x - 10.4).abs() < 0.001, "got {x}");
        assert_eq!(value["pages"][0]["spans"][0]["bold"], true);
        assert!(value["pages"][1].get("spans").is_none());
        assert_eq!(value["pages"][1]["markdown"], "World");
    }

    #[test]
    fn document_info_json_lists_page_metadata() {
        let info = DocumentInfo {
            page_count: 2,
            pages: vec![page_info(1), page_info(2)],
        };
        let value = document_info_json(&info);
        assert_eq!(value["page_count"], 2);
        assert_eq!(value["pages"]["1"]["image_count"], 2);
        assert_eq!(value["pages"]["2"]["contains_images"], true);
    }

    #[test]
    fn warning_page_extracts_page_number_from_all_variants() {
        let warnings = [
            ExtractionWarning::PageOutOfBounds {
                page_number: 1,
                total_pages: 1,
            },
            ExtractionWarning::LoadPage {
                page_number: 2,
                reason: "x".to_string(),
            },
            ExtractionWarning::UnreadableText { page_number: 3 },
            ExtractionWarning::SavePage {
                page_number: 4,
                reason: "y".to_string(),
            },
        ];
        let numbers: Vec<u16> = warnings.iter().map(warning_page).collect();
        assert_eq!(numbers, vec![1, 2, 3, 4]);
    }
}
