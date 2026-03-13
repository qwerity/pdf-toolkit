use crate::error::{PdfCutterError, Result};
use crate::output::{
    OutputFormat, PageResult, file_size_kb, output_stem, reserve_output_file, resolve_output_dir,
};
use crate::page_selection::PageSelection;
use pdfium_render::prelude::*;
use std::fmt;
use std::num::NonZeroU16;
use std::path::{Path, PathBuf};

pub struct ExtractionReport {
    pub pages: Vec<PageResult>,
    pub warnings: Vec<ExtractionWarning>,
}

#[derive(Debug, Eq, PartialEq)]
pub enum ExtractionWarning {
    PageOutOfBounds { page_number: u16, total_pages: u16 },
    LoadPage { page_number: u16, reason: String },
    SavePage { page_number: u16, reason: String },
}

impl fmt::Display for ExtractionWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PageOutOfBounds {
                page_number,
                total_pages,
            } => write!(
                f,
                "page {page_number} is out of bounds (1..={total_pages}). Skipping."
            ),
            Self::LoadPage {
                page_number,
                reason,
            } => write!(f, "could not get page {page_number}: {reason}. Skipping."),
            Self::SavePage {
                page_number,
                reason,
            } => write!(f, "could not save page {page_number}: {reason}. Skipping."),
        }
    }
}

struct SavedPage {
    output_path: PathBuf,
    size_kb: u64,
}

struct OutputContext<'a> {
    output_dir: &'a Path,
    stem: &'a str,
    page_number: NonZeroU16,
    output_format: OutputFormat,
}

/// Extracts selected pages from a PDF into single-page PDF or PNG files.
pub fn extract_pages(
    pdfium: &Pdfium,
    pdf_path: &Path,
    selection: PageSelection,
    output_format: OutputFormat,
    output_folder: Option<&Path>,
) -> Result<ExtractionReport> {
    output_format.validate()?;

    let document = pdfium
        .load_pdf_from_file(pdf_path, None)
        .map_err(|source| PdfCutterError::OpenPdf {
            path: pdf_path.to_path_buf(),
            source,
        })?;

    let pages = document.pages();
    let total_pages = pages.len();

    if total_pages == 0 {
        return Err(PdfCutterError::EmptyDocument);
    }

    let resolved_selection = selection.resolve(total_pages);
    let output_dir = resolve_output_dir(output_folder)?;
    let stem = output_stem(pdf_path);
    let mut warnings = resolved_selection
        .out_of_bounds
        .into_iter()
        .map(|page_number| ExtractionWarning::PageOutOfBounds {
            page_number: page_number.get(),
            total_pages,
        })
        .collect::<Vec<_>>();
    let mut results = Vec::with_capacity(resolved_selection.page_numbers.len());

    for page_number in resolved_selection.page_numbers {
        let source_page_index = page_number.get() - 1;
        let page = match pages.get(source_page_index) {
            Ok(page) => page,
            Err(error) => {
                warnings.push(ExtractionWarning::LoadPage {
                    page_number: page_number.get(),
                    reason: error.to_string(),
                });
                continue;
            }
        };

        let width_mm = page.width().to_mm();
        let height_mm = page.height().to_mm();

        match save_page(
            pdfium,
            &document,
            &page,
            source_page_index,
            OutputContext {
                output_dir: &output_dir,
                stem: &stem,
                page_number,
                output_format,
            },
        ) {
            Ok(saved_page) => results.push(PageResult {
                output_path: saved_page.output_path,
                width_mm,
                height_mm,
                size_kb: saved_page.size_kb,
            }),
            Err(error) => warnings.push(ExtractionWarning::SavePage {
                page_number: page_number.get(),
                reason: error.to_string(),
            }),
        }
    }

    Ok(ExtractionReport {
        pages: results,
        warnings,
    })
}

fn save_page(
    pdfium: &Pdfium,
    source_document: &PdfDocument<'_>,
    source_page: &PdfPage<'_>,
    source_page_index: u16,
    output_context: OutputContext<'_>,
) -> Result<SavedPage> {
    match output_context.output_format {
        OutputFormat::Pdf => {
            save_pdf_page(pdfium, source_document, source_page_index, &output_context)
        }
        OutputFormat::Png { .. } => save_png_page(source_page, &output_context),
    }
}

fn save_pdf_page(
    pdfium: &Pdfium,
    source_document: &PdfDocument<'_>,
    source_page_index: u16,
    output_context: &OutputContext<'_>,
) -> Result<SavedPage> {
    let mut reserved_output = reserve_output_file(
        output_context.output_dir,
        output_context.stem,
        output_context.page_number,
        OutputFormat::Pdf,
    )?;
    let mut output_document =
        pdfium
            .create_new_pdf()
            .map_err(|source| PdfCutterError::CreateOutputPdf {
                path: reserved_output.path.clone(),
                source,
            })?;

    output_document
        .pages_mut()
        .copy_page_from_document(source_document, source_page_index, 0)
        .map_err(|source| PdfCutterError::CopyPage {
            page_number: source_page_index + 1,
            path: reserved_output.path.clone(),
            source,
        })?;

    output_document
        .save_to_writer(&mut reserved_output.file)
        .map_err(|source| PdfCutterError::SavePdf {
            path: reserved_output.path.clone(),
            source,
        })?;

    Ok(SavedPage {
        size_kb: file_size_kb(&reserved_output.file, &reserved_output.path)?,
        output_path: reserved_output.path,
    })
}

fn save_png_page(
    source_page: &PdfPage<'_>,
    output_context: &OutputContext<'_>,
) -> Result<SavedPage> {
    let mut reserved_output = reserve_output_file(
        output_context.output_dir,
        output_context.stem,
        output_context.page_number,
        output_context.output_format,
    )?;
    let OutputFormat::Png { scale } = output_context.output_format else {
        unreachable!("PNG pages must use PNG output format");
    };
    let bitmap = render_page_to_bitmap(source_page, scale)?;

    bitmap
        .as_image()
        .write_to(&mut reserved_output.file, image::ImageFormat::Png)
        .map_err(|source| PdfCutterError::SaveImage {
            format: "PNG",
            path: reserved_output.path.clone(),
            source,
        })?;

    Ok(SavedPage {
        size_kb: file_size_kb(&reserved_output.file, &reserved_output.path)?,
        output_path: reserved_output.path,
    })
}

fn render_page_to_bitmap<'page>(
    page: &'page PdfPage<'page>,
    scale: f32,
) -> Result<PdfBitmap<'page>> {
    let render_config = PdfRenderConfig::new()
        .scale_page_by_factor(scale)
        .set_clear_color(PdfColor::WHITE);

    page.render_with_config(&render_config)
        .map_err(|source| PdfCutterError::RenderPage { source })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdfium_runtime::create_pdfium;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use std::time::{SystemTime, UNIX_EPOCH};

    const PDF: &str = "test_assets/input.pdf";

    fn test_guard() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();

        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .expect("test lock should not be poisoned")
    }

    fn pdfium() -> Pdfium {
        create_pdfium().expect("failed to bind pdfium")
    }

    fn temp_output_dir(prefix: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after UNIX_EPOCH")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "pdf_cutter_{prefix}_{}_{}",
            std::process::id(),
            unique
        ));
        fs::create_dir_all(&dir).expect("temporary output dir should be created");
        dir
    }

    fn cleanup_dir(path: &Path) {
        let _ = fs::remove_dir_all(path);
    }

    fn nz(value: u16) -> NonZeroU16 {
        NonZeroU16::new(value).expect("test page numbers must be non-zero")
    }

    #[test]
    fn test_out_of_range_pages_are_reported_as_warnings() {
        let _guard = test_guard();
        let pdfium = pdfium();
        let report = extract_pages(
            &pdfium,
            Path::new(PDF),
            PageSelection::Specific(vec![nz(18)]),
            OutputFormat::Pdf,
            None,
        )
        .expect("out-of-range pages should be reported as warnings");

        assert!(report.pages.is_empty());
        assert_eq!(
            report.warnings,
            vec![ExtractionWarning::PageOutOfBounds {
                page_number: 18,
                total_pages: 17,
            }]
        );
    }

    #[test]
    fn test_valid_pdf_page_produces_file() {
        let _guard = test_guard();
        let pdfium = pdfium();
        let output_dir = temp_output_dir("valid_pdf");

        let report = extract_pages(
            &pdfium,
            Path::new(PDF),
            PageSelection::Specific(vec![nz(1)]),
            OutputFormat::Pdf,
            Some(&output_dir),
        )
        .expect("extraction should succeed");

        assert!(report.warnings.is_empty());
        assert_eq!(report.pages.len(), 1);

        let result = &report.pages[0];
        assert!(result.output_path.exists());
        assert_eq!(
            result.output_path.extension().and_then(|ext| ext.to_str()),
            Some("pdf")
        );
        assert!(result.width_mm > 0.0);
        assert!(result.height_mm > 0.0);
        assert!(result.size_kb > 0);
        let bytes = fs::read(&result.output_path).expect("failed to read output PDF");
        assert_eq!(&bytes[..5], b"%PDF-");

        cleanup_dir(&output_dir);
    }

    #[test]
    fn test_valid_png_page_produces_file() {
        let _guard = test_guard();
        let pdfium = pdfium();
        let output_dir = temp_output_dir("valid_png");

        let report = extract_pages(
            &pdfium,
            Path::new(PDF),
            PageSelection::Specific(vec![nz(1)]),
            OutputFormat::Png { scale: 2.0 },
            Some(&output_dir),
        )
        .expect("extraction should succeed");

        assert!(report.warnings.is_empty());
        assert_eq!(report.pages.len(), 1);

        let result = &report.pages[0];
        assert!(result.output_path.exists());
        assert_eq!(
            result.output_path.extension().and_then(|ext| ext.to_str()),
            Some("png")
        );
        assert!(result.width_mm > 0.0);
        assert!(result.height_mm > 0.0);
        assert!(result.size_kb > 0);
        let bytes = fs::read(&result.output_path).expect("failed to read output PNG");
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");

        cleanup_dir(&output_dir);
    }

    #[test]
    fn test_png_scale_changes_output_dimensions() {
        let _guard = test_guard();
        let pdfium = pdfium();
        let small_output_dir = temp_output_dir("png_scale_small");
        let large_output_dir = temp_output_dir("png_scale_large");

        let small_report = extract_pages(
            &pdfium,
            Path::new(PDF),
            PageSelection::Specific(vec![nz(1)]),
            OutputFormat::Png { scale: 1.0 },
            Some(&small_output_dir),
        )
        .expect("small-scale extraction should succeed");
        let large_report = extract_pages(
            &pdfium,
            Path::new(PDF),
            PageSelection::Specific(vec![nz(1)]),
            OutputFormat::Png { scale: 3.0 },
            Some(&large_output_dir),
        )
        .expect("large-scale extraction should succeed");

        let small_image =
            image::open(&small_report.pages[0].output_path).expect("small PNG should load");
        let large_image =
            image::open(&large_report.pages[0].output_path).expect("large PNG should load");

        assert!(small_report.warnings.is_empty());
        assert!(large_report.warnings.is_empty());
        assert_eq!(large_image.width(), small_image.width() * 3);
        assert_eq!(large_image.height(), small_image.height() * 3);

        cleanup_dir(&small_output_dir);
        cleanup_dir(&large_output_dir);
    }

    #[test]
    fn test_duplicate_pages_are_deduplicated() {
        let _guard = test_guard();
        let pdfium = pdfium();
        let output_dir = temp_output_dir("dedup");

        let report = extract_pages(
            &pdfium,
            Path::new(PDF),
            PageSelection::Specific(vec![nz(1), nz(1), nz(1)]),
            OutputFormat::Pdf,
            Some(&output_dir),
        )
        .expect("extraction should succeed");

        assert!(report.warnings.is_empty());
        assert_eq!(report.pages.len(), 1);

        cleanup_dir(&output_dir);
    }

    #[test]
    fn test_existing_output_file_is_not_overwritten() {
        let _guard = test_guard();
        let pdfium = pdfium();
        let output_dir = temp_output_dir("existing_output");
        let existing_output = output_dir.join("input_page_1.pdf");

        fs::write(&existing_output, b"sentinel").expect("existing output should be written");

        let report = extract_pages(
            &pdfium,
            Path::new(PDF),
            PageSelection::Specific(vec![nz(1)]),
            OutputFormat::Pdf,
            Some(&output_dir),
        )
        .expect("extraction should succeed");

        assert!(report.warnings.is_empty());
        assert_eq!(report.pages.len(), 1);
        assert_eq!(
            report.pages[0]
                .output_path
                .file_name()
                .and_then(|name| name.to_str()),
            Some("input_page_1_1.pdf")
        );
        assert_eq!(
            fs::read(&existing_output).expect("existing output should stay readable"),
            b"sentinel"
        );

        cleanup_dir(&output_dir);
    }
}
