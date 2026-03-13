use crate::error::{PdfCutterError, Result};
use crate::output::{
    OutputFormat, PageResult, file_size_kb, output_stem, reserve_named_output_file,
    reserve_output_file, resolve_output_dir,
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
    page_count: usize,
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
    let page_numbers = resolved_selection.page_numbers;

    if let OutputFormat::Pdf { concat: true } = output_format {
        let pages = save_concat_pdf(
            pdfium,
            &document,
            &page_numbers,
            &output_dir,
            &stem,
            &mut warnings,
        )?
        .into_iter()
        .map(|saved_page| PageResult {
            output_path: saved_page.output_path,
            page_count: saved_page.page_count,
            width_mm: 0.0,
            height_mm: 0.0,
            size_kb: saved_page.size_kb,
        })
        .collect();

        return Ok(ExtractionReport { pages, warnings });
    }

    let mut results = Vec::with_capacity(page_numbers.len());

    for page_number in page_numbers {
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
                page_count: saved_page.page_count,
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

fn save_concat_pdf(
    pdfium: &Pdfium,
    source_document: &PdfDocument<'_>,
    page_numbers: &[NonZeroU16],
    output_dir: &Path,
    stem: &str,
    warnings: &mut Vec<ExtractionWarning>,
) -> Result<Option<SavedPage>> {
    if page_numbers.is_empty() {
        return Ok(None);
    }

    let mut reserved_output = reserve_named_output_file(
        output_dir,
        &format!("{stem}_concat"),
        OutputFormat::Pdf { concat: true },
    )?;
    let mut output_document =
        pdfium
            .create_new_pdf()
            .map_err(|source| PdfCutterError::CreateOutputPdf {
                path: reserved_output.path.clone(),
                source,
            })?;
    let mut copied_pages = 0usize;

    for &page_number in page_numbers {
        let destination_page_index =
            u16::try_from(copied_pages).expect("copied page counts must fit in u16");

        match output_document.pages_mut().copy_page_from_document(
            source_document,
            page_number.get() - 1,
            destination_page_index,
        ) {
            Ok(()) => copied_pages += 1,
            Err(error) => warnings.push(ExtractionWarning::SavePage {
                page_number: page_number.get(),
                reason: error.to_string(),
            }),
        }
    }

    if copied_pages == 0 {
        drop(reserved_output.file);
        let _ = std::fs::remove_file(&reserved_output.path);
        return Ok(None);
    }

    output_document
        .save_to_writer(&mut reserved_output.file)
        .map_err(|source| PdfCutterError::SavePdf {
            path: reserved_output.path.clone(),
            source,
        })?;
    let size_kb = file_size_kb(&reserved_output.file, &reserved_output.path)?;

    Ok(Some(SavedPage {
        output_path: reserved_output.path,
        page_count: copied_pages,
        size_kb,
    }))
}

fn save_page(
    pdfium: &Pdfium,
    source_document: &PdfDocument<'_>,
    source_page: &PdfPage<'_>,
    source_page_index: u16,
    output_context: OutputContext<'_>,
) -> Result<SavedPage> {
    match output_context.output_format {
        OutputFormat::Pdf { .. } => {
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
        OutputFormat::Pdf { concat: false },
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
    let size_kb = file_size_kb(&reserved_output.file, &reserved_output.path)?;

    Ok(SavedPage {
        output_path: reserved_output.path,
        page_count: 1,
        size_kb,
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
    let size_kb = file_size_kb(&reserved_output.file, &reserved_output.path)?;

    Ok(SavedPage {
        output_path: reserved_output.path,
        page_count: 1,
        size_kb,
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
