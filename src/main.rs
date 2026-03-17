mod cli_output;

use crate::cli_output::{print_page_count, print_page_output, print_text_output};
use anyhow::{Context, Result};
use clap::Parser;
use pdf_cutter::cli::Args;
use pdf_cutter::error::{PdfCutterError, Result as LibraryResult};
use pdf_cutter::{
    ExtractionReport, OutputFormat, create_pdfium, document_page_count, extract_pages, extract_text,
};
use std::path::Path;
use std::process::ExitCode;

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
    validate_input_file(&args.input)?;

    if args.page_count {
        return show_page_count(&args.input);
    }

    if args.extract {
        return run_text_extraction(&args);
    }

    let output_format = args.output_format()?;
    let report = run_page_extraction(&args, output_format)?;

    print_page_output(&args, output_format, &report)
}

fn show_page_count(path: &Path) -> Result<()> {
    let pdfium = create_pdfium().context("failed to initialize pdfium runtime")?;
    let page_count = document_page_count(&pdfium, path)
        .with_context(|| format!("failed to read page count from '{}'", path.display()))?;

    print_page_count(page_count)
}

fn run_text_extraction(args: &Args) -> Result<()> {
    args.validate_extract_args()?;
    let pdfium = create_pdfium().context("failed to initialize pdfium runtime")?;
    let report = extract_text(
        &pdfium,
        &args.input,
        args.pages.selection().clone(),
        args.concat,
        args.output.as_deref(),
    )
    .with_context(|| format!("failed to extract text from '{}'", args.input.display()))?;

    print_text_output(args, &report)
}

fn run_page_extraction(args: &Args, output_format: OutputFormat) -> Result<ExtractionReport> {
    let pdfium = create_pdfium().context("failed to initialize pdfium runtime")?;

    extract_pages(
        &pdfium,
        &args.input,
        args.pages.selection().clone(),
        output_format,
        args.output.as_deref(),
    )
    .with_context(|| format!("failed to extract pages from '{}'", args.input.display()))
}

fn validate_input_file(path: &Path) -> LibraryResult<()> {
    if !path.exists() {
        return Err(PdfCutterError::InputMissing(path.to_path_buf()));
    }

    if !path.is_file() {
        return Err(PdfCutterError::InputNotFile(path.to_path_buf()));
    }

    Ok(())
}
