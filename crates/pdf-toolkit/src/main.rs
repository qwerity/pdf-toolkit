mod cli_output;

use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Parser;

use pdf_toolkit::cli::{
    Cli, Command, CutArgs, InspectArgs, RenderArgs, Source, StructuredArgs, TextArgs,
};
use pdf_toolkit::error::{PdfError, Result as LibraryResult};
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
    cut_pages(
        &args.source,
        args.output.as_deref(),
        args.output_format(),
        args.concat,
    )
}

fn render(args: &RenderArgs) -> Result<()> {
    // PNG output is one file per page, so there is never a merged result to
    // describe and the selection text is not echoed back.
    const ECHOES_A_MERGED_SELECTION: bool = false;

    cut_pages(
        &args.source,
        args.output.as_deref(),
        args.output_format(),
        ECHOES_A_MERGED_SELECTION,
    )
}

/// Runs the page-extraction engine and reports what it produced.
///
/// `cut` and `render` differ only in the format they ask for and in whether a
/// merged selection can exist to be echoed; everything between — resolving the
/// selection, naming outputs, collecting per-page warnings — is one path.
fn cut_pages(
    source: &Source,
    output: Option<&Path>,
    output_format: OutputFormat,
    echoes_merged_selection: bool,
) -> Result<()> {
    let pdfium = pdfium()?;
    let input = &source.input;
    let report = extract_pages(
        &pdfium,
        input,
        source.pages.selection().clone(),
        output_format,
        output,
    )
    .with_context(|| format!("failed to extract pages from '{}'", input.display()))?;

    print_page_output(
        source.pages.raw(),
        echoes_merged_selection,
        output_format,
        &report,
    )
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
        return Err(PdfError::InputMissing(path.to_path_buf()));
    }

    if !path.is_file() {
        return Err(PdfError::InputNotFile(path.to_path_buf()));
    }

    Ok(())
}
