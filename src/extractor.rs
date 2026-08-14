use crate::document::{
    destination_page_index, open_document, page_count_u16, page_dimensions_px, page_index,
    page_info_for_page,
};
use crate::error::{PdfCutterError, Result};
use crate::output::{
    OutputFormat, PageInfo, PageResult, PngSize, file_size_kb, output_stem,
    reserve_named_output_file, reserve_output_file, resolve_output_dir,
};
use crate::page_selection::PageSelection;
use pdfium_render::prelude::*;
use std::fmt;
use std::num::NonZeroU16;
use std::path::{Path, PathBuf};

pub struct ExtractionReport {
    pub pages: Vec<PageResult>,
    pub source_pages: Vec<PageInfo>,
    pub warnings: Vec<ExtractionWarning>,
}

#[derive(Debug, Eq, PartialEq)]
pub enum ExtractionWarning {
    PageOutOfBounds { page_number: u16, total_pages: u16 },
    LoadPage { page_number: u16, reason: String },
    UnreadableText { page_number: u16 },
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
            Self::UnreadableText { page_number } => {
                write!(
                    f,
                    "extracted text for page {page_number} appears corrupted and was skipped."
                )
            }
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

struct SavedConcatOutput {
    saved_page: SavedPage,
    source_pages: Vec<PageInfo>,
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
    // Validate up front so library callers that bypass the CLI's own
    // validation get a structured error instead of a panic at render time
    // (e.g. a PNG width beyond i32::MAX).
    output_format.validate()?;

    let document = open_document(pdfium, pdf_path)?;

    let pages = document.pages();
    let total_pages = page_count_u16(pages)?;

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
        let saved_output = save_concat_pdf(
            pdfium,
            &document,
            &page_numbers,
            &output_dir,
            &stem,
            &mut warnings,
        )?;

        let (pages, source_pages) = match saved_output {
            Some(saved_output) => (
                vec![PageResult {
                    page_number: None,
                    output_path: saved_output.saved_page.output_path,
                    page_count: saved_output.saved_page.page_count,
                    width_px: 0,
                    height_px: 0,
                    contains_images: false,
                    size_kb: saved_output.saved_page.size_kb,
                }],
                saved_output.source_pages,
            ),
            None => (Vec::new(), Vec::new()),
        };

        return Ok(ExtractionReport {
            pages,
            source_pages,
            warnings,
        });
    }

    let mut results = Vec::with_capacity(page_numbers.len());
    let mut source_pages = Vec::with_capacity(page_numbers.len());

    for page_number in page_numbers {
        let page = match pages.get(page_index(page_number)) {
            Ok(page) => page,
            Err(error) => {
                warnings.push(ExtractionWarning::LoadPage {
                    page_number: page_number.get(),
                    reason: error.to_string(),
                });
                continue;
            }
        };

        let page_info = page_info_for_page(page_number.get(), &page);
        let (width_px, height_px) = output_dimensions_px(&page, output_format);

        match save_page(
            pdfium,
            &document,
            &page,
            OutputContext {
                output_dir: &output_dir,
                stem: &stem,
                page_number,
                output_format,
            },
        ) {
            Ok(saved_page) => {
                source_pages.push(page_info.clone());
                results.push(PageResult {
                    page_number: Some(page_number.get()),
                    output_path: saved_page.output_path,
                    page_count: saved_page.page_count,
                    width_px,
                    height_px,
                    contains_images: page_info.contains_images,
                    size_kb: saved_page.size_kb,
                });
            }
            Err(error) => warnings.push(ExtractionWarning::SavePage {
                page_number: page_number.get(),
                reason: error.to_string(),
            }),
        }
    }

    Ok(ExtractionReport {
        pages: results,
        source_pages,
        warnings,
    })
}

fn output_dimensions_px(page: &PdfPage<'_>, output_format: OutputFormat) -> (u32, u32) {
    match output_format {
        OutputFormat::Pdf { .. } => page_dimensions_px(page, 1.0),
        OutputFormat::Png { size } => png_output_dimensions_px(page, size),
    }
}

fn png_output_dimensions_px(page: &PdfPage<'_>, size: PngSize) -> (u32, u32) {
    match size {
        PngSize::Scale(scale) => page_dimensions_px(page, scale),
        PngSize::WidthPx(width_px) => {
            let height_px =
                ((page.height().value * width_px as f32) / page.width().value).round() as u32;

            (width_px, height_px)
        }
    }
}

fn save_concat_pdf(
    pdfium: &Pdfium,
    source_document: &PdfDocument<'_>,
    page_numbers: &[NonZeroU16],
    output_dir: &Path,
    stem: &str,
    warnings: &mut Vec<ExtractionWarning>,
) -> Result<Option<SavedConcatOutput>> {
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
    // Bounded by the deduplicated `NonZeroU16` selection, so it fits in u16.
    let mut copied_pages: u16 = 0;
    let mut source_pages = Vec::with_capacity(page_numbers.len());
    let document_pages = source_document.pages();

    for &page_number in page_numbers {
        let source_page_index = page_index(page_number);

        match output_document.pages_mut().copy_page_from_document(
            source_document,
            source_page_index,
            destination_page_index(copied_pages),
        ) {
            Ok(()) => {
                copied_pages += 1;

                match document_pages.get(source_page_index) {
                    Ok(page) => source_pages.push(page_info_for_page(page_number.get(), &page)),
                    Err(error) => warnings.push(ExtractionWarning::LoadPage {
                        page_number: page_number.get(),
                        reason: error.to_string(),
                    }),
                }
            }
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

    Ok(Some(SavedConcatOutput {
        saved_page: SavedPage {
            output_path: reserved_output.path,
            page_count: usize::from(copied_pages),
            size_kb,
        },
        source_pages,
    }))
}

fn save_page(
    pdfium: &Pdfium,
    source_document: &PdfDocument<'_>,
    source_page: &PdfPage<'_>,
    output_context: OutputContext<'_>,
) -> Result<SavedPage> {
    match output_context.output_format {
        OutputFormat::Pdf { .. } => save_pdf_page(pdfium, source_document, &output_context),
        OutputFormat::Png { .. } => save_png_page(source_page, &output_context),
    }
}

fn save_pdf_page(
    pdfium: &Pdfium,
    source_document: &PdfDocument<'_>,
    output_context: &OutputContext<'_>,
) -> Result<SavedPage> {
    let page_number = output_context.page_number;
    let mut reserved_output = reserve_output_file(
        output_context.output_dir,
        output_context.stem,
        page_number,
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
        .copy_page_from_document(source_document, page_index(page_number), 0)
        .map_err(|source| PdfCutterError::CopyPage {
            page_number: page_number.get(),
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
    let OutputFormat::Png { size } = output_context.output_format else {
        unreachable!("PNG pages must use PNG output format");
    };
    let bitmap = render_page_to_bitmap(source_page, size)?;

    bitmap
        .as_image()
        .map_err(|source| PdfCutterError::RenderPage { source })?
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
    size: PngSize,
) -> Result<PdfBitmap<'page>> {
    let render_config = match size {
        PngSize::Scale(scale) => PdfRenderConfig::new().scale_page_by_factor(scale),
        PngSize::WidthPx(width_px) => PdfRenderConfig::new().set_target_width(
            i32::try_from(width_px)
                .expect("invariant: extract_pages validates width <= i32::MAX on entry"),
        ),
    }
    .set_clear_color(PdfColor::WHITE);

    page.render_with_config(&render_config)
        .map_err(|source| PdfCutterError::RenderPage { source })
}
