use crate::PageSelection;
use clap::{Parser, ValueEnum};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// Path to the PDF file.
    #[arg(short, long)]
    pub input: PathBuf,

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

    /// Output folder for extracted files. Defaults to the current directory.
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// Scale factor used when rendering PNG output.
    #[arg(
        short = 's',
        long,
        value_parser = parse_scale_arg,
        required_if_eq("format", "png")
    )]
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
    fn test_cli_omits_scale_by_default() {
        let args = Args::try_parse_from(["pdf_cutter", "--input", "test_assets/input.pdf"])
            .expect("CLI args should parse");

        assert_eq!(args.scale, None);
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
            "--scale",
            "2",
        ])
        .expect("CLI args should parse");

        assert_eq!(args.format, CliOutputFormat::Png);
        assert_eq!(args.scale, Some(2.0));
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
    fn test_cli_accepts_short_scale_flag() {
        let args = Args::try_parse_from([
            "pdf_cutter",
            "--input",
            "test_assets/input.pdf",
            "-s",
            "3.5",
        ])
        .expect("CLI args should parse");

        assert_eq!(args.scale, Some(3.5));
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
    fn test_cli_parses_page_ranges_during_arg_parsing() {
        let args = Args::try_parse_from([
            "pdf_cutter",
            "--input",
            "test_assets/input.pdf",
            "--pages",
            "1-3,5-6,9",
        ])
        .expect("CLI args should parse");

        assert_eq!(
            args.pages,
            PageSelection::Specific(vec![nz(1), nz(2), nz(3), nz(5), nz(6), nz(9)])
        );
    }

    #[test]
    fn test_cli_accepts_page_alias() {
        let args = Args::try_parse_from([
            "pdf_cutter",
            "--input",
            "test_assets/input.pdf",
            "--page",
            "1-2",
        ])
        .expect("CLI args should parse");

        assert_eq!(args.pages, PageSelection::Specific(vec![nz(1), nz(2)]));
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

    #[test]
    fn test_cli_requires_scale_for_png() {
        let error = Args::try_parse_from([
            "pdf_cutter",
            "--input",
            "test_assets/input.pdf",
            "--format",
            "png",
        ])
        .expect_err("png output without a scale should be rejected");

        assert!(error.to_string().contains("--scale"));
    }

    #[test]
    fn test_cli_rejects_non_positive_scale() {
        let error = Args::try_parse_from([
            "pdf_cutter",
            "--input",
            "test_assets/input.pdf",
            "--format",
            "png",
            "--scale",
            "0",
        ])
        .expect_err("non-positive scale should be rejected");

        assert!(error.to_string().contains("greater than zero"));
    }
}
