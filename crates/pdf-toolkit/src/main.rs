mod cli_output;

use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Parser;

use pdf_toolkit::cli::{Cli, Command, CutArgs, InspectArgs, RenderArgs, StructuredArgs, TextArgs};
use pdf_toolkit::error::{PdfCutterError, Result as LibraryResult};
use pdf_toolkit::{
    OutputFormat, Pdfium, create_pdfium, extract_pages, extract_structured_text, extract_text,
    inspect_document,
};

use crate::cli_output::{
    print_document_info, print_page_output, print_structured_output, print_text_output,
};

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    validate_input_file(cli.command.input())?;

    match cli.command {
        Command::Inspect(args) => inspect(&args),
        Command::Cut(args) => cut(&args),
        Command::Render(args) => render(&args),
        Command::Text(args) => text(&args),
        Command::Structured(args) => structured(&args),
    }
}

fn inspect(args: &InspectArgs) -> Result<()> {
    let pdfium = pdfium()?;
    let document_info = inspect_document(&pdfium, &args.input)
        .with_context(|| format!("failed to inspect '{}'", args.input.display()))?;

    print_document_info(&document_info)
}

fn cut(args: &CutArgs) -> Result<()> {
    let output_format = args.output_format();
    let report = extract(
        &args.source.input,
        args.source.pages.selection().clone(),
        output_format,
        args.output.as_deref(),
    )?;

    print_page_output(args.source.pages.raw(), args.concat, output_format, &report)
}

fn render(args: &RenderArgs) -> Result<()> {
    let output_format = args.output_format();
    let report = extract(
        &args.source.input,
        args.source.pages.selection().clone(),
        output_format,
        args.output.as_deref(),
    )?;

    // PNG output is one file per page, so there is never a merged result to
    // describe and the selection text is not echoed.
    print_page_output(args.source.pages.raw(), false, output_format, &report)
}

fn extract(
    input: &Path,
    selection: pdf_toolkit::PageSelection,
    output_format: OutputFormat,
    output: Option<&Path>,
) -> Result<pdf_toolkit::ExtractionReport> {
    let pdfium = pdfium()?;

    extract_pages(&pdfium, input, selection, output_format, output)
        .with_context(|| format!("failed to extract pages from '{}'", input.display()))
}

fn text(args: &TextArgs) -> Result<()> {
    let pdfium = pdfium()?;
    let report = extract_text(
        &pdfium,
        &args.source.input,
        args.source.pages.selection().clone(),
        args.concat,
        args.output.as_deref(),
    )
    .with_context(|| {
        format!(
            "failed to extract text from '{}'",
            args.source.input.display()
        )
    })?;

    print_text_output(args.source.pages.raw(), &report)
}

fn structured(args: &StructuredArgs) -> Result<()> {
    let pdfium = pdfium()?;
    let report = extract_structured_text(
        &pdfium,
        &args.source.input,
        args.source.pages.selection().clone(),
        args.structured_format.to_output_format(),
    )
    .with_context(|| {
        format!(
            "failed to extract structured text from '{}'",
            args.source.input.display()
        )
    })?;

    print_structured_output(&report)
}

fn pdfium() -> Result<Pdfium> {
    create_pdfium().context("failed to initialize pdfium runtime")
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
