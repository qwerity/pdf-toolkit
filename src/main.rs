use anyhow::{Context, Result, bail};
use clap::Parser;
use pdf_cutter::cli::{Args, CliOutputFormat};
use pdf_cutter::error::{PdfCutterError, Result as LibraryResult};
use pdf_cutter::{ExtractionReport, OutputFormat, create_pdfium, extract_pages};
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

fn main() -> ExitCode {
    match run_cli(Args::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run_cli(args: Args) -> Result<()> {
    ensure_input_file(&args.input)?;
    let start = Instant::now();
    let report = extract_requested_pages(&args)?;

    print_report(&report, start.elapsed())
}

fn extract_requested_pages(args: &Args) -> Result<ExtractionReport> {
    let pdfium = create_pdfium().context("failed to initialize pdfium runtime")?;
    let output_format = resolve_output_format(args)?;

    extract_pages(
        &pdfium,
        &args.input,
        args.pages.clone(),
        output_format,
        args.output.as_deref(),
    )
    .with_context(|| format!("failed to extract pages from '{}'", args.input.display()))
}

fn resolve_output_format(args: &Args) -> Result<OutputFormat> {
    match args.format {
        CliOutputFormat::Pdf => {
            if args.scale.is_some() {
                bail!("--scale can only be used with --format png");
            }

            Ok(OutputFormat::Pdf {
                concat: args.concat,
            })
        }
        CliOutputFormat::Png => {
            if args.concat {
                bail!("--concat can only be used with --format pdf");
            }

            Ok(OutputFormat::Png {
                scale: args
                    .scale
                    .context("--scale is required when --format png")?,
            })
        }
    }
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
        if page.page_count == 1 {
            println!(
                "Extracted -> {} ({:.1}x{:.1} mm, {} KB)",
                page.output_path.display(),
                page.width_mm,
                page.height_mm,
                page.size_kb,
            );
        } else {
            println!(
                "Extracted -> {} ({} pages, {} KB)",
                page.output_path.display(),
                page.page_count,
                page.size_kb,
            );
        }
    }

    let extracted_page_count = report
        .pages
        .iter()
        .map(|page| page.page_count)
        .sum::<usize>();

    println!(
        "Done - {} page(s) extracted into {} file(s) in {:.2?}",
        extracted_page_count,
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
    use pdf_cutter::PageSelection;
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

    #[test]
    fn test_resolve_output_format_accepts_concat_pdf() {
        let args = Args {
            input: PathBuf::from("test_assets/input.pdf"),
            pages: PageSelection::All,
            format: CliOutputFormat::Pdf,
            concat: true,
            output: None,
            scale: None,
        };

        let output_format = resolve_output_format(&args).expect("pdf concat should be valid");

        assert_eq!(output_format, OutputFormat::Pdf { concat: true });
    }

    #[test]
    fn test_resolve_output_format_rejects_concat_png() {
        let args = Args {
            input: PathBuf::from("test_assets/input.pdf"),
            pages: PageSelection::All,
            format: CliOutputFormat::Png,
            concat: true,
            output: None,
            scale: Some(2.0),
        };

        let error = resolve_output_format(&args).expect_err("png concat should be rejected");

        assert!(error.to_string().contains("--concat"));
    }
}
