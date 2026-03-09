use pdfium_render::prelude::*;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

pub struct PageResult {
    pub output_path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub size_kb: u64,
}

pub enum PageSelection {
    /// Extract every page in the document.
    All,
    /// Extract the given page numbers (1-indexed, duplicates are ignored).
    Specific(Vec<u16>),
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Bind to the pdfium native library. Call once and keep the result alive for
/// the entire process — passing `&Pdfium` to [`extract_pages`] avoids
/// re-binding on every call.
pub fn create_pdfium() -> Result<Pdfium, String> {
    let mut exe_lib_path = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    exe_lib_path.push(Pdfium::pdfium_platform_library_name());

    let bind = Pdfium::bind_to_library(exe_lib_path.to_string_lossy().as_ref())
        .or_else(|_| Pdfium::bind_to_system_library())
        .or_else(|_| Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path("./")))
        .or_else(|_| {
            Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path("./bin/"))
        })
        .map_err(|e| {
            format!("Failed to bind to pdfium library: {e}. Make sure pdfium is installed.")
        })?;

    Ok(Pdfium::new(bind))
}

/// Extract pages from a PDF.
///
/// - `pdfium`  — created once by the caller via [`create_pdfium`].
/// - `selection` — `PageSelection::All` or `PageSelection::Specific(nums)`.
///   Duplicate or out-of-range page numbers are silently skipped after a
///   warning; the caller receives an `Ok(Vec)` with only the successful pages.
pub fn extract_pages(
    pdfium: &Pdfium,
    pdf_path: &Path,
    selection: PageSelection,
    scale: f32,
    output_folder: Option<&Path>,
) -> Result<Vec<PageResult>, String> {
    let document = pdfium
        .load_pdf_from_file(pdf_path, None)
        .map_err(|e| format!("Failed to open PDF file: {e}"))?;

    let pages = document.pages();
    let total_pages = pages.len();

    if total_pages == 0 {
        return Err("The document contains no pages.".to_string());
    }

    // Resolve + deduplicate page list before touching the filesystem.
    let page_numbers: Vec<u16> = match selection {
        PageSelection::All => (1..=total_pages).collect(),
        PageSelection::Specific(mut nums) => {
            nums.sort_unstable();
            nums.dedup();
            nums
        }
    };

    // Prepare output directory once — propagate the error immediately.
    let out_dir = prepare_output_dir(output_folder)?;

    // Derive stem once — it is the same for every page.
    let stem = pdf_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "document".to_string());

    let mut results = Vec::with_capacity(page_numbers.len());

    for page_number in page_numbers {
        if page_number == 0 || page_number > total_pages {
            eprintln!(
                "Warning: page {page_number} is out of bounds (1..={total_pages}). Skipping."
            );
            continue;
        }

        let page = match pages.get(page_number - 1) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("Warning: could not get page {page_number}: {e}. Skipping.");
                continue;
            }
        };

        let bitmap = match render_page(&page, scale) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("Warning: could not render page {page_number}: {e}. Skipping.");
                continue;
            }
        };

        let image = bitmap.as_image();
        let width = image.width();
        let height = image.height();

        let output_path = out_dir.join(format!("{stem}_page_{page_number}.png"));

        let size_kb = match save_and_get_size(&image, &output_path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Warning: could not save page {page_number}: {e}. Skipping.");
                continue;
            }
        };

        results.push(PageResult {
            output_path,
            width,
            height,
            size_kb,
        });
    }

    Ok(results)
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

fn prepare_output_dir(output_folder: Option<&Path>) -> Result<PathBuf, String> {
    match output_folder {
        Some(folder) => {
            std::fs::create_dir_all(folder).map_err(|e| {
                format!(
                    "Failed to create output directory '{}': {e}",
                    folder.display()
                )
            })?;
            Ok(folder.to_path_buf())
        }
        None => {
            std::env::current_dir().map_err(|e| format!("Failed to get current directory: {e}"))
        }
    }
}

fn render_page<'a>(page: &'a PdfPage<'a>, scale: f32) -> Result<PdfBitmap<'a>, String> {
    let config = PdfRenderConfig::new()
        .scale_page_by_factor(scale)
        .set_clear_color(PdfColor::WHITE);
    page.render_with_config(&config)
        .map_err(|e| format!("Render error: {e}"))
}

fn save_and_get_size(image: &image::DynamicImage, output_path: &Path) -> Result<u64, String> {
    image
        .save(output_path)
        .map_err(|e| format!("Failed to save image to {}: {e}", output_path.display()))?;

    let metadata = std::fs::metadata(output_path)
        .map_err(|e| format!("Failed to read metadata for {}: {e}", output_path.display()))?;

    Ok(metadata.len() / 1024)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn pdfium() -> Pdfium {
        create_pdfium().expect("failed to bind pdfium")
    }

    const PDF: &str = "test_assets/input.pdf";

    #[test]
    fn test_out_of_range_pages_are_skipped() {
        let pdf = PathBuf::from(PDF);
        let pdfium = pdfium();

        // Page 0 is invalid (1-indexed).
        let r = extract_pages(&pdfium, &pdf, PageSelection::Specific(vec![0]), 1.0, None).unwrap();
        assert!(r.is_empty());

        // Page 18 is beyond the 17-page document.
        let r = extract_pages(&pdfium, &pdf, PageSelection::Specific(vec![18]), 1.0, None).unwrap();
        assert!(r.is_empty());
    }

    #[test]
    fn test_valid_page_produces_file() {
        let pdf = PathBuf::from(PDF);
        let pdfium = pdfium();
        let tmp = PathBuf::from("test_output_valid");

        let results = extract_pages(
            &pdfium,
            &pdf,
            PageSelection::Specific(vec![1]),
            1.0,
            Some(&tmp),
        )
        .expect("extraction failed");

        assert_eq!(results.len(), 1);
        let r = &results[0];
        assert!(r.output_path.exists());
        assert!(r.width > 0);
        assert!(r.height > 0);
        assert!(r.size_kb > 0);

        let _ = fs::remove_file(&r.output_path);
        let _ = fs::remove_dir(&tmp);
    }

    #[test]
    fn test_duplicate_pages_are_deduplicated() {
        let pdf = PathBuf::from(PDF);
        let pdfium = pdfium();
        let tmp = PathBuf::from("test_output_dedup");

        // Passing the same page three times should yield exactly one result.
        let results = extract_pages(
            &pdfium,
            &pdf,
            PageSelection::Specific(vec![1, 1, 1]),
            1.0,
            Some(&tmp),
        )
        .expect("extraction failed");

        assert_eq!(results.len(), 1);

        let _ = fs::remove_file(&results[0].output_path);
        let _ = fs::remove_dir(&tmp);
    }
}
