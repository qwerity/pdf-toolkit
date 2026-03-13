use anyhow::{Context, Result, bail};
use clap::Parser;
use pdf_cutter::cli::{Args, CliOutputFormat};
use pdf_cutter::error::{PdfCutterError, Result as LibraryResult};
use pdf_cutter::{ExtractionReport, OutputFormat, create_pdfium, extract_pages};
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
    let start = Instant::now();
    let report = extract_requested_pages(&args)?;

    print_report(&report, start.elapsed())
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
