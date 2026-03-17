use crate::error::{PdfCutterError, Result};
use pdfium_render::prelude::{PdfDocument, Pdfium};
use std::path::Path;

/// Returns the page count for a PDF.
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
