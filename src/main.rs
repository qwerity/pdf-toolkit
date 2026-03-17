use anyhow::{Context, Result, bail};
use clap::Parser;
use pdf_cutter::cli::{Args, CliOutputFormat};
use pdf_cutter::error::{PdfCutterError, Result as LibraryResult};
use pdf_cutter::{
    ExtractionReport, OutputFormat, TextExtractionReport, create_pdfium, document_page_count,
    extract_pages, extract_text,
};
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

fn main() -> ExitCode {
    match run_cli(Args::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run_cli(args: Args) -> Result<()> {
    ensure_input_file(&args.input)?;

    if args.doc_numbers {
        return print_document_page_count(&args.input);
    }

    if args.extract {
        return extract_requested_text(&args);
    }

    let start = Instant::now();
    let report = extract_requested_pages(&args)?;

    print_report(&report, start.elapsed())
}

fn print_document_page_count(path: &Path) -> Result<()> {
    let pdfium = create_pdfium().context("failed to initialize pdfium runtime")?;
    let page_count = document_page_count(&pdfium, path)
        .with_context(|| format!("failed to read page count from '{}'", path.display()))?;
    println!("{page_count}");
    Ok(())
}

fn extract_requested_text(args: &Args) -> Result<()> {
    validate_text_extraction_args(args)?;
    let start = Instant::now();
    let pdfium = create_pdfium().context("failed to initialize pdfium runtime")?;
    let report = extract_text(
        &pdfium,
        &args.input,
        args.pages.clone(),
        args.concat,
        args.output.as_deref(),
    )
    .with_context(|| format!("failed to extract text from '{}'", args.input.display()))?;

    print_text_report(&report, start.elapsed())
}

fn extract_requested_pages(args: &Args) -> Result<ExtractionReport> {
    let pdfium = create_pdfium().context("failed to initialize pdfium runtime")?;
    let output_format = resolve_output_format(args)?;

    extract_pages(
        &pdfium,
        &args.input,
        args.pages.clone(),
        output_format,
        args.output.as_deref(),
    )
    .with_context(|| format!("failed to extract pages from '{}'", args.input.display()))
}

fn validate_text_extraction_args(args: &Args) -> Result<()> {
    if matches!(args.format, CliOutputFormat::Png) {
        bail!("--extract cannot be used with --format png");
    }

    if args.scale.is_some() {
        bail!("--scale cannot be used with --extract");
    }

    Ok(())
}

fn resolve_output_format(args: &Args) -> Result<OutputFormat> {
    let output_format = match args.format {
        CliOutputFormat::Pdf => {
            if args.scale.is_some() {
                bail!("--scale can only be used with --format png");
            }

            OutputFormat::Pdf {
                concat: args.concat,
            }
        }
        CliOutputFormat::Png => {
            if args.concat {
                bail!("--concat can only be used with --format pdf");
            }

            OutputFormat::Png {
                scale: args
                    .scale
                    .context("--scale is required when --format png")?,
            }
        }
    };

    output_format.validate()?;
    Ok(output_format)
}

fn print_text_report(report: &TextExtractionReport, elapsed: Duration) -> Result<()> {
    for warning in &report.warnings {
        eprintln!("Warning: {warning}");
    }

    if report.files.is_empty() {
        bail!(
            "no text was extracted; check that the requested pages are within range and the output directory is writable"
        );
    }

    for file in &report.files {
        println!(
            "Extracted text -> {} ({} page(s), {} KB)",
            file.output_path.display(),
            file.page_count,
            file.size_kb,
        );
    }

    let extracted_page_count = report
        .files
        .iter()
        .map(|file| file.page_count)
        .sum::<usize>();

    println!(
        "Done - text extracted from {} page(s) into {} file(s) in {:.2?}",
        extracted_page_count,
        report.files.len(),
        elapsed,
    );

    Ok(())
}

fn print_report(report: &ExtractionReport, elapsed: Duration) -> Result<()> {
    for warning in &report.warnings {
        eprintln!("Warning: {warning}");
    }

    if report.pages.is_empty() {
        bail!(
            "no pages were extracted; check that the requested pages are within range and the output directory is writable"
        );
    }

    for page in &report.pages {
        if page.page_count == 1 {
            println!(
                "Extracted -> {} ({:.1}x{:.1} mm, {} KB)",
                page.output_path.display(),
                page.width_mm,
                page.height_mm,
                page.size_kb,
            );
        } else {
            println!(
                "Extracted -> {} ({} pages, {} KB)",
                page.output_path.display(),
                page.page_count,
                page.size_kb,
            );
        }
    }

    let extracted_page_count = report
        .pages
        .iter()
        .map(|page| page.page_count)
        .sum::<usize>();

    println!(
        "Done - {} page(s) extracted into {} file(s) in {:.2?}",
        extracted_page_count,
        report.pages.len(),
        elapsed,
    );

    Ok(())
}

fn ensure_input_file(path: &Path) -> LibraryResult<()> {
    if !path.exists() {
        return Err(PdfCutterError::InputMissing(path.to_path_buf()));
    }

    if !path.is_file() {
        return Err(PdfCutterError::InputNotFile(path.to_path_buf()));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_text_extraction_args;
    use clap::Parser;
    use clap::error::ErrorKind;
    use pdf_cutter::PageSelection;
    use pdf_cutter::cli::{Args, CliOutputFormat};

    #[test]
    fn parses_doc_numbers_short_flag() {
        let args = Args::try_parse_from(["pdf_cutter", "--input", "input.pdf", "-n"])
            .expect("short -n flag should parse");

        assert!(args.doc_numbers);
    }

    #[test]
    fn parses_extract_short_flag() {
        let args = Args::try_parse_from(["pdf_cutter", "--input", "input.pdf", "-e"])
            .expect("short -e flag should parse");

        assert!(args.extract);
    }

    #[test]
    fn rejects_extract_with_doc_numbers() {
        let error = Args::try_parse_from(["pdf_cutter", "--input", "input.pdf", "-e", "-n"])
            .expect_err("extract should conflict with doc numbers");

        assert_eq!(error.kind(), ErrorKind::ArgumentConflict);
    }

    #[test]
    fn rejects_png_output_for_text_extraction() {
        let args = Args {
            input: "input.pdf".into(),
            doc_numbers: false,
            extract: true,
            pages: PageSelection::All,
            format: CliOutputFormat::Png,
            concat: false,
            output: None,
            scale: None,
        };

        let error = validate_text_extraction_args(&args)
            .expect_err("png output should be rejected for text extraction");

        assert_eq!(
            error.to_string(),
            "--extract cannot be used with --format png"
        );
    }
}
