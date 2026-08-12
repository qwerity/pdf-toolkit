// Shared across test binaries; each binary uses only a subset of the helpers.
#![allow(dead_code)]

use pdfium_render::prelude::*;
use std::path::Path;

/// One positioned text run on a fixture page.
pub struct Text {
    pub x: f32,
    pub y: f32,
    pub size: f32,
    pub bold: bool,
    pub text: String,
}

impl Text {
    pub fn plain(x: f32, y: f32, size: f32, text: &str) -> Self {
        Self {
            x,
            y,
            size,
            bold: false,
            text: text.to_string(),
        }
    }

    pub fn bold(x: f32, y: f32, size: f32, text: &str) -> Self {
        Self {
            bold: true,
            ..Self::plain(x, y, size, text)
        }
    }
}

/// Serializing handle to the shared pdfium runtime.
///
/// Pdfium itself is not safe under interleaved multi-threaded use (parallel
/// tests segfault even with pdfium-render's `thread_safe` call marshalling),
/// so the handle holds a process-wide lock for the duration of each test.
/// Sharing one `Pdfium` instance also keeps a panicking test from poisoning
/// pdfium-render's global marshalling lock and cascading into other tests.
pub struct PdfiumHandle {
    _lock: std::sync::MutexGuard<'static, ()>,
    pdfium: &'static Pdfium,
}

impl std::ops::Deref for PdfiumHandle {
    type Target = Pdfium;

    fn deref(&self) -> &Pdfium {
        self.pdfium
    }
}

pub fn pdfium() -> PdfiumHandle {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    static PDFIUM: std::sync::OnceLock<Pdfium> = std::sync::OnceLock::new();

    let lock = LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let pdfium = PDFIUM.get_or_init(|| {
        pdf_cutter::create_pdfium().expect("pdfium runtime must be available (see ./dist)")
    });

    PdfiumHandle {
        _lock: lock,
        pdfium,
    }
}

/// Writes a fixture PDF with one A4 page per entry in `pages`.
pub fn write_pdf(pdfium: &Pdfium, path: &Path, pages: &[Vec<Text>]) {
    let mut document = pdfium.create_new_pdf().expect("create document");
    let regular = document.fonts_mut().helvetica();
    let bold = document.fonts_mut().helvetica_bold();

    for page_items in pages {
        let mut page = document
            .pages_mut()
            .create_page_at_end(PdfPagePaperSize::a4())
            .expect("create page");

        for item in page_items {
            let font = if item.bold { bold } else { regular };
            page.objects_mut()
                .create_text_object(
                    PdfPoints::new(item.x),
                    PdfPoints::new(item.y),
                    &item.text,
                    font,
                    PdfPoints::new(item.size),
                )
                .expect("add text object");
        }
    }

    document.save_to_file(path).expect("save fixture PDF");
}

/// Writes a fixture PDF with `page_count` pages of distinct body text.
pub fn write_simple_pdf(pdfium: &Pdfium, path: &Path, page_count: usize) {
    let pages: Vec<Vec<Text>> = (1..=page_count)
        .map(|number| {
            vec![Text::plain(
                72.0,
                720.0,
                12.0,
                &format!("Sample body text content for page number {number}."),
            )]
        })
        .collect();

    write_pdf(pdfium, path, &pages);
}

/// Writes a one-page fixture PDF containing a single 200x200pt image.
pub fn write_pdf_with_image(pdfium: &Pdfium, path: &Path) {
    let mut document = pdfium.create_new_pdf().expect("create document");
    let mut page = document
        .pages_mut()
        .create_page_at_end(PdfPagePaperSize::a4())
        .expect("create page");

    let bitmap = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
        4,
        4,
        image::Rgb([200, 30, 30]),
    ));
    let mut object = PdfPageImageObject::new(&document, &bitmap).expect("create image object");
    object.scale(200.0, 200.0).expect("scale image");
    object
        .translate(PdfPoints::new(100.0), PdfPoints::new(500.0))
        .expect("position image");
    page.objects_mut()
        .add_image_object(object)
        .expect("add image object");

    document.save_to_file(path).expect("save fixture PDF");
}

/// Builds a syntactically valid PDF with zero pages.
pub fn minimal_empty_pdf() -> Vec<u8> {
    let header = "%PDF-1.7\n";
    let object_1 = "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n";
    let object_2 = "2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n";
    let offset_1 = header.len();
    let offset_2 = offset_1 + object_1.len();
    let xref_offset = offset_2 + object_2.len();

    format!(
        "{header}{object_1}{object_2}xref\n0 3\n0000000000 65535 f \n\
         {offset_1:010} 00000 n \n{offset_2:010} 00000 n \ntrailer\n\
         << /Size 3 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n"
    )
    .into_bytes()
}
