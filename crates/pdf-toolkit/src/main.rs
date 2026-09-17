mod cli_output;
mod service;

use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{CommandFactory, FromArgMatches};

use pdf_toolkit::cli::{
    BuildingArgs, CadfixArgs, Cli, Command, CutArgs, DossierArgs, DownsampleArgs, InspectArgs,
    ModelArgs, RenderArgs, Source, StructuredArgs, TextArgs, VectorArgs,
};
use pdf_toolkit::error::{PdfError, Result as LibraryResult};
use pdf_toolkit::{
    OutputFormat, Pdfium, create_pdfium, extract_pages, extract_structured_text, extract_text,
    inspect_document,
};

use crate::cli_output::{
    print_building_output, print_cadfix_output, print_document_info, print_dossier_output,
    print_downsample_output, print_model_output, print_page_output, print_structured_output,
    print_text_output, print_vector_output,
};

fn main() -> ExitCode {
    match launch() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn launch() -> Result<()> {
    service::watch_parent()?;
    let matches = Cli::command().subcommand(service::command()).get_matches();
    if let Some(args) = matches.subcommand_matches("serve") {
        service::run(service::Config::from_arg_matches(args)?)
    } else {
        run(Cli::from_arg_matches(&matches)?)
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
        Command::Vector(args) => vector(&args),
        Command::Dossier(args) => dossier(&args),
        Command::Cadfix(args) => cadfix(&args),
        Command::Building(args) => building(&args),
        Command::Model(args) => model(&args),
        Command::Downsample(args) => downsample(&args),
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

/// Recovers CAD geometry, classifies each page, and exports GeoJSON.
///
/// The only command that does not touch PDFium: the geometry comes from a
/// content-stream interpreter, because a renderer cannot tell you what path a
/// drawing is made of.
fn vector(args: &VectorArgs) -> Result<()> {
    args.validate()
        .map_err(|message| anyhow::anyhow!(message))?;

    let options = pdf_vector::VectorOptions {
        references: args.georeference.clone(),
        ..vector_options(args.parcel.as_deref())?
    };
    let report = pdf_vector::extract_document(
        &args.source.input,
        &args.source.pages.page_numbers(),
        &options,
    )?;

    if let Some(dir) = args.output.as_deref() {
        write_geojson(dir, &report)?;
    }
    print_vector_output(&report)
}

/// Assembles the whole-dossier annotation, merging vision-pass answers when
/// they are supplied.
fn dossier(args: &DossierArgs) -> Result<()> {
    let answers = args
        .answers
        .as_deref()
        .map(pdf_dossier::Answers::load)
        .transpose()?;
    let mut options = pdf_dossier::DossierOptions {
        answers,
        output_dir: args.output.clone(),
    };
    let vector_options = vector_options(args.parcel.as_deref())?;

    let pdfium = pdfium()?;
    // The document is read once; a vision-pass merge re-assembles over the
    // same analysis rather than re-extracting and re-rendering every page.
    let analysis = pdf_dossier::analyze(&pdfium, &args.input, &vector_options)?;
    let mut report = pdf_dossier::assemble(&analysis, &options)?;

    // The live vision pass answers what the deterministic run flagged and the
    // answers file left open, then the report is re-assembled on the combined
    // answers — the same merge path a reviewed file takes.
    if let Some(model) = args.vlm.as_deref() {
        let client = pdf_vlm::VlmClient::from_env(model)?;
        let flagged = pdf_dossier::pages_awaiting_vision(&report, options.answers.as_ref());

        if !flagged.is_empty() {
            let (mut live, vlm_warnings) = client.annotate_pages(&pdfium, &args.input, &flagged)?;
            for warning in &vlm_warnings {
                eprintln!("Warning: {warning}");
            }
            pdf_dossier::seed_vision_reasons(&report, &mut live);
            let mut combined = options.answers.take().unwrap_or_default();
            combined.absorb(live);
            options.answers = Some(combined);
            report = pdf_dossier::assemble(&analysis, &options)?;
            report.llm_usage = Some(serde_json::to_value(client.calls())?);
            // The merged answers are what `building --answers` wants next, so
            // they are written beside the report's own files.
            if let Some(dir) = args.output.as_deref() {
                report.answers_file = Some(write_answers(
                    dir,
                    &report.document,
                    options.answers.as_ref().expect("just set"),
                )?);
            }
        }
    }

    print_dossier_output(&report)
}

/// Writes the merged vision-pass answers as `answers/<document>.json` under
/// the output directory and returns that relative path.
fn write_answers(dir: &Path, document: &str, answers: &pdf_dossier::Answers) -> Result<String> {
    let subdir = dir.join("answers");
    std::fs::create_dir_all(&subdir)
        .with_context(|| format!("cannot create '{}'", subdir.display()))?;
    let name = format!("answers/{document}.json");
    pdf_vector::write_pretty_json(&dir.join(&name), answers)?;
    Ok(name)
}

/// The vector options a `--parcel` file implies: the caller's parcel
/// polygons, or none.
fn vector_options(parcel: Option<&Path>) -> Result<pdf_vector::VectorOptions> {
    Ok(pdf_vector::VectorOptions {
        parcels: parcel
            .map(pdf_vector::read_parcel_file)
            .transpose()?
            .unwrap_or_default(),
        ..Default::default()
    })
}

fn cadfix(args: &CadfixArgs) -> Result<()> {
    let client = llm_providers::Client::from_env(&args.model)?;
    let pdfium = pdfium()?;
    let report = pdf_vlm::cad::fix_page(
        &client,
        &pdfium,
        &args.input,
        args.page,
        args.output.as_deref(),
    )?;
    print_cadfix_output(&report)
}

fn building(args: &BuildingArgs) -> Result<()> {
    let answers = args
        .answers
        .as_deref()
        .map(pdf_dossier::Answers::load)
        .transpose()?;
    let client = llm_providers::Client::from_env(&args.model)?;
    let pdfium = pdfium()?;
    let vector_options = vector_options(args.parcel.as_deref())?;
    let report = pdf_vlm::building::reconstruct(
        &client,
        &pdfium,
        &args.input,
        &vector_options,
        answers.as_ref(),
        args.output.as_deref(),
    )?;
    print_building_output(&report)
}

/// Reconstructs the building from its plan sheets, deterministically; a
/// hosted model is consulted for the parameters only when asked, and even
/// then its proposal is checked against the drawings before use.
fn model(args: &ModelArgs) -> Result<()> {
    args.validate()
        .map_err(|message| anyhow::anyhow!(message))?;
    let vector_options = vector_options(args.parcel.as_deref())?;
    let proposer = match args.llm_params.as_deref() {
        Some(model) => Some(pdf_vlm::params::Proposer::from_env(model, &args.input)?),
        None => None,
    };
    let callback = proposer.as_ref().map(|p| p.callback());
    let hints = match &args.answers {
        Some(path) => {
            let answers = pdf_dossier::Answers::load(path)?;
            pdf_model::SheetHints::from_labels(
                answers
                    .pages
                    .iter()
                    .map(|(page, answer)| (*page, answer.labels.as_slice())),
            )
        }
        None => pdf_model::SheetHints::default(),
    };
    let options = pdf_model::RunOptions {
        params_file: args.params.as_deref(),
        references: args.georeference.clone(),
        vector_options,
        output_dir: args.output.as_deref(),
        proposer: callback.as_ref().map(|f| f as pdf_model::Proposer<'_>),
        hints,
    };
    let mut report = pdf_model::run(&args.input, &options)?;
    if let Some(proposer) = &proposer {
        report.llm_usage = Some(serde_json::to_value(proposer.calls())?);
    }
    print_model_output(&report)
}

/// Resamples the document's raster images and writes the smaller PDF.
///
/// Like `vector`, it reads the file with `lopdf` rather than PDFium: PDFium
/// cannot save a document without regenerating the content streams it
/// touched, and leaving those untouched is the point.
fn downsample(args: &DownsampleArgs) -> Result<()> {
    args.validate()
        .map_err(|message| anyhow::anyhow!(message))?;
    let mut options = args.options();
    if let Some(budget) = service::downsample_budget()? {
        options.working_memory_bytes = budget;
    }
    let report = pdf_downsample::downsample(&args.input, &args.output, &options)?;
    print_downsample_output(&report)
}

/// Writes one `<document>_p<NN>.geojson` per page, beside the JSON on stdout.
fn write_geojson(dir: &Path, report: &pdf_vector::DocumentVector) -> Result<()> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("cannot create output directory '{}'", dir.display()))?;

    for page in &report.pages {
        let path = dir.join(pdf_vector::page_geojson_file_name(
            &report.document,
            page.page_number,
        ));
        pdf_vector::write_page_geojson(&path, &page.geojson)?;
    }
    Ok(())
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
