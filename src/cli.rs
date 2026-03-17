use crate::PageSelection;
use clap::{Parser, ValueEnum};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// Path to the PDF file.
    #[arg(short, long)]
    pub input: PathBuf,

    /// Print the total number of pages in the input PDF and exit.
    #[arg(short = 'n', long = "doc-numbers")]
    pub doc_numbers: bool,

    /// Extract text into .txt file(s) in the output directory.
    #[arg(short = 'e', long, conflicts_with = "doc_numbers")]
    pub extract: bool,

    /// Page numbers to extract, comma-separated (for example "1,2,5-7") or "all".
    #[arg(
        short = 'p',
        long,
        visible_alias = "page",
        value_parser = parse_pages_arg,
        default_value = "all"
    )]
    pub pages: PageSelection,

    /// Output format for extracted pages.
    #[arg(short = 'f', long, value_enum, default_value_t = CliOutputFormat::Pdf)]
    pub format: CliOutputFormat,

    /// Merge selected pages into a single PDF, or one text file with --extract.
    #[arg(short = 'c', long)]
    pub concat: bool,

    /// Output folder for extracted files. Defaults to the current directory.
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// Scale factor used when rendering PNG output.
    #[arg(short = 's', long, value_parser = parse_scale_arg)]
    pub scale: Option<f32>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub enum CliOutputFormat {
    Pdf,
    Png,
}

fn parse_pages_arg(value: &str) -> std::result::Result<PageSelection, String> {
    PageSelection::parse(value).map_err(|error| error.to_string())
}

fn parse_scale_arg(value: &str) -> std::result::Result<f32, String> {
    let scale = value.parse::<f32>().map_err(|error| error.to_string())?;

    if scale.is_finite() && scale > 0.0 {
        Ok(scale)
    } else {
        Err(format!(
            "image scale must be a finite number greater than zero, got {scale}"
        ))
    }
}
