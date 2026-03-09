mod pdf_extractor;

use pdf_extractor::PageSelection;

use clap::Parser;
use std::path::PathBuf;
use std::process;
use std::time::Instant;

/// Extract specific pages from a PDF file as PNG images.
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to the PDF file
    #[arg(short, long)]
    input: PathBuf,

    /// Page numbers to extract, comma-separated (e.g. "1,2,5") or "all"
    #[arg(short = 'p', long)]
    pages: String,

    /// Scale factor for image quality (e.g., 2.0 or 3.0 for better quality)
    #[arg(short, long, default_value_t = 2.0)]
    scale: f32,

    /// Output folder for extracted images (defaults to current directory)
    #[arg(short, long)]
    output: Option<PathBuf>,
}

fn main() {
    let args = Args::parse();

    if !args.input.exists() {
        eprintln!("Error: '{}' does not exist.", args.input.display());
        process::exit(1);
    }

    // Bind to the native library once — shared across all operations.
    let pdfium = pdf_extractor::create_pdfium().unwrap_or_else(|e| {
        eprintln!("Error: {e}");
        process::exit(1);
    });

    let selection = parse_page_selection(&args.pages);
    let start = Instant::now();

    match pdf_extractor::extract_pages(
        &pdfium,
        &args.input,
        selection,
        args.scale,
        args.output.as_deref(),
    ) {
        Ok(results) => {
            if results.is_empty() {
                eprintln!(
                    "Error: no pages were extracted — check that the page numbers are valid."
                );
                process::exit(1);
            }
            for r in &results {
                println!(
                    "Extracted → {} ({}×{} px, {} KB)",
                    r.output_path.display(),
                    r.width,
                    r.height,
                    r.size_kb,
                );
            }
            println!(
                "Done — {} page(s) extracted in {:.2?}",
                results.len(),
                start.elapsed(),
            );
        }
        Err(e) => {
            eprintln!("Error: {e}");
            process::exit(1);
        }
    }
}

/// Converts the `--pages` string into a [`PageSelection`].
fn parse_page_selection(pages: &str) -> PageSelection {
    if pages.trim().eq_ignore_ascii_case("all") {
        PageSelection::All
    } else {
        let nums = pages
            .split(',')
            .filter_map(|s| {
                let s = s.trim();
                s.parse::<u16>()
                    .map_err(|_| {
                        eprintln!("Warning: invalid page '{s}'. Skipping.");
                    })
                    .ok()
            })
            .collect();
        PageSelection::Specific(nums)
    }
}
