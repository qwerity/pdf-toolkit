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
    let pdfium = PDFIUM.get_or_init(bind_pdfium);

    PdfiumHandle {
        _lock: lock,
        pdfium,
    }
}

/// Binds the native library for an in-process test.
///
/// The library ships in the workspace's `dist/`, and cargo runs an integration
/// test with the working directory set to the *package* root, so the runtime's
/// own relative lookup cannot see it from here. Naming the directory is all
/// that is needed; how a library file is found inside it stays the runtime's
/// business.
fn bind_pdfium() -> Pdfium {
    let dist = workspace_root().join("dist");

    pdf_toolkit::create_pdfium_at(&dist).unwrap_or_else(|error| {
        panic!(
            "pdfium runtime must be available in {}: {error}",
            dist.display()
        )
    })
}

/// The test corpus root (`data/`): real building-permit submissions,
/// committed as the test data set (personal data; the repository is
/// private).
pub fn data_dir() -> std::path::PathBuf {
    workspace_root().join("data")
}

/// Whether the corpus files a test needs are present, and what to do when
/// they are not.
///
/// A partial checkout still gets a passing `cargo test`, with a loud SKIP
/// line naming what was skipped. A silent skip would drop the only tests
/// that check real-corpus behaviour, so `PDF_TOOLKIT_REQUIRE_CORPUS=1` turns
/// the skip into a failure — set it in CI.
///
/// Returns false when the caller should return without asserting.
#[must_use]
pub fn corpus_available(test: &str, required: &[std::path::PathBuf]) -> bool {
    let missing: Vec<String> = required
        .iter()
        .filter(|path| !path.is_file())
        .map(|path| path.display().to_string())
        .collect();
    if missing.is_empty() {
        return true;
    }
    assert!(
        std::env::var_os("PDF_TOOLKIT_REQUIRE_CORPUS").is_none(),
        "{test}: PDF_TOOLKIT_REQUIRE_CORPUS is set but the corpus is missing: {}",
        missing.join(", ")
    );
    eprintln!(
        "SKIP {test}: corpus files not present ({}); check out data/ to enable this test.",
        missing.join(", ")
    );
    false
}

/// The workspace root, two levels above this crate (`crates/pdf-toolkit`).
///
/// Shared with the CLI harness, which spawns the binary with this as its
/// working directory.
pub fn workspace_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crate must live at <workspace>/crates/<name>")
        .to_path_buf()
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

/// Writes a one-page fixture with body text and a free-text note typed onto
/// the page in the box `(left, bottom, right, top)`, as a cover stamped
/// "Adopted as Regulation" after it was made.
pub fn write_pdf_with_note(
    pdfium: &Pdfium,
    path: &Path,
    texts: Vec<Text>,
    note: &str,
    (left, bottom, right, top): (f32, f32, f32, f32),
) {
    write_pdf(pdfium, path, &[texts]);
    let mut document = pdfium
        .load_pdf_from_file(path, None)
        .expect("reopen fixture");
    {
        let mut page = document.pages_mut().get(0).expect("first page");
        let mut annotation = page
            .annotations_mut()
            .create_free_text_annotation(note)
            .expect("add note");
        annotation
            .set_bounds(PdfRect::new_from_values(bottom, left, top, right))
            .expect("place note");
    }
    let bytes = document.save_to_bytes().expect("save fixture PDF");
    std::fs::write(path, bytes).expect("write fixture PDF");
}

/// One unit of text that defeats the readability heuristic: vowel-less
/// uppercase words with heavy digit/symbol noise.
pub const GARBLED_UNIT: &str = "X9#Z 7Q$W 3K%T ";

/// Writes a one-page fixture whose text is statistically unreadable.
/// The text is split across lines so every character stays on the page:
/// pdfium drops characters positioned beyond the page edge.
pub fn write_garbled_pdf(pdfium: &Pdfium, path: &Path) {
    let garbled_line = GARBLED_UNIT.repeat(4);
    let garbled_lines: Vec<Text> = (0..4)
        .map(|row| Text::plain(72.0, 720.0 - row as f32 * 20.0, 12.0, &garbled_line))
        .collect();
    write_pdf(pdfium, path, &[garbled_lines]);
}

/// Writes a one-page fixture that mimics a rotated scan: every text object
/// is pre-rotated 90° counter-clockwise in page space and the page carries a
/// 90° clockwise `/Rotate` entry, so the text reads normally when displayed.
/// `Text.x`/`Text.y` are the unrotated page-space anchor of each run.
pub fn write_rotated_pdf(pdfium: &Pdfium, path: &Path, texts: &[Text]) {
    let mut document = pdfium.create_new_pdf().expect("create document");
    let font = document.fonts_mut().helvetica();
    let mut page = document
        .pages_mut()
        .create_page_at_end(PdfPagePaperSize::a4())
        .expect("create page");

    for item in texts {
        let mut object = page
            .objects_mut()
            .create_text_object(
                PdfPoints::new(0.0),
                PdfPoints::new(0.0),
                &item.text,
                font,
                PdfPoints::new(item.size),
            )
            .expect("add text object");
        object
            .rotate_counter_clockwise_degrees(90.0)
            .expect("rotate text object");
        object
            .translate(PdfPoints::new(item.x), PdfPoints::new(item.y))
            .expect("position text object");
    }

    page.set_rotation(PdfPageRenderRotation::Degrees90);
    document.save_to_file(path).expect("save fixture PDF");
}

/// Writes a one-page fixture PDF containing a single square image of
/// `size_pt` points per side (A4 page, so 200pt ≈ 8% coverage and
/// 400pt ≈ 32%).
pub fn write_pdf_with_image(pdfium: &Pdfium, path: &Path, size_pt: f32) {
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
    object.scale(size_pt, size_pt).expect("scale image");
    object
        .translate(PdfPoints::new(100.0), PdfPoints::new(400.0))
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
