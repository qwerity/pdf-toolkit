use crate::error::{PdfCutterError, Result};
use pdfium_render::prelude::{PdfDocument, Pdfium};
use std::path::Path;

/// Loads the input PDF and returns its total page count.
///
/// Parameters:
/// - `pdfium`: initialized PDFium runtime used to open the document.
/// - `pdf_path`: path to the source PDF file.
///
/// Returns:
/// - `Ok(u16)`: the total number of pages in the document. Empty PDFs return `0`.
///
/// # Examples
///
/// ```no_run
/// use pdf_cutter::{create_pdfium, document_page_count};
/// use std::path::Path;
///
/// let pdfium = create_pdfium()?;
/// let page_count = document_page_count(&pdfium, Path::new("input.pdf"))?;
/// println!("{page_count}");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn document_page_count(pdfium: &Pdfium, pdf_path: &Path) -> Result<u16> {
    Ok(open_document(pdfium, pdf_path)?.pages().len())
}

pub(crate) fn open_document<'pdfium>(
    pdfium: &'pdfium Pdfium,
    pdf_path: &Path,
) -> Result<PdfDocument<'pdfium>> {
    pdfium
        .load_pdf_from_file(pdf_path, None)
        .map_err(|source| PdfCutterError::OpenPdf {
            path: pdf_path.to_path_buf(),
            source,
        })
}
