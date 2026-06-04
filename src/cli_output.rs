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
