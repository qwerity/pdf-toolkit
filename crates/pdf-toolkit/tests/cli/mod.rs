//! Process-level harness for the CLI contract tests.
//!
//! These tests spawn the real binary, so they pin what a caller actually sees:
//! the argv it passes, the JSON on stdout, the warnings on stderr and the exit
//! code. Two applications consume that contract by piping stdout into a JSON
//! parser, which is why it is pinned here rather than at the library boundary.
//!
//! **Every argv this crate's tests build is constructed by the `argv` module
//! below and nowhere else.** A change to the command-line surface is then a
//! change to those builders, and the assertions on the JSON — the part that
//! must survive a refactor — do not move at all.

#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

/// Outcome of one invocation of the binary.
pub struct Run {
    pub stdout: String,
    pub stderr: String,
    pub status: Option<i32>,
}

impl Run {
    pub fn succeeded(&self) -> bool {
        self.status == Some(0)
    }

    /// Parses stdout as JSON, failing with the captured streams so a broken
    /// contract reports what the tool actually printed.
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.stdout).unwrap_or_else(|error| {
            panic!(
                "stdout is not valid JSON ({error})\n--- stdout ---\n{}\n--- stderr ---\n{}",
                self.stdout, self.stderr
            )
        })
    }

    pub fn expect_success(self) -> Self {
        assert!(
            self.succeeded(),
            "expected success, got status {:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            self.status,
            self.stdout,
            self.stderr
        );
        self
    }

    pub fn expect_failure(self) -> Self {
        assert!(
            !self.succeeded(),
            "expected failure, got success\n--- stdout ---\n{}\n--- stderr ---\n{}",
            self.stdout,
            self.stderr
        );
        self
    }
}

/// Runs the binary with `args`.
///
/// The working directory is pinned to the workspace root because that is where
/// the PDFium library sits (`./dist/`), and the runtime's last-resort lookup is
/// relative. Inheriting the harness's directory would make the tests depend on
/// how the runner was invoked.
pub fn run(args: &[&str]) -> Run {
    let output = Command::new(binary())
        .args(args)
        .current_dir(crate::common::workspace_root())
        .output()
        .expect("failed to spawn the binary under test");

    Run {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        status: output.status.code(),
    }
}

/// Runs the binary, requires success, and returns the parsed stdout.
pub fn run_json(args: &[&str]) -> Value {
    run(args).expect_success().json()
}

pub fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_pdf-toolkit"))
}

/// Every command line the tests issue, in one place.
///
/// The refactor to subcommands rewrites the bodies here; nothing else in the
/// contract tests names a flag.
pub mod argv {
    /// Page count and per-page metadata.
    pub fn inspect(input: &str) -> Vec<String> {
        vec!["inspect".into(), input.into()]
    }

    /// Cut pages into single-page PDFs.
    pub fn cut_pdf(input: &str, pages: &str, out: &str) -> Vec<String> {
        vec![
            "cut".into(),
            input.into(),
            "--pages".into(),
            pages.into(),
            "--output".into(),
            out.into(),
        ]
    }

    /// Merge the selected pages into one PDF.
    pub fn cut_pdf_concat(input: &str, pages: &str, out: &str) -> Vec<String> {
        let mut args = cut_pdf(input, pages, out);
        args.push("--concat".into());
        args
    }

    /// Render pages to PNG at a scale factor.
    pub fn render_png_scale(input: &str, pages: &str, out: &str, scale: &str) -> Vec<String> {
        vec![
            "render".into(),
            input.into(),
            "--pages".into(),
            pages.into(),
            "--output".into(),
            out.into(),
            "--scale".into(),
            scale.into(),
        ]
    }

    /// Render pages to PNG at a fixed output width.
    pub fn render_png_width(input: &str, pages: &str, out: &str, width_px: &str) -> Vec<String> {
        vec![
            "render".into(),
            input.into(),
            "--pages".into(),
            pages.into(),
            "--output".into(),
            out.into(),
            "--scale-width-px".into(),
            width_px.into(),
        ]
    }

    /// Whole-dossier annotation, deterministic passes only.
    pub fn dossier(input: &str) -> Vec<String> {
        vec!["dossier".into(), input.into()]
    }

    /// Whole-dossier annotation with vision-pass answers merged and GeoJSON
    /// written under `out`.
    pub fn dossier_merged(input: &str, answers: &str, out: &str) -> Vec<String> {
        vec![
            "dossier".into(),
            input.into(),
            "--answers".into(),
            answers.into(),
            "--output".into(),
            out.into(),
        ]
    }

    /// Extract plain text into `.txt` files.
    pub fn text(input: &str, pages: &str, out: &str) -> Vec<String> {
        vec![
            "text".into(),
            input.into(),
            "--pages".into(),
            pages.into(),
            "--output".into(),
            out.into(),
        ]
    }

    /// Extract plain text for the selection into a single file.
    pub fn text_concat(input: &str, pages: &str, out: &str) -> Vec<String> {
        let mut args = text(input, pages, out);
        args.push("--concat".into());
        args
    }

    /// Structured extraction with spans and markdown.
    pub fn structured(input: &str, pages: &str) -> Vec<String> {
        vec![
            "structured".into(),
            input.into(),
            "--pages".into(),
            pages.into(),
        ]
    }

    /// Structured extraction, markdown only.
    pub fn structured_markdown(input: &str, pages: &str) -> Vec<String> {
        let mut args = structured(input, pages);
        args.push("--structured-format".into());
        args.push("markdown".into());
        args
    }
}

/// Borrows an owned argv as the slice `run` takes.
pub fn as_args(argv: &[String]) -> Vec<&str> {
    argv.iter().map(String::as_str).collect()
}

/// Runs an owned argv built by [`argv`].
pub fn run_argv(argv: &[String]) -> Run {
    run(&as_args(argv))
}

/// Runs an owned argv built by [`argv`], requiring success.
pub fn json_argv(argv: &[String]) -> Value {
    run_argv(argv).expect_success().json()
}

/// Runs an argv once per test binary and shares the parsed result.
///
/// Several tests assert on different fields of the same command's output.
/// Spawning the binary for each of them re-pays a process launch and a dlopen
/// of the 7 MB native library to re-derive a document a sibling test already
/// produced, so identical command lines are run once and cached.
///
/// Only for commands that write nothing: a cached run cannot be observed
/// twice, so anything asserting on files or on stderr must call [`run_argv`].
pub fn json_argv_cached(argv: &[String]) -> Value {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<HashMap<String, Value>>> =
        std::sync::OnceLock::new();

    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let key = argv.join("\u{1f}");

    if let Some(hit) = cache.lock().expect("cache lock").get(&key) {
        return hit.clone();
    }

    let value = json_argv(argv);
    cache.lock().expect("cache lock").insert(key, value.clone());
    value
}

/// The exact set of keys on a JSON object, sorted.
pub fn keys(value: &Value) -> Vec<String> {
    let mut names: Vec<String> = value
        .as_object()
        .unwrap_or_else(|| panic!("expected a JSON object, got {value}"))
        .keys()
        .cloned()
        .collect();
    names.sort();
    names
}

/// Asserts an object carries exactly `expected` keys.
pub fn assert_keys(value: &Value, expected: &[&str]) {
    let mut want: Vec<String> = expected.iter().map(|k| (*k).to_string()).collect();
    want.sort();
    assert_eq!(keys(value), want, "key set changed for {value}");
}

pub fn path_of(entry: &Value) -> PathBuf {
    PathBuf::from(
        entry
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("entry has no string `path`: {entry}")),
    )
}

/// Asserts the `path` in a result entry names a file that was actually written.
pub fn assert_written(entry: &Value) {
    let path = path_of(entry);
    assert!(
        path.is_absolute(),
        "path must be absolute: {}",
        path.display()
    );
    assert!(path.is_file(), "path was not written: {}", path.display());
}

pub fn as_str(path: &Path) -> &str {
    path.to_str().expect("fixture paths must be valid UTF-8")
}
