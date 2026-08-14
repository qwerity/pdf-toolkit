use crate::structured_text_extractor::StructuredOutputFormat;
use crate::{OutputFormat, PageSelection, PngSize};
use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use std::path::PathBuf;

const MODE_ERROR: &str = "choose either extraction or cutting: use --extract for text extraction, or use --format for page cutting";

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// Input PDF.
    #[arg(short, long)]
    pub input: PathBuf,

    /// Print page count and per-page metadata, then exit.
    #[arg(short = 'n', long = "page-count")]
    pub page_count: bool,

    /// Extract text into `.txt` file(s).
    #[arg(short = 'e', long, conflicts_with = "page_count")]
    pub extract: bool,

    /// Extract structured text as JSON with spans (font, position) and markdown.
    #[arg(long = "extract-structured", conflicts_with_all = ["page_count", "extract"])]
    pub extract_structured: bool,

    /// Structured extraction format: "full" (spans + markdown) or "markdown" (markdown only).
    #[arg(
        long = "structured-format",
        default_value = "full",
        requires = "extract_structured"
    )]
    pub structured_format: StructuredFormatArg,

    /// Pages to extract, like `1,2,5-7` or `all`.
    #[arg(
        short = 'p',
        long,
        visible_alias = "page",
        value_parser = parse_pages,
        default_value = "all"
    )]
    pub pages: PagesArg,

    /// Cutting output format.
    #[arg(short = 'f', long, value_enum)]
    pub format: Option<CliFormat>,

    /// Merge selected pages into one file.
    #[arg(short = 'c', long)]
    pub concat: bool,

    /// Output directory. Defaults to the current directory.
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// PNG scale. Defaults to `2.0`.
    #[arg(
        short = 's',
        long,
        value_parser = parse_scale,
        conflicts_with = "scale_width_px"
    )]
    pub scale: Option<f32>,

    /// Target PNG width in pixels. Preserves aspect ratio.
    #[arg(long = "scale-width-px", value_parser = parse_scale_width_px, conflicts_with = "scale")]
    pub scale_width_px: Option<u32>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub enum CliFormat {
    Pdf,
    Png,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub enum StructuredFormatArg {
    Full,
    Markdown,
}

impl StructuredFormatArg {
    pub fn to_output_format(self) -> StructuredOutputFormat {
        match self {
            Self::Full => StructuredOutputFormat::Full,
            Self::Markdown => StructuredOutputFormat::MarkdownOnly,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PagesArg {
    raw: String,
    parsed: PageSelection,
}

impl PagesArg {
    pub fn parse(value: &str) -> std::result::Result<Self, String> {
        Ok(Self {
            raw: value.to_string(),
            parsed: PageSelection::parse(value).map_err(|error| error.to_string())?,
        })
    }

    pub fn raw(&self) -> &str {
        &self.raw
    }

    pub fn selection(&self) -> &PageSelection {
        &self.parsed
    }
}

impl Args {
    /// Test-support constructor: parses `extra` flags on top of the required
    /// `--input input.pdf` seed. Hidden from the documented API; kept public
    /// so the binary crate's tests can share it.
    #[doc(hidden)]
    pub fn try_from_flags(extra: &[&str]) -> std::result::Result<Self, clap::Error> {
        let mut argv = vec!["pdf_cutter", "--input", "input.pdf"];
        argv.extend_from_slice(extra);
        Self::try_parse_from(argv)
    }

    pub fn validate_extract_args(&self) -> Result<()> {
        if self.format.is_some() {
            bail!(MODE_ERROR);
        }

        if self.scale.is_some() {
            bail!("--scale cannot be used with --extract");
        }

        if self.scale_width_px.is_some() {
            bail!("--scale-width-px cannot be used with --extract");
        }

        Ok(())
    }

    pub fn validate_extract_structured_args(&self) -> Result<()> {
        if self.format.is_some() {
            bail!(MODE_ERROR);
        }

        if self.scale.is_some() {
            bail!("--scale cannot be used with --extract-structured");
        }

        if self.scale_width_px.is_some() {
            bail!("--scale-width-px cannot be used with --extract-structured");
        }

        if self.concat {
            bail!("--concat cannot be used with --extract-structured");
        }

        Ok(())
    }

    pub fn output_format(&self) -> Result<OutputFormat> {
        if self.scale.is_some() && self.format.is_none() {
            bail!("--scale can only be used with --format png");
        }

        if self.scale_width_px.is_some() && self.format.is_none() {
            bail!("--scale-width-px can only be used with --format png");
        }

        let format = self.format.context(MODE_ERROR)?;

        let output_format = match format {
            CliFormat::Pdf => {
                if self.scale.is_some() {
                    bail!("--scale can only be used with --format png");
                }

                if self.scale_width_px.is_some() {
                    bail!("--scale-width-px can only be used with --format png");
                }

                OutputFormat::Pdf {
                    concat: self.concat,
                }
            }
            CliFormat::Png => {
                if self.concat {
                    bail!("--concat can only be used with --format pdf");
                }

                match (self.scale, self.scale_width_px) {
                    (Some(scale), None) => OutputFormat::Png {
                        size: PngSize::Scale(scale),
                    },
                    (None, Some(width_px)) => OutputFormat::Png {
                        size: PngSize::WidthPx(width_px),
                    },
                    (None, None) => OutputFormat::Png {
                        size: PngSize::Scale(2.0),
                    },
                    (Some(_), Some(_)) => {
                        bail!("choose either --scale or --scale-width-px with --format png")
                    }
                }
            }
        };

        output_format.validate()?;
        Ok(output_format)
    }
}

fn parse_pages(value: &str) -> std::result::Result<PagesArg, String> {
    PagesArg::parse(value)
}

fn parse_scale(value: &str) -> std::result::Result<f32, String> {
    let scale = value.parse::<f32>().map_err(|error| error.to_string())?;

    if scale.is_finite() && scale > 0.0 {
        Ok(scale)
    } else {
        Err(format!(
            "image scale must be a finite number greater than zero, got {scale}"
        ))
    }
}

fn parse_scale_width_px(value: &str) -> std::result::Result<u32, String> {
    let width_px = value.parse::<u32>().map_err(|error| error.to_string())?;

    if width_px > 0 {
        Ok(width_px)
    } else {
        Err("image width must be an integer greater than zero".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_args(extra: &[&str]) -> Result<Args, clap::Error> {
        Args::try_from_flags(extra)
    }

    fn args(extra: &[&str]) -> Args {
        parse_args(extra).expect("arguments must parse")
    }

    #[test]
    fn defaults_select_all_pages_without_mode() {
        let args = args(&[]);
        assert_eq!(args.pages.raw(), "all");
        assert_eq!(args.pages.selection(), &PageSelection::All);
        assert!(args.format.is_none());
        assert!(!args.concat);
        assert!(!args.extract);
        assert!(!args.extract_structured);
    }

    #[test]
    fn pages_argument_parses_selection_and_keeps_raw() {
        let args = args(&["-p", "2-3"]);
        assert_eq!(args.pages.raw(), "2-3");
        assert!(
            matches!(args.pages.selection(), PageSelection::Specific(pages) if pages.len() == 2)
        );
    }

    #[test]
    fn invalid_pages_argument_is_rejected() {
        assert!(parse_args(&["-p", "0"]).is_err());
        assert!(parse_args(&["-p", "5-3"]).is_err());
        assert!(parse_args(&["-p", ""]).is_err());
    }

    #[test]
    fn extraction_modes_conflict_with_each_other() {
        assert!(parse_args(&["--extract", "--page-count"]).is_err());
        assert!(parse_args(&["--extract-structured", "--extract"]).is_err());
        assert!(parse_args(&["--extract-structured", "--page-count"]).is_err());
    }

    #[test]
    fn structured_format_requires_structured_mode() {
        assert!(parse_args(&["--structured-format", "markdown"]).is_err());
        let args = args(&["--extract-structured", "--structured-format", "markdown"]);
        assert_eq!(args.structured_format, StructuredFormatArg::Markdown);
    }

    #[test]
    fn structured_format_maps_to_output_format() {
        assert_eq!(
            StructuredFormatArg::Full.to_output_format(),
            StructuredOutputFormat::Full
        );
        assert_eq!(
            StructuredFormatArg::Markdown.to_output_format(),
            StructuredOutputFormat::MarkdownOnly
        );
    }

    #[test]
    fn scale_flags_conflict() {
        assert!(parse_args(&["-f", "png", "-s", "2", "--scale-width-px", "100"]).is_err());
    }

    #[test]
    fn scale_must_be_positive_and_finite() {
        assert!(parse_args(&["-s", "0"]).is_err());
        assert!(parse_args(&["-s", "-1"]).is_err());
        assert!(parse_args(&["-s", "nan"]).is_err());
        assert!(parse_args(&["-s", "inf"]).is_err());
        assert!(parse_args(&["-s", "abc"]).is_err());
    }

    #[test]
    fn scale_width_must_be_positive_integer() {
        assert!(parse_args(&["--scale-width-px", "0"]).is_err());
        assert!(parse_args(&["--scale-width-px", "1.5"]).is_err());
    }

    #[test]
    fn validate_extract_args_rejects_cutting_flags() {
        assert!(args(&["-e", "-f", "pdf"]).validate_extract_args().is_err());
        assert!(args(&["-e", "-s", "2"]).validate_extract_args().is_err());
        assert!(
            args(&["-e", "--scale-width-px", "100"])
                .validate_extract_args()
                .is_err()
        );
        assert!(args(&["-e"]).validate_extract_args().is_ok());
        assert!(args(&["-e", "-c"]).validate_extract_args().is_ok());
    }

    #[test]
    fn validate_extract_structured_args_rejects_cutting_and_concat() {
        let base = "--extract-structured";
        assert!(
            args(&[base, "-f", "pdf"])
                .validate_extract_structured_args()
                .is_err()
        );
        assert!(
            args(&[base, "-s", "2"])
                .validate_extract_structured_args()
                .is_err()
        );
        assert!(
            args(&[base, "--scale-width-px", "10"])
                .validate_extract_structured_args()
                .is_err()
        );
        assert!(
            args(&[base, "-c"])
                .validate_extract_structured_args()
                .is_err()
        );
        assert!(args(&[base]).validate_extract_structured_args().is_ok());
    }

    #[test]
    fn output_format_requires_format_flag() {
        assert!(args(&[]).output_format().is_err());
        assert!(args(&["-s", "2"]).output_format().is_err());
        assert!(args(&["--scale-width-px", "10"]).output_format().is_err());
    }

    #[test]
    fn output_format_pdf_rejects_png_flags() {
        assert!(args(&["-f", "pdf", "-s", "2"]).output_format().is_err());
        assert!(
            args(&["-f", "pdf", "--scale-width-px", "10"])
                .output_format()
                .is_err()
        );
    }

    #[test]
    fn output_format_pdf_carries_concat() {
        assert_eq!(
            args(&["-f", "pdf", "-c"]).output_format().unwrap(),
            OutputFormat::Pdf { concat: true }
        );
        assert_eq!(
            args(&["-f", "pdf"]).output_format().unwrap(),
            OutputFormat::Pdf { concat: false }
        );
    }

    #[test]
    fn output_format_png_defaults_to_scale_two() {
        assert_eq!(
            args(&["-f", "png"]).output_format().unwrap(),
            OutputFormat::Png {
                size: PngSize::Scale(2.0)
            }
        );
    }

    #[test]
    fn output_format_png_uses_explicit_sizes() {
        assert_eq!(
            args(&["-f", "png", "-s", "1.5"]).output_format().unwrap(),
            OutputFormat::Png {
                size: PngSize::Scale(1.5)
            }
        );
        assert_eq!(
            args(&["-f", "png", "--scale-width-px", "640"])
                .output_format()
                .unwrap(),
            OutputFormat::Png {
                size: PngSize::WidthPx(640)
            }
        );
    }

    #[test]
    fn output_format_png_rejects_concat() {
        assert!(args(&["-f", "png", "-c"]).output_format().is_err());
    }
}
