//! Command-line surface.
//!
//! One subcommand per operation, which is what lets clap reject a nonsensical
//! request instead of the program checking for one. The flag form this
//! replaced had four modes selected by `--page-count` / `--extract` /
//! `--extract-structured` / `--format`, every pair of which had to be declared
//! mutually exclusive, and options that were meaningful in one mode and an
//! error in another: `--concat` applied to PDF but not PNG, `--scale` to PNG
//! but not PDF, and neither to text. Those combinations are now unspeakable —
//! `cut` has no `--scale` to reject — so the validation they needed is gone
//! rather than reimplemented.
//!
//! Each tool is a subcommand, so adding one is adding a
//! variant to [`Command`] rather than reconciling two ways of being invoked.

use std::path::PathBuf;

use clap::{Args as ClapArgs, Parser, Subcommand, ValueEnum};

use pdf_core::PageSelection;
use pdf_geo::local::Reference;
use pdf_pages::OutputFormat;
use pdf_render::PngSize;
use pdf_text::StructuredOutputFormat;

#[derive(Parser, Debug)]
#[command(
    name = "pdf-toolkit",
    about = "Inspect PDFs, cut pages to PDF or PNG, extract text, and recover CAD geometry as GeoJSON.",
    version,
    author
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Report the page count and per-page metadata, including whether a page
    /// needs a vision-model pass.
    Inspect(InspectArgs),
    /// Cut selected pages into single-page PDFs, or one merged PDF.
    Cut(CutArgs),
    /// Render selected pages as PNG images.
    Render(RenderArgs),
    /// Extract plain text into `.txt` files.
    Text(TextArgs),
    /// Extract positioned text spans and a markdown reconstruction, to stdout.
    Structured(StructuredArgs),
    /// Recover CAD vector geometry, classify each page, and export GeoJSON.
    Vector(VectorArgs),
    /// Assemble the whole-dossier annotation: labels, routing, QR payloads,
    /// entities, groups and a document graph, with VLM work orders for what
    /// deterministic extraction cannot read.
    Dossier(DossierArgs),
}

/// The source document and the pages wanted from it.
#[derive(ClapArgs, Debug)]
pub struct Source {
    /// Source PDF.
    pub input: PathBuf,

    /// Pages to act on, like `1,2,5-7` or `all`.
    #[arg(
        short = 'p',
        long,
        visible_alias = "page",
        value_parser = parse_pages,
        default_value = "all"
    )]
    pub pages: PagesArg,
}

#[derive(ClapArgs, Debug)]
pub struct InspectArgs {
    /// Source PDF.
    pub input: PathBuf,
}

#[derive(ClapArgs, Debug)]
pub struct CutArgs {
    #[command(flatten)]
    pub source: Source,

    /// Output directory. Defaults to the current directory.
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// Merge the selected pages into one PDF.
    #[arg(short, long)]
    pub concat: bool,
}

#[derive(ClapArgs, Debug)]
pub struct RenderArgs {
    #[command(flatten)]
    pub source: Source,

    /// Output directory. Defaults to the current directory.
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// Scale factor applied to the page's own size. Defaults to `2.0` when
    /// no sizing option is given.
    #[arg(short, long, value_parser = parse_scale, conflicts_with = "scale_width_px")]
    pub scale: Option<f32>,

    /// Target width in pixels. Preserves the aspect ratio.
    #[arg(long = "scale-width-px", value_parser = parse_scale_width_px)]
    pub scale_width_px: Option<u32>,
}

#[derive(ClapArgs, Debug)]
pub struct TextArgs {
    #[command(flatten)]
    pub source: Source,

    /// Output directory. Defaults to the current directory.
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// Write the selected pages into one file.
    #[arg(short, long)]
    pub concat: bool,
}

#[derive(ClapArgs, Debug)]
pub struct StructuredArgs {
    #[command(flatten)]
    pub source: Source,

    /// `full` includes spans and markdown; `markdown` emits markdown only.
    #[arg(long = "structured-format", default_value = "full")]
    pub structured_format: StructuredFormatArg,
}

#[derive(ClapArgs, Debug)]
pub struct DossierArgs {
    /// Source PDF.
    pub input: PathBuf,

    /// Vision-pass answers to merge (`docs/annotation-schema.md`). Without
    /// them the annotation carries VLM work orders instead of the answered
    /// semantic layer.
    #[arg(long)]
    pub answers: Option<PathBuf>,

    /// Annotate the flagged pages with a hosted vision model, in-process.
    ///
    /// Bare `--vlm` uses the corpus benchmark's winner
    /// (`gemini-3.8-flash`); pass a spec to pick another model. The spec's
    /// shape picks the provider: `gemini-*` uses the Gemini API
    /// (`GEMINI_API_KEY`), a `vendor/model` id like
    /// `qwen/qwen2.5-vl-72b-instruct` uses OpenRouter
    /// (`OPENROUTER_API_KEY`). **This sends page images to that provider** —
    /// the only networked path in the toolkit, and only ever opt-in. Pages
    /// covered by `--answers` are not re-asked.
    #[arg(
        long,
        value_name = "MODEL",
        num_args = 0..=1,
        default_missing_value = pdf_vlm::DEFAULT_MODEL
    )]
    pub vlm: Option<String>,

    /// Also write each drawing page's GeoJSON under
    /// `<dir>/geojson/<document>/`, the layout the annotation's `cad.geojson`
    /// paths reference. The same annotation JSON is on stdout either way.
    #[arg(short, long)]
    pub output: Option<PathBuf>,
}

#[derive(ClapArgs, Debug)]
pub struct VectorArgs {
    #[command(flatten)]
    pub source: Source,

    /// A real-world reference, as `local_x,local_y,longitude,latitude`.
    ///
    /// Repeat it. Two or more place a drawing that carries no coordinate table
    /// of its own: the local pair is a point in the drawing's own frame — the
    /// coordinates an unplaced page's GeoJSON is written in — and the second
    /// pair is where that point actually is.
    ///
    /// Pages that can be placed from their own printed table ignore these; the
    /// sheet's own figures are checkable against the drawing and a supplied
    /// pair is not.
    ///
    /// `allow_hyphen_values` because a local coordinate is very often negative
    /// — the frame's origin is the drawing's centre — and without it a leading
    /// minus makes clap read the value as an unknown flag.
    #[arg(
        long = "georeference",
        short = 'g',
        value_parser = parse_reference_arg,
        allow_hyphen_values = true
    )]
    pub georeference: Vec<Reference>,

    /// Also write each page's GeoJSON to this directory. The same GeoJSON is
    /// in the JSON on stdout either way.
    #[arg(short, long)]
    pub output: Option<PathBuf>,
}

impl VectorArgs {
    /// Rejects a single reference up front.
    ///
    /// One reference fixes position but leaves rotation and scale free, so a
    /// placement built from it would be confidently wrong rather than refused.
    /// The check lives here so the message names the flag the user typed.
    pub fn validate(&self) -> Result<(), String> {
        if self.georeference.len() == 1 {
            return Err(
                "--georeference needs at least two references: one fixes position but leaves \
                 rotation and scale undetermined"
                    .to_string(),
            );
        }
        Ok(())
    }
}

impl Command {
    /// The source PDF, whichever command was asked for. Used by the one
    /// check that applies to all of them: that the input exists and is a file.
    pub fn input(&self) -> &PathBuf {
        match self {
            Self::Inspect(args) => &args.input,
            Self::Cut(args) => &args.source.input,
            Self::Render(args) => &args.source.input,
            Self::Text(args) => &args.source.input,
            Self::Structured(args) => &args.source.input,
            Self::Vector(args) => &args.source.input,
            Self::Dossier(args) => &args.input,
        }
    }
}

impl CutArgs {
    pub fn output_format(&self) -> OutputFormat {
        OutputFormat::Pdf {
            concat: self.concat,
        }
    }
}

impl RenderArgs {
    /// The size to render at.
    ///
    /// `--scale` and `--scale-width-px` are declared as conflicting, so at
    /// most one is present; neither means the default scale.
    pub fn png_size(&self) -> PngSize {
        match (self.scale, self.scale_width_px) {
            (Some(scale), _) => PngSize::Scale(scale),
            (None, Some(width_px)) => PngSize::WidthPx(width_px),
            (None, None) => PngSize::Scale(DEFAULT_PNG_SCALE),
        }
    }

    pub fn output_format(&self) -> OutputFormat {
        OutputFormat::Png {
            size: self.png_size(),
        }
    }
}

/// Applied when `render` is given neither sizing option.
const DEFAULT_PNG_SCALE: f32 = 2.0;

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

/// A page selection, kept alongside the text it was written as.
///
/// The raw text is echoed back in the `concat` result, where it describes what
/// the merged file contains; rendering the parsed selection instead would
/// report `1-3` as `1,2,3` and no longer match what was asked for.
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

    /// The selection as one-based page numbers, or empty for `all`.
    ///
    /// `vector` reads pages through a different parser from the rest of the
    /// toolkit, so it resolves the selection against that document's own page
    /// count rather than through `PageSelection::resolve`.
    pub fn page_numbers(&self) -> Vec<usize> {
        match &self.parsed {
            PageSelection::All => Vec::new(),
            PageSelection::Specific(pages) => pages.iter().map(|p| usize::from(p.get())).collect(),
        }
    }
}

impl Cli {
    /// Test-support constructor. Hidden from the documented API; kept public
    /// so the binary crate's tests can share it.
    #[doc(hidden)]
    pub fn try_from_args(args: &[&str]) -> std::result::Result<Self, clap::Error> {
        let mut argv = vec!["pdf-toolkit"];
        argv.extend_from_slice(args);
        Self::try_parse_from(argv)
    }
}

fn parse_pages(value: &str) -> std::result::Result<PagesArg, String> {
    PagesArg::parse(value)
}

fn parse_reference_arg(value: &str) -> std::result::Result<Reference, String> {
    pdf_vector::parse_reference(value)
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

    fn parse(args: &[&str]) -> Cli {
        Cli::try_from_args(args).expect("command line must parse")
    }

    fn rejects(args: &[&str]) {
        assert!(
            Cli::try_from_args(args).is_err(),
            "expected {args:?} to be rejected"
        );
    }

    #[test]
    fn every_command_names_its_input() {
        let cases = [
            vec!["inspect", "a.pdf"],
            vec!["cut", "a.pdf"],
            vec!["render", "a.pdf"],
            vec!["text", "a.pdf"],
            vec!["structured", "a.pdf"],
            vec!["dossier", "a.pdf"],
        ];

        for args in cases {
            assert_eq!(
                parse(&args).command.input(),
                &PathBuf::from("a.pdf"),
                "for {args:?}"
            );
        }
    }

    #[test]
    fn a_command_is_required() {
        rejects(&[]);
        rejects(&["a.pdf"]);
    }

    #[test]
    fn an_input_is_required() {
        for command in ["inspect", "cut", "render", "text", "structured", "dossier"] {
            rejects(&[command]);
        }
    }

    #[test]
    fn pages_default_to_all_and_accept_lists_and_ranges() {
        let Command::Cut(args) = parse(&["cut", "a.pdf"]).command else {
            panic!("expected cut");
        };
        assert_eq!(args.source.pages.raw(), "all");

        let Command::Cut(args) = parse(&["cut", "a.pdf", "--pages", "1-3,7"]).command else {
            panic!("expected cut");
        };
        assert_eq!(args.source.pages.raw(), "1-3,7");
    }

    #[test]
    fn page_is_an_alias_for_pages() {
        let Command::Text(args) = parse(&["text", "a.pdf", "--page", "2"]).command else {
            panic!("expected text");
        };
        assert_eq!(args.source.pages.raw(), "2");
    }

    #[test]
    fn an_unparseable_selection_is_rejected_at_parse_time() {
        rejects(&["cut", "a.pdf", "--pages", "3-1"]);
        rejects(&["cut", "a.pdf", "--pages", "abc"]);
        rejects(&["cut", "a.pdf", "--pages", ""]);
    }

    #[test]
    fn cut_produces_pdf_output_and_honours_concat() {
        let Command::Cut(args) = parse(&["cut", "a.pdf"]).command else {
            panic!("expected cut");
        };
        assert_eq!(args.output_format(), OutputFormat::Pdf { concat: false });

        let Command::Cut(args) = parse(&["cut", "a.pdf", "--concat"]).command else {
            panic!("expected cut");
        };
        assert_eq!(args.output_format(), OutputFormat::Pdf { concat: true });
    }

    #[test]
    fn render_defaults_to_scale_two() {
        let Command::Render(args) = parse(&["render", "a.pdf"]).command else {
            panic!("expected render");
        };
        assert_eq!(args.png_size(), PngSize::Scale(2.0));
    }

    #[test]
    fn render_takes_either_a_scale_or_a_width() {
        let Command::Render(args) = parse(&["render", "a.pdf", "--scale", "1.5"]).command else {
            panic!("expected render");
        };
        assert_eq!(args.png_size(), PngSize::Scale(1.5));

        let Command::Render(args) = parse(&["render", "a.pdf", "--scale-width-px", "800"]).command
        else {
            panic!("expected render");
        };
        assert_eq!(args.png_size(), PngSize::WidthPx(800));
    }

    #[test]
    fn render_refuses_both_sizing_options_at_once() {
        rejects(&["render", "a.pdf", "--scale", "2", "--scale-width-px", "800"]);
    }

    #[test]
    fn render_refuses_a_size_that_cannot_be_drawn() {
        for scale in ["0", "-1", "nan", "inf"] {
            rejects(&["render", "a.pdf", "--scale", scale]);
        }
        rejects(&["render", "a.pdf", "--scale-width-px", "0"]);
    }

    /// The combinations the flag-based surface had to reject at runtime are
    /// now unspeakable: the option does not exist on the command it was
    /// meaningless for.
    #[test]
    fn options_do_not_exist_on_commands_they_never_applied_to() {
        rejects(&["cut", "a.pdf", "--scale", "2"]);
        rejects(&["cut", "a.pdf", "--scale-width-px", "800"]);
        rejects(&["render", "a.pdf", "--concat"]);
        rejects(&["text", "a.pdf", "--scale", "2"]);
        rejects(&["structured", "a.pdf", "--concat"]);
        rejects(&["structured", "a.pdf", "--output", "out"]);
        rejects(&["inspect", "a.pdf", "--pages", "1"]);
        rejects(&["cut", "a.pdf", "--structured-format", "markdown"]);
        rejects(&["text", "a.pdf", "--structured-format", "markdown"]);
        rejects(&["dossier", "a.pdf", "--pages", "1"]);
        rejects(&["dossier", "a.pdf", "--concat"]);
        rejects(&["vector", "a.pdf", "--answers", "x.json"]);
        rejects(&["vector", "a.pdf", "--vlm", "gemini-3.8-flash"]);
    }

    #[test]
    fn dossier_takes_optional_answers_and_output() {
        let Command::Dossier(args) = parse(&["dossier", "a.pdf"]).command else {
            panic!("expected dossier");
        };
        assert!(args.answers.is_none() && args.output.is_none());

        let Command::Dossier(args) =
            parse(&["dossier", "a.pdf", "--answers", "vlm.json", "-o", "out"]).command
        else {
            panic!("expected dossier");
        };
        assert_eq!(
            args.answers.as_deref(),
            Some(std::path::Path::new("vlm.json"))
        );
        assert_eq!(args.output.as_deref(), Some(std::path::Path::new("out")));

        let Command::Dossier(args) =
            parse(&["dossier", "a.pdf", "--vlm", "qwen/qwen2.5-vl-72b-instruct"]).command
        else {
            panic!("expected dossier");
        };
        assert_eq!(args.vlm.as_deref(), Some("qwen/qwen2.5-vl-72b-instruct"));
    }

    /// `--vlm` without a value means "the benchmark's winner", so a caller
    /// opts into the vision pass without having to know model ids.
    #[test]
    fn bare_vlm_defaults_to_the_benchmark_winner() {
        let Command::Dossier(args) = parse(&["dossier", "a.pdf", "--vlm"]).command else {
            panic!("expected dossier");
        };
        assert_eq!(args.vlm.as_deref(), Some(pdf_vlm::DEFAULT_MODEL));
        assert_eq!(args.vlm.as_deref(), Some("gemini-3.8-flash"));

        let Command::Dossier(args) = parse(&["dossier", "a.pdf"]).command else {
            panic!("expected dossier");
        };
        assert!(args.vlm.is_none(), "no flag means no network at all");
    }

    #[test]
    fn structured_format_defaults_to_full() {
        let Command::Structured(args) = parse(&["structured", "a.pdf"]).command else {
            panic!("expected structured");
        };
        assert_eq!(args.structured_format, StructuredFormatArg::Full);
        assert_eq!(
            args.structured_format.to_output_format(),
            StructuredOutputFormat::Full
        );

        let Command::Structured(args) =
            parse(&["structured", "a.pdf", "--structured-format", "markdown"]).command
        else {
            panic!("expected structured");
        };
        assert_eq!(
            args.structured_format.to_output_format(),
            StructuredOutputFormat::MarkdownOnly
        );
    }

    #[test]
    fn output_directory_is_optional_where_files_are_written() {
        let Command::Cut(args) = parse(&["cut", "a.pdf"]).command else {
            panic!("expected cut");
        };
        assert!(args.output.is_none());

        let Command::Cut(args) = parse(&["cut", "a.pdf", "-o", "out"]).command else {
            panic!("expected cut");
        };
        assert_eq!(args.output.as_deref(), Some(std::path::Path::new("out")));
    }
}
