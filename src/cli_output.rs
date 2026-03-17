use anyhow::{Context, Result, bail};
use pdf_cutter::cli::Args;
use pdf_cutter::{ExtractionReport, ExtractionWarning, OutputFormat, TextExtractionReport};
use serde_json::{Map, Value, json};

pub fn print_page_count(page_count: u16) -> Result<()> {
    print_json(json!({
        "page_count": page_count
    }))
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
    add_png_scale(&mut output, output_format);

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

fn is_aggregate_text_output(report: &TextExtractionReport) -> bool {
    report
        .files
        .first()
        .is_some_and(|file| file.page_number.is_none())
}

fn add_png_scale(output: &mut Map<String, Value>, output_format: OutputFormat) {
    if let OutputFormat::Png { scale } = output_format {
        output.insert("scale".to_string(), json!(scale));
    }
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
