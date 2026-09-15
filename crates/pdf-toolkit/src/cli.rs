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
    /// Refine one page's recovered CAD geometry with a hosted model
    /// (experimental): layers, noise removal, ring closing — proposed by the
    /// model, applied deterministically.
    Cadfix(CadfixArgs),
    /// Reconstruct the building's 3D massing from the dossier's drawings with
    /// a hosted model (experimental): footprints from extracted geometry,
    /// heights read off the elevations, extruded GeoJSON for a map.
    Building(BuildingArgs),
    /// Reconstruct the building storey by storey from the CAD-exported plan
    /// sheets, deterministically: walls, rooms and openings by fill colour,
    /// storeys stacked on the printed grid, levels from the section —
    /// `model.json`, glTF, and extruded GeoJSON placed on the map where the
    /// dossier allows.
    Model(ModelArgs),
    /// Shrink the PDF by resampling raster images drawn above a resolution,
    /// leaving text, fonts and vector content byte-identical.
    Downsample(DownsampleArgs),
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
    /// (`OPENROUTER_API_KEY`), and `gpt-*` ids like `gpt-6-astra` use the
    /// OpenAI API (`OPENAI_API_KEY`). **This sends page images to that
    /// provider** — networked and only ever opt-in, like `cadfix` and
    /// `building`. Pages covered by `--answers` are not re-asked.
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

    /// A GeoJSON file of WGS 84 parcel polygon(s), matched by shape to
    /// drawing sheets that print no coordinate table — see `vector --parcel`.
    #[arg(long, value_name = "FILE")]
    pub parcel: Option<PathBuf>,
}

#[derive(ClapArgs, Debug)]
pub struct CadfixArgs {
    /// Source PDF.
    pub input: PathBuf,

    /// The page holding the drawing to refine.
    #[arg(long, value_parser = parse_page_number)]
    pub page: usize,

    /// Hosted model; the spec's shape picks the provider, as with `--vlm`.
    /// **This sends the page image to that provider.**
    #[arg(long, default_value = pdf_vlm::DEFAULT_MODEL)]
    pub model: String,

    /// Also write the refined collection as
    /// `<dir>/<document>_pNN.fixed.geojson`. The same JSON is on stdout
    /// either way.
    #[arg(short, long)]
    pub output: Option<PathBuf>,
}

#[derive(ClapArgs, Debug)]
pub struct BuildingArgs {
    /// Source PDF.
    pub input: PathBuf,

    /// Reviewed or vision-pass answers (`docs/annotation-schema.md`); their
    /// labels sharpen which sheets are shown to the model.
    #[arg(long)]
    pub answers: Option<PathBuf>,

    /// Hosted model; the spec's shape picks the provider, as with `--vlm`.
    /// **This sends the selected drawing sheets to that provider.**
    #[arg(long, default_value = pdf_vlm::DEFAULT_MODEL)]
    pub model: String,

    /// Also write the extruded collection as
    /// `<dir>/building/<document>.geojson`. The same JSON is on stdout
    /// either way.
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// A GeoJSON file of WGS 84 parcel polygon(s), matched by shape to the
    /// anchor sheet when it prints no coordinate table — what puts a
    /// reconstruction on the map for a dossier without a survey table.
    #[arg(long, value_name = "FILE")]
    pub parcel: Option<PathBuf>,
}

#[derive(ClapArgs, Debug)]
pub struct ModelArgs {
    /// Source PDF.
    pub input: PathBuf,

    /// A parameters file (the `params` block of a previous run, edited):
    /// which pages are the floor plans and the roof plan, the plan scale,
    /// the storey levels, the fill colours, the grid labels. Skips detection.
    #[arg(long, value_name = "FILE")]
    pub params: Option<PathBuf>,

    /// Ask a hosted model to propose the parameters, shown the detected
    /// ones and the drawing sheets; the proposal is checked against the
    /// drawings before use. Bare `--llm-params` uses `gemini-3.8-flash`.
    /// **This sends the drawing sheets to that provider.**
    #[arg(
        long = "llm-params",
        value_name = "MODEL",
        num_args = 0..=1,
        default_missing_value = pdf_vlm::DEFAULT_MODEL
    )]
    pub llm_params: Option<String>,

    /// A real-world reference placing the model, as
    /// `local_x,local_y,longitude,latitude` in the model's own metres.
    /// Repeat it: two or more settle position and rotation. Without them
    /// the model is placed from a georeferenced sibling sheet drawing the
    /// same outline, or stays in its own metres.
    #[arg(
        long = "georeference",
        short = 'g',
        value_parser = parse_reference_arg,
        allow_hyphen_values = true
    )]
    pub georeference: Vec<Reference>,

    /// A GeoJSON file of WGS 84 parcel polygon(s), matched by shape to the
    /// dossier's site plan when it prints no coordinate table — what lets
    /// the model be placed from that sheet.
    #[arg(long, value_name = "FILE")]
    pub parcel: Option<PathBuf>,

    /// Reviewed or vision-pass answers (`docs/annotation-schema.md`): their
    /// page labels name the floor plans, the roof plan and the elevations
    /// for a set whose title blocks the classifier cannot read.
    #[arg(long, value_name = "FILE")]
    pub answers: Option<PathBuf>,

    /// Also write `model/<document>.model.json`, `.glb`, `.geojson` and
    /// `.params.json` under this directory. The same JSON is on stdout
    /// either way.
    #[arg(short, long)]
    pub output: Option<PathBuf>,
}

impl ModelArgs {
    /// Rejects a single reference, for the reason `vector` does.
    pub fn validate(&self) -> Result<(), String> {
        refuse_single_reference(&self.georeference, "rotation")
    }
}

/// One reference fixes position but leaves the named freedoms open, so a
/// placement built from it would be confidently wrong rather than refused.
/// The check lives here so the message names the flag the user typed.
fn refuse_single_reference(references: &[Reference], leaves: &str) -> Result<(), String> {
    if references.len() == 1 {
        return Err(format!(
            "--georeference needs at least two references: one fixes position but leaves \
             {leaves} undetermined"
        ));
    }
    Ok(())
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

    /// A GeoJSON file of WGS 84 parcel polygon(s) — the cadastre's statement
    /// of the boundary — matched by shape to sheets that draw the parcel but
    /// print no coordinate table (`georeference.source: "parcel_polygon"`).
    #[arg(long, value_name = "FILE")]
    pub parcel: Option<PathBuf>,
}

impl VectorArgs {
    /// Rejects a single reference up front.
    pub fn validate(&self) -> Result<(), String> {
        refuse_single_reference(&self.georeference, "rotation and scale")
    }
}

#[derive(ClapArgs, Debug)]
pub struct DownsampleArgs {
    /// Source PDF.
    pub input: PathBuf,

    /// The PDF to write. Must not be the input.
    #[arg(short, long)]
    pub output: PathBuf,

    /// Resolution to resample to, in pixels per displayed inch.
    #[arg(long, default_value_t = 200, value_parser = parse_dpi)]
    pub dpi: u32,

    /// Only images drawn above this resolution are resampled. Defaults to
    /// 10 % above `--dpi`; must not be below it.
    #[arg(long = "threshold-dpi", value_parser = parse_dpi)]
    pub threshold_dpi: Option<u32>,

    /// JPEG quality, 1–100, for images the PDF stores as JPEG. Losslessly
    /// stored images stay lossless.
    #[arg(
        long,
        default_value_t = pdf_downsample::DEFAULT_JPEG_QUALITY,
        value_parser = clap::value_parser!(u8).range(1..=100)
    )]
    pub quality: u8,
}

impl DownsampleArgs {
    /// Rejects a threshold below the target, which would ask to enlarge images.
    pub fn validate(&self) -> Result<(), String> {
        match self.threshold_dpi {
            Some(threshold) if threshold < self.dpi => Err(format!(
                "--threshold-dpi {threshold} is below --dpi {}; images are only ever made smaller",
                self.dpi
            )),
            _ => Ok(()),
        }
    }

    pub fn options(&self) -> pdf_downsample::DownsampleOptions {
        let defaults = pdf_downsample::DownsampleOptions::new(self.dpi);
        pdf_downsample::DownsampleOptions {
            threshold_dpi: self.threshold_dpi.unwrap_or(defaults.threshold_dpi),
            jpeg_quality: self.quality,
            ..defaults
        }
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
            Self::Cadfix(args) => &args.input,
            Self::Building(args) => &args.input,
            Self::Model(args) => &args.input,
            Self::Downsample(args) => &args.input,
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

fn parse_page_number(raw: &str) -> Result<usize, String> {
    raw.parse::<usize>()
        .ok()
        .filter(|n| *n >= 1)
        .ok_or_else(|| format!("'{raw}' is not a one-based page number"))
}

/// Resolutions a page image is meaningfully resampled to: below 36 dpi text is
/// unreadable, above 2400 nothing is ever drawn that finely.
fn parse_dpi(value: &str) -> std::result::Result<u32, String> {
    let dpi = value.parse::<u32>().map_err(|error| error.to_string())?;
    if (36..=2400).contains(&dpi) {
        Ok(dpi)
    } else {
        Err(format!("dpi must be between 36 and 2400, got {dpi}"))
    }
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
    fn model_takes_the_answers_file() {
        let cli = parse(&["model", "a.pdf", "--answers", "x.json"]);
        assert!(matches!(cli.command, Command::Model(ref m) if m.answers.is_some()));
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
            vec!["cadfix", "a.pdf", "--page", "9"],
            vec!["building", "a.pdf"],
            vec!["model", "a.pdf"],
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
    fn downsample_needs_an_output_and_defaults_to_200_dpi() {
        rejects(&["downsample", "a.pdf"]);
        let Command::Downsample(args) = parse(&["downsample", "a.pdf", "-o", "b.pdf"]).command
        else {
            panic!("expected downsample");
        };
        let options = args.options();
        assert_eq!(
            (
                options.target_dpi,
                options.threshold_dpi,
                options.jpeg_quality
            ),
            (200, 220, 85)
        );
        assert!(args.validate().is_ok());
    }

    #[test]
    fn downsample_rejects_out_of_range_values_and_a_threshold_below_the_target() {
        rejects(&["downsample", "a.pdf", "-o", "b.pdf", "--dpi", "10"]);
        rejects(&["downsample", "a.pdf", "-o", "b.pdf", "--quality", "0"]);
        rejects(&["downsample", "a.pdf", "-o", "b.pdf", "--quality", "101"]);
        let Command::Downsample(args) = parse(&[
            "downsample",
            "a.pdf",
            "-o",
            "b.pdf",
            "--dpi",
            "300",
            "--threshold-dpi",
            "150",
        ])
        .command
        else {
            panic!("expected downsample");
        };
        assert!(args.validate().is_err());
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
        rejects(&["cadfix", "a.pdf", "--page", "9", "--pages", "1-3"]);
        rejects(&["cadfix", "a.pdf", "--page", "9", "--answers", "x.json"]);
        rejects(&["building", "a.pdf", "--page", "9"]);
        rejects(&["dossier", "a.pdf", "--model", "gpt-6-astra"]);
        rejects(&["model", "a.pdf", "--pages", "1"]);
        rejects(&["model", "a.pdf", "--model", "gpt-6-astra"]);
    }

    #[test]
    fn model_is_offline_unless_asked_and_refuses_one_reference() {
        let Command::Model(args) = parse(&["model", "a.pdf"]).command else {
            panic!("expected model");
        };
        assert!(args.llm_params.is_none() && args.params.is_none() && args.output.is_none());
        args.validate().expect("no references is fine");

        let Command::Model(args) = parse(&["model", "a.pdf", "--llm-params"]).command else {
            panic!("expected model");
        };
        assert_eq!(args.llm_params.as_deref(), Some("gemini-3.8-flash"));

        let Command::Model(args) = parse(&[
            "model",
            "a.pdf",
            "--params",
            "p.json",
            "-g",
            "-1,-2,44.5,40.1",
            "-g",
            "3,4,44.6,40.2",
            "--parcel",
            "parcel.geojson",
            "-o",
            "out",
        ])
        .command
        else {
            panic!("expected model");
        };
        assert_eq!(args.georeference.len(), 2);
        assert_eq!(args.params.as_deref(), Some(std::path::Path::new("p.json")));
        args.validate().expect("two references");

        let Command::Model(args) = parse(&["model", "a.pdf", "-g", "0,0,44.5,40.1"]).command else {
            panic!("expected model");
        };
        assert!(args.validate().is_err(), "one reference is refused");
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
    fn cadfix_needs_a_page_and_defaults_to_astra() {
        rejects(&["cadfix", "a.pdf"]);
        rejects(&["cadfix", "a.pdf", "--page", "0"]);
        rejects(&["cadfix", "a.pdf", "--page", "nine"]);

        let Command::Cadfix(args) = parse(&["cadfix", "a.pdf", "--page", "9"]).command else {
            panic!("expected cadfix");
        };
        assert_eq!(args.page, 9);
        assert_eq!(args.model, pdf_vlm::DEFAULT_MODEL);
        assert!(args.output.is_none());
    }

    #[test]
    fn building_takes_answers_and_a_model_override() {
        let Command::Building(args) = parse(&[
            "building",
            "a.pdf",
            "--answers",
            "a.json",
            "--model",
            pdf_vlm::ASTRA_MODEL,
            "-o",
            "out",
        ])
        .command
        else {
            panic!("expected building");
        };
        assert_eq!(
            args.answers.as_deref(),
            Some(std::path::Path::new("a.json"))
        );
        assert_eq!(args.model, pdf_vlm::ASTRA_MODEL);
        assert_eq!(args.output.as_deref(), Some(std::path::Path::new("out")));

        let Command::Building(args) = parse(&["building", "a.pdf"]).command else {
            panic!("expected building");
        };
        assert_eq!(
            args.model,
            pdf_vlm::DEFAULT_MODEL,
            "the corpus benchmark winner is the default"
        );
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
