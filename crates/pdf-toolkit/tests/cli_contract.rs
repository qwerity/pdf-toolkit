//! The command-line contract, pinned by running the binary.
//!
//! Two applications consume this tool by spawning it and parsing stdout, so
//! what is asserted here is the product: the shape of the JSON, which keys
//! exist in which mode, that stdout carries JSON and nothing else, that
//! warnings go to stderr, and that a rejected command line exits non-zero
//! without printing a partial document.
//!
//! The argv is built by `cli::argv` so that moving the surface to subcommands
//! rewrites those builders and leaves every assertion below untouched.

mod cli;
mod common;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use cli::{argv, as_str, assert_keys, assert_written, json_argv, json_argv_cached, run, run_argv};
use common::{Text, pdfium, write_garbled_pdf, write_pdf, write_pdf_with_image, write_simple_pdf};
use serde_json::Value;

/// Fixture PDFs, written once for the whole test binary.
///
/// They are built through pdfium, which the harness serialises; creating them
/// once keeps that lock off the path of tests that only spawn the binary.
fn fixtures() -> &'static Fixtures {
    static FIXTURES: OnceLock<Fixtures> = OnceLock::new();
    FIXTURES.get_or_init(Fixtures::build)
}

struct Fixtures {
    dir: PathBuf,
    simple: PathBuf,
    garbled: PathBuf,
    image: PathBuf,
    styled: PathBuf,
}

impl Fixtures {
    /// Where the fixtures live.
    ///
    /// Deliberately not a `TempDir`: these are owned by a `static`, and a
    /// static's destructor never runs, so a `TempDir` here would clean up
    /// nothing and leave one orphaned tree per `cargo test`. A fixed path
    /// under `target/` is emptied on the way in, so a run reuses one directory
    /// instead of accumulating them, and `cargo clean` takes it away.
    fn root() -> PathBuf {
        let root = common::workspace_root()
            .join("target")
            .join("tmp")
            .join("cli_contract");

        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create fixture directory");
        root
    }

    fn build() -> Self {
        let dir = Self::root();
        let simple = dir.join("simple.pdf");
        let garbled = dir.join("garbled.pdf");
        let image = dir.join("image.pdf");
        let styled = dir.join("styled.pdf");

        let pdfium = pdfium();
        write_simple_pdf(&pdfium, &simple, 3);
        write_garbled_pdf(&pdfium, &garbled);
        write_pdf_with_image(&pdfium, &image, 400.0);
        write_pdf(
            &pdfium,
            &styled,
            &[vec![
                Text::bold(72.0, 760.0, 20.0, "Document Heading"),
                Text::plain(72.0, 720.0, 11.0, "Body text under the heading."),
            ]],
        );

        Self {
            dir,
            simple,
            garbled,
            image,
            styled,
        }
    }
}

/// A private output directory for one test.
fn out_dir(name: &str) -> PathBuf {
    let dir = fixtures().dir.join(format!("out_{name}"));
    std::fs::create_dir_all(&dir).expect("create output dir");
    dir
}

fn simple() -> &'static Path {
    &fixtures().simple
}

// ---------------------------------------------------------------------------
// inspection
// ---------------------------------------------------------------------------

#[test]
fn inspect_reports_page_count_and_one_entry_per_page() {
    let output = json_argv_cached(&argv::inspect(as_str(simple())));

    assert_eq!(output["page_count"], 3);
    let pages = output["pages"].as_object().expect("pages object");
    assert_eq!(pages.len(), 3);
    assert!(
        ["1", "2", "3"].iter().all(|key| pages.contains_key(*key)),
        "pages must be keyed by one-based page number, got {:?}",
        pages.keys().collect::<Vec<_>>()
    );
}

#[test]
fn inspect_page_entry_carries_exactly_the_documented_keys() {
    let output = json_argv_cached(&argv::inspect(as_str(simple())));

    assert_keys(
        &output["pages"]["1"],
        &[
            "width_px",
            "height_px",
            "contains_images",
            "image_count",
            "image_coverage",
            "llm_recommended",
        ],
    );
    assert_keys(&output, &["page_count", "pages"]);
}

#[test]
fn inspect_reports_a4_dimensions_in_points_for_a_text_page() {
    let output = json_argv_cached(&argv::inspect(as_str(simple())));
    let page = &output["pages"]["1"];

    assert_eq!(page["width_px"], 595);
    assert_eq!(page["height_px"], 842);
    assert_eq!(page["contains_images"], false);
    assert_eq!(page["image_count"], 0);
    assert_eq!(page["llm_recommended"], false);
}

#[test]
fn inspect_omits_llm_reasons_when_no_pass_is_recommended() {
    let output = json_argv_cached(&argv::inspect(as_str(simple())));

    assert_eq!(output["pages"]["1"]["llm_recommended"], false);
    assert!(
        output["pages"]["1"].get("llm_reasons").is_none(),
        "llm_reasons must be absent rather than empty when nothing is recommended"
    );
}

#[test]
fn inspect_explains_why_an_image_heavy_page_needs_a_vision_pass() {
    let output = json_argv_cached(&argv::inspect(as_str(&fixtures().image)));
    let page = &output["pages"]["1"];

    assert_eq!(page["llm_recommended"], true);
    assert_eq!(page["contains_images"], true);
    assert!(
        page["image_coverage"].as_f64().expect("coverage") > 0.2,
        "a 400pt square on A4 must exceed the image_heavy threshold, got {}",
        page["image_coverage"]
    );

    let reasons = page["llm_reasons"].as_array().expect("llm_reasons array");
    assert!(!reasons.is_empty());
    for reason in reasons {
        assert_keys(reason, &["reason", "detail"]);
        assert!(reason["reason"].is_string() && reason["detail"].is_string());
    }
    let slugs: Vec<&str> = reasons
        .iter()
        .filter_map(|r| r["reason"].as_str())
        .collect();
    assert!(
        slugs.contains(&"image_heavy") || slugs.contains(&"no_text_layer"),
        "expected an image-related reason, got {slugs:?}"
    );
}

#[test]
fn inspect_flags_a_garbled_text_layer_as_unreadable() {
    let output = json_argv_cached(&argv::inspect(as_str(&fixtures().garbled)));
    let page = &output["pages"]["1"];

    assert_eq!(page["llm_recommended"], true);
    let slugs: Vec<&str> = page["llm_reasons"]
        .as_array()
        .expect("llm_reasons array")
        .iter()
        .filter_map(|r| r["reason"].as_str())
        .collect();
    assert!(
        slugs.contains(&"unreadable_text"),
        "expected unreadable_text, got {slugs:?}"
    );
}

// ---------------------------------------------------------------------------
// cutting to PDF
// ---------------------------------------------------------------------------

#[test]
fn cut_pdf_writes_one_entry_per_selected_page() {
    let out = out_dir("cut_pdf");
    let output = json_argv(&argv::cut_pdf(as_str(simple()), "1,3", as_str(&out)));

    let pages = output["pages"].as_object().expect("pages object");
    assert_eq!(pages.len(), 2);
    assert!(pages.contains_key("1") && pages.contains_key("3"));
    for entry in pages.values() {
        assert_written(entry);
    }
}

#[test]
fn cut_pdf_entry_carries_exactly_the_documented_keys() {
    let out = out_dir("cut_pdf_keys");
    let output = json_argv(&argv::cut_pdf(as_str(simple()), "1", as_str(&out)));

    assert_keys(
        &output["pages"]["1"],
        &[
            "path",
            "size_kb",
            "width_px",
            "height_px",
            "contains_images",
        ],
    );
    assert_keys(&output, &["pages"]);
}

#[test]
fn cut_pdf_reports_dimensions_at_unit_scale() {
    let out = out_dir("cut_pdf_dims");
    let output = json_argv(&argv::cut_pdf(as_str(simple()), "2", as_str(&out)));
    let page = &output["pages"]["2"];

    assert_eq!(page["width_px"], 595);
    assert_eq!(page["height_px"], 842);
    assert!(page["size_kb"].as_u64().expect("size_kb") > 0);
}

#[test]
fn cut_pdf_concat_reports_one_file_and_the_source_page_metadata() {
    let out = out_dir("cut_concat");
    let output = json_argv(&argv::cut_pdf_concat(as_str(simple()), "1-2", as_str(&out)));

    assert_keys(&output, &["concat", "page_info"]);
    assert_keys(&output["concat"], &["path", "pages", "size_kb"]);
    assert_eq!(
        output["concat"]["pages"], "1-2",
        "concat echoes the selection as written on the command line"
    );
    assert_written(&output["concat"]);

    let info = output["page_info"].as_object().expect("page_info object");
    assert_eq!(info.len(), 2);
    assert!(
        output.get("pages").is_none(),
        "concat output must not also carry a per-page map"
    );
}

// ---------------------------------------------------------------------------
// rendering to PNG
// ---------------------------------------------------------------------------

#[test]
fn render_png_reports_the_scale_it_used() {
    let out = out_dir("png_scale");
    let output = json_argv(&argv::render_png_scale(
        as_str(simple()),
        "1",
        as_str(&out),
        "2",
    ));

    assert_keys(&output, &["scale", "pages"]);
    assert_eq!(output["scale"], 2.0);
    assert_written(&output["pages"]["1"]);
}

/// A4 in PDF points. The reported `width_px`/`height_px` of an unrendered page
/// are these truncated to whole points, which is why a rendered dimension is
/// not simply the reported one multiplied by the scale.
const A4_WIDTH_PT: f64 = 595.276;
const A4_HEIGHT_PT: f64 = 841.89;

#[test]
fn render_png_reports_rendered_pixel_dimensions_not_page_points() {
    let out = out_dir("png_dims");
    let output = json_argv(&argv::render_png_scale(
        as_str(simple()),
        "1",
        as_str(&out),
        "2",
    ));
    let page = &output["pages"]["1"];

    assert_eq!(
        page["width_px"], 1191,
        "A4 is {A4_WIDTH_PT}pt wide, so scale 2 rounds up past the 1190 that 595pt would give"
    );
    assert_eq!(page["height_px"], 1684, "{A4_HEIGHT_PT}pt at scale 2");
}

#[test]
fn render_png_at_target_width_reports_that_width_and_preserves_aspect() {
    let out = out_dir("png_width");
    let output = json_argv(&argv::render_png_width(
        as_str(simple()),
        "1",
        as_str(&out),
        "1400",
    ));

    assert_keys(&output, &["scale_width_px", "pages"]);
    assert_eq!(output["scale_width_px"], 1400);

    let page = &output["pages"]["1"];
    assert_eq!(page["width_px"], 1400);
    let height = page["height_px"].as_f64().expect("height");
    let expected = 1400.0 * A4_HEIGHT_PT / A4_WIDTH_PT;
    assert!(
        (height - expected).abs() <= 1.0,
        "aspect ratio must be preserved: got {height}, expected about {expected}"
    );
}

// ---------------------------------------------------------------------------
// plain text
// ---------------------------------------------------------------------------

#[test]
fn text_over_all_pages_writes_one_aggregate_file() {
    let out = out_dir("text_all");
    let output = json_argv(&argv::text(as_str(simple()), "all", as_str(&out)));

    assert_keys(&output, &["concat"]);
    assert_keys(&output["concat"], &["path", "pages", "size_kb"]);
    assert_written(&output["concat"]);

    let body = std::fs::read_to_string(cli::path_of(&output["concat"])).expect("read text");
    for page in 1..=3 {
        assert!(
            body.contains(&format!("____Page{page}____")),
            "each page must be marked in the aggregate file"
        );
    }
}

#[test]
fn text_for_a_selection_writes_one_file_per_page() {
    let out = out_dir("text_pages");
    let output = json_argv(&argv::text(as_str(simple()), "1,3", as_str(&out)));

    let pages = output["pages"].as_object().expect("pages object");
    assert_eq!(pages.len(), 2);
    for key in ["1", "3"] {
        assert_keys(&pages[key], &["path", "size_kb"]);
        assert_written(&pages[key]);
    }
}

#[test]
fn text_with_concat_collapses_a_selection_into_one_file() {
    let out = out_dir("text_concat");
    let output = json_argv(&argv::text_concat(as_str(simple()), "1-2", as_str(&out)));

    assert_keys(&output, &["concat"]);
    assert_eq!(output["concat"]["pages"], "1-2");
    assert_written(&output["concat"]);
}

// ---------------------------------------------------------------------------
// structured text
// ---------------------------------------------------------------------------

#[test]
fn structured_full_emits_pages_with_markdown_and_spans() {
    let output = json_argv_cached(&argv::structured(as_str(&fixtures().styled), "all"));

    assert_keys(&output, &["pages", "selected_page_count"]);
    assert_eq!(output["selected_page_count"], 1);

    let pages = output["pages"].as_array().expect("pages array");
    assert_eq!(pages.len(), 1, "structured output is an array, not a map");
    assert_keys(&pages[0], &["page", "markdown", "spans"]);
    assert_eq!(pages[0]["page"], 1);
    assert!(
        pages[0]["markdown"]
            .as_str()
            .expect("markdown")
            .contains("Document Heading")
    );
}

#[test]
fn structured_span_carries_exactly_the_documented_keys() {
    let output = json_argv_cached(&argv::structured(as_str(&fixtures().styled), "all"));
    let spans = output["pages"][0]["spans"].as_array().expect("spans array");

    assert!(!spans.is_empty(), "a text page must yield spans");
    for span in spans {
        assert_keys(
            span,
            &[
                "text",
                "font_size",
                "font_name",
                "bold",
                "italic",
                "x",
                "y",
                "width",
                "height",
            ],
        );
    }
}

#[test]
fn structured_markdown_only_omits_spans_entirely() {
    let output = json_argv_cached(&argv::structured_markdown(
        as_str(&fixtures().styled),
        "all",
    ));
    let page = &output["pages"][0];

    assert_keys(page, &["page", "markdown"]);
    assert!(
        page.get("spans").is_none(),
        "markdown-only output must not carry a spans key"
    );
    assert!(!page["markdown"].as_str().expect("markdown").is_empty());
}

#[test]
fn structured_honours_a_page_selection() {
    let output = json_argv_cached(&argv::structured(as_str(simple()), "2"));

    assert_eq!(output["selected_page_count"], 1);
    let pages = output["pages"].as_array().expect("pages array");
    assert_eq!(pages.len(), 1);
    assert_eq!(
        pages[0]["page"], 2,
        "page numbers stay one-based and absolute"
    );
}

// ---------------------------------------------------------------------------
// warnings and partial results
// ---------------------------------------------------------------------------

#[test]
fn out_of_bounds_pages_are_reported_in_errors_without_failing_the_run() {
    let out = out_dir("oob");
    let run = run_argv(&argv::text(as_str(simple()), "1,99", as_str(&out))).expect_success();
    let output = run.json();

    let errors = output["errors"].as_object().expect("errors object");
    assert!(
        errors.contains_key("99"),
        "errors are keyed by the offending page number, got {:?}",
        errors.keys().collect::<Vec<_>>()
    );
    assert!(
        output["pages"]
            .as_object()
            .expect("pages")
            .contains_key("1"),
        "the pages that are in range must still be produced"
    );
}

#[test]
fn warnings_are_written_to_stderr_and_never_to_stdout() {
    let out = out_dir("warn_stream");
    let run = run_argv(&argv::text(as_str(simple()), "1,99", as_str(&out))).expect_success();

    assert!(
        run.stderr.contains("Warning:"),
        "expected a human-readable warning on stderr, got {:?}",
        run.stderr
    );
    run.json();
    assert!(
        !run.stdout.contains("Warning:"),
        "stdout must stay parseable JSON, got {:?}",
        run.stdout
    );
}

#[test]
fn a_garbled_page_is_skipped_and_explained_rather_than_silently_empty() {
    let out = out_dir("garbled_text");
    let run = run_argv(&argv::text(
        as_str(&fixtures().garbled),
        "all",
        as_str(&out),
    ))
    .expect_success();
    let output = run.json();

    let errors = output["errors"].as_object().expect("errors object");
    assert_eq!(errors.len(), 1, "the one unreadable page must be reported");
    assert!(
        errors["1"]
            .as_str()
            .expect("error message")
            .contains("corrupted"),
        "the error must say why the page was skipped, got {:?}",
        errors["1"]
    );
    assert!(
        output.get("pages").is_none() && output.get("concat").is_none(),
        "a run that produced no file must not claim one"
    );
}

/// A run that produced nothing is the case a user most needs explained, so it
/// reports on stderr like any other.
///
/// It did not always: warnings were printed on the path that produced files,
/// and the empty-result path returned before reaching it, leaving stderr
/// silent and the reason visible only to a caller parsing `errors`.
#[test]
fn warnings_reach_stderr_whether_or_not_any_page_succeeded() {
    let out = out_dir("warn_both_paths");

    let partial = run_argv(&argv::text(as_str(simple()), "1,99", as_str(&out))).expect_success();
    assert!(
        partial.stderr.contains("Warning:"),
        "a partially successful run echoes its warnings, got {:?}",
        partial.stderr
    );

    let total = run_argv(&argv::text(
        as_str(&fixtures().garbled),
        "all",
        as_str(&out),
    ))
    .expect_success();
    assert!(
        total.stderr.contains("Warning:"),
        "a run that produced nothing must still say why, got {:?}",
        total.stderr
    );
    assert!(
        total.json()["errors"].is_object(),
        "the reason must also be available to a caller"
    );
}

// ---------------------------------------------------------------------------
// stdout discipline
// ---------------------------------------------------------------------------

#[test]
fn stdout_is_a_single_json_document_in_every_success_mode() {
    let out = out_dir("pure_json");
    let dir = as_str(&out);
    let input = as_str(simple());

    let modes: Vec<Vec<String>> = vec![
        argv::inspect(input),
        argv::cut_pdf(input, "1", dir),
        argv::cut_pdf_concat(input, "1-2", dir),
        argv::render_png_scale(input, "1", dir, "1"),
        argv::render_png_width(input, "1", dir, "600"),
        argv::text(input, "1", dir),
        argv::text_concat(input, "1-2", dir),
        argv::structured(input, "1"),
        argv::structured_markdown(input, "1"),
    ];

    for mode in &modes {
        let run = run_argv(mode).expect_success();
        let value: Value = serde_json::from_str(&run.stdout).unwrap_or_else(|error| {
            panic!(
                "`{}` did not print one JSON document: {error}",
                mode.join(" ")
            )
        });
        assert!(
            value.is_object(),
            "`{}` must print a JSON object",
            mode.join(" ")
        );
    }
}

// ---------------------------------------------------------------------------
// rejected command lines
// ---------------------------------------------------------------------------

/// Asserts a command line is refused with a non-zero status, a message on
/// stderr, and no partial document on stdout for a caller to misparse.
fn assert_rejected(args: &[&str]) {
    let run = run(args).expect_failure();
    assert!(
        !run.stderr.trim().is_empty(),
        "a rejected command line must say why: {args:?}"
    );
    assert!(
        run.stdout.trim().is_empty(),
        "a rejected command line must print no JSON, got {:?}",
        run.stdout
    );
}

#[test]
fn a_missing_input_file_is_refused() {
    assert_rejected(&cli::as_args(&argv::inspect("does_not_exist.pdf")));
}

#[test]
fn a_directory_given_as_input_is_refused() {
    // A directory that is actually in the repository, so this keeps exercising
    // the not-a-file guard. Naming one that does not exist on a fresh clone
    // would still pass, by way of the missing-input guard instead.
    let directory = common::workspace_root().join("crates");
    assert!(directory.is_dir(), "fixture directory must exist");

    assert_rejected(&cli::as_args(&argv::inspect(as_str(&directory))));
}

#[test]
fn a_file_that_is_not_a_pdf_is_refused() {
    let bogus = fixtures().dir.join("not_a_pdf.pdf");
    std::fs::write(&bogus, b"this is not a PDF").expect("write bogus file");

    assert_rejected(&cli::as_args(&argv::inspect(as_str(&bogus))));
}

/// Options that were meaningful in one mode and an error in another no longer
/// exist on the command they did not apply to, so the request cannot be
/// expressed at all. `cli.rs` covers the parse; this covers the exit status a
/// caller sees.
#[test]
fn an_option_that_does_not_belong_to_a_command_is_refused() {
    let input = as_str(simple());

    assert_rejected(&["cut", input, "--scale", "2"]);
    assert_rejected(&["cut", input, "--scale-width-px", "800"]);
    assert_rejected(&["render", input, "--concat"]);
    assert_rejected(&["text", input, "--scale", "2"]);
    assert_rejected(&["structured", input, "--concat"]);
    assert_rejected(&["structured", input, "--output", "out"]);
}

#[test]
fn png_sizing_options_are_mutually_exclusive() {
    assert_rejected(&[
        "render",
        as_str(simple()),
        "--scale",
        "2",
        "--scale-width-px",
        "800",
    ]);
}

#[test]
fn a_non_positive_scale_is_refused() {
    let input = as_str(simple());

    assert_rejected(&["render", input, "--scale", "0"]);
    assert_rejected(&["render", input, "--scale", "-1"]);
    assert_rejected(&["render", input, "--scale-width-px", "0"]);
}

#[test]
fn an_unparseable_page_selection_is_refused() {
    let input = as_str(simple());

    assert_rejected(&["cut", input, "--pages", "3-1"]);
    assert_rejected(&["cut", input, "--pages", "abc"]);
}

#[test]
fn a_command_line_naming_no_command_is_refused() {
    assert_rejected(&[]);
    assert_rejected(&[as_str(simple())]);
}

#[test]
fn a_command_with_no_input_is_refused() {
    for command in ["inspect", "cut", "render", "text", "structured"] {
        assert_rejected(&[command]);
    }
}

/// The old flag surface is gone rather than quietly accepted, so a caller
/// still on it fails loudly instead of appearing to work.
#[test]
fn the_previous_flag_surface_is_no_longer_accepted() {
    let input = as_str(simple());

    assert_rejected(&["--input", input, "--page-count"]);
    assert_rejected(&["--input", input, "--format", "pdf"]);
    assert_rejected(&["--input", input, "--extract"]);
    assert_rejected(&["--input", input, "--extract-structured"]);
}

// ---------------------------------------------------------------------------
// the surface itself
// ---------------------------------------------------------------------------

#[test]
fn version_and_help_are_available_and_name_the_tool() {
    let version = run(&["--version"]).expect_success();
    assert!(
        version.stdout.contains(env!("CARGO_PKG_VERSION")),
        "--version must report the package version, got {:?}",
        version.stdout
    );

    let help = run(&["--help"]).expect_success();
    assert!(
        !help.stdout.trim().is_empty(),
        "--help must describe the tool"
    );
}
