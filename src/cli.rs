use crate::{OutputFormat, PageSelection};
use clap::{Parser, ValueEnum};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// Path to the PDF file.
    #[arg(short, long)]
    pub input: PathBuf,

    /// Page numbers to extract, comma-separated (for example "1,2,5") or "all".
    #[arg(
        short = 'p',
        long,
        value_parser = parse_pages_arg,
        default_value = "all"
    )]
    pub pages: PageSelection,

    /// Output format for extracted pages.
    #[arg(short = 'f', long, value_enum, default_value_t = CliOutputFormat::Pdf)]
    pub format: CliOutputFormat,

    /// Output folder for extracted files. Defaults to the current directory.
    #[arg(short, long)]
    pub output: Option<PathBuf>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub enum CliOutputFormat {
    Pdf,
    Png,
}

impl From<CliOutputFormat> for OutputFormat {
    fn from(value: CliOutputFormat) -> Self {
        match value {
            CliOutputFormat::Pdf => OutputFormat::Pdf,
            CliOutputFormat::Png => OutputFormat::Png,
        }
    }
}

fn parse_pages_arg(value: &str) -> std::result::Result<PageSelection, String> {
    PageSelection::parse(value).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU16;

    fn nz(value: u16) -> NonZeroU16 {
        NonZeroU16::new(value).expect("test page numbers must be non-zero")
    }

    #[test]
    fn test_cli_defaults_to_pdf_format() {
        let args = Args::try_parse_from(["pdf_cutter", "--input", "test_assets/input.pdf"])
            .expect("CLI args should parse");

        assert_eq!(args.format, CliOutputFormat::Pdf);
    }

    #[test]
    fn test_cli_defaults_pages_to_all() {
        let args = Args::try_parse_from(["pdf_cutter", "--input", "test_assets/input.pdf"])
            .expect("CLI args should parse");

        assert_eq!(args.pages, PageSelection::All);
    }

    #[test]
    fn test_cli_accepts_png_format() {
        let args = Args::try_parse_from([
            "pdf_cutter",
            "--input",
            "test_assets/input.pdf",
            "--pages",
            "1",
            "--format",
            "png",
        ])
        .expect("CLI args should parse");

        assert_eq!(args.format, CliOutputFormat::Png);
    }

    #[test]
    fn test_cli_rejects_jpeg_format() {
        let error = Args::try_parse_from([
            "pdf_cutter",
            "--input",
            "test_assets/input.pdf",
            "--pages",
            "1",
            "--format",
            "jpeg",
        ])
        .expect_err("CLI args should reject unsupported formats");

        assert!(error.to_string().contains("jpeg"));
    }

    #[test]
    fn test_cli_parses_specific_pages_during_arg_parsing() {
        let args = Args::try_parse_from([
            "pdf_cutter",
            "--input",
            "test_assets/input.pdf",
            "--pages",
            "1,3",
        ])
        .expect("CLI args should parse");

        assert_eq!(args.pages, PageSelection::Specific(vec![nz(1), nz(3)]));
    }

    #[test]
    fn test_cli_rejects_invalid_page_numbers() {
        let error = Args::try_parse_from([
            "pdf_cutter",
            "--input",
            "test_assets/input.pdf",
            "--pages",
            "0",
        ])
        .expect_err("CLI args should be rejected");

        assert!(error.to_string().contains("positive integer"));
    }
}
