use anyhow::{Context, Result, bail};
use clap::Parser;
use pdf_cutter::cli::Args;
use pdf_cutter::error::{PdfCutterError, Result as LibraryResult};
use pdf_cutter::{ExtractionReport, create_pdfium, extract_pages};
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    run_cli(Args::parse())
}

fn run_cli(args: Args) -> Result<()> {
    ensure_input_file(&args.input)?;
    let start = Instant::now();
    let report = extract_requested_pages(&args)?;

    print_report(&report, start.elapsed())
}

fn extract_requested_pages(args: &Args) -> Result<ExtractionReport> {
    let pdfium = create_pdfium().context("failed to initialize pdfium runtime")?;

    extract_pages(
        &pdfium,
        &args.input,
        args.pages.clone(),
        args.format.into(),
        args.output.as_deref(),
    )
    .with_context(|| format!("failed to extract pages from '{}'", args.input.display()))
}

fn print_report(report: &ExtractionReport, elapsed: Duration) -> Result<()> {
    for warning in &report.warnings {
        eprintln!("Warning: {warning}");
    }

    if report.pages.is_empty() {
        bail!(
            "no pages were extracted; check that the requested pages are within range and the output directory is writable"
        );
    }

    for page in &report.pages {
        println!(
            "Extracted -> {} ({:.1}x{:.1} mm, {} KB)",
            page.output_path.display(),
            page.width_mm,
            page.height_mm,
            page.size_kb,
        );
    }

    println!(
        "Done - {} page(s) extracted in {:.2?}",
        report.pages.len(),
        elapsed,
    );

    Ok(())
}

fn ensure_input_file(path: &Path) -> LibraryResult<()> {
    if !path.exists() {
        return Err(PdfCutterError::InputMissing(path.to_path_buf()));
    }

    if !path.is_file() {
        return Err(PdfCutterError::InputNotFile(path.to_path_buf()));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestPath {
        path: PathBuf,
        is_dir: bool,
    }

    impl TestPath {
        fn unique_path(name: &str) -> PathBuf {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock should be after the Unix epoch")
                .as_nanos();

            std::env::temp_dir().join(format!(
                "pdf_cutter_{name}_{}_{}",
                std::process::id(),
                nanos
            ))
        }

        fn file(name: &str) -> Self {
            let path = Self::unique_path(name);
            File::create(&path).expect("test file should be created");
            Self {
                path,
                is_dir: false,
            }
        }

        fn dir(name: &str) -> Self {
            let path = Self::unique_path(name);
            fs::create_dir(&path).expect("test directory should be created");
            Self { path, is_dir: true }
        }
    }

    impl Drop for TestPath {
        fn drop(&mut self) {
            let _ = if self.is_dir {
                fs::remove_dir(&self.path)
            } else {
                fs::remove_file(&self.path)
            };
        }
    }

    #[test]
    fn test_ensure_input_file_accepts_existing_file() {
        let file = TestPath::file("validate_existing_file");

        let result = ensure_input_file(&file.path);

        assert!(result.is_ok());
    }

    #[test]
    fn test_ensure_input_file_rejects_missing_file() {
        let path = TestPath::unique_path("validate_missing_file");

        let result = ensure_input_file(&path);

        assert!(
            matches!(result, Err(PdfCutterError::InputMissing(error_path)) if error_path == path)
        );
    }

    #[test]
    fn test_ensure_input_file_rejects_directory() {
        let dir = TestPath::dir("validate_directory");

        let result = ensure_input_file(&dir.path);

        assert!(
            matches!(result, Err(PdfCutterError::InputNotFile(error_path)) if error_path == dir.path)
        );
    }
}
