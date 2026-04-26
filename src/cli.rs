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
