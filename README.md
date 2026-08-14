# pdf_cutter

CLI tool for inspecting PDFs, extracting pages as single-page PDFs or PNGs, and extracting text.

Built as a quicker alternative to [Docling](https://github.com/docling-project/docling)
for turning PDFs into text and structured markdown: a single native binary over
PDFium, with no Python runtime to install.

## Build

`build.sh` creates a runtime bundle in `./dist` with:

- `pdf_cutter`
- the matching native PDFium library

Basic build:

```bash
./build.sh
```

Explicit platform:

```bash
./build.sh macos
./build.sh linux
./build.sh windows
```

Build release archives and checksum files in `./artifacts` too:

```bash
./build.sh --with-artifacts
```

Optional environment variables:

- `RELEASE_TAG`: overrides the archive version label
- `SKIP_BUILD=1`: skips the Cargo validation/build pipeline

The build script downloads the matching PDFium binary and places it next to the executable in `./dist`.

## Usage

Run the bundled binary from `./dist`:

```bash
./dist/pdf_cutter --input input.pdf --format pdf
```

Extract specific pages as separate PDFs:

```bash
./dist/pdf_cutter --input input.pdf --format pdf --pages 1,3,5
```

Use page ranges:

```bash
./dist/pdf_cutter --input input.pdf --format pdf --pages 1-3,5-6,9
./dist/pdf_cutter --input input.pdf --format pdf --page 1-3,5-6,9
```

Render selected pages as PNG files:

```bash
./dist/pdf_cutter --input input.pdf --pages 1-3 --format png --scale 2 --output out
```

Render PNG files with a required output width:

```bash
./dist/pdf_cutter --input input.pdf --pages 1-3 --format png --scale-width-px 1400 --output out
```

Extract structured text (spans + markdown) for all pages:

```bash
./dist/pdf_cutter --input input.pdf --extract-structured
```

Extract structured text in markdown-only mode:

```bash
./dist/pdf_cutter --input input.pdf --extract-structured --structured-format markdown
```

Extract structured text for specific pages:

```bash
./dist/pdf_cutter --input input.pdf --extract-structured --pages 1-3
```

Extract text for all pages into one file:

```bash
./dist/pdf_cutter --input input.pdf --extract --output out
```

Extract text for specific pages into separate files:

```bash
./dist/pdf_cutter --input input.pdf --extract --pages 1,3,5 --output out
```

Extract text for selected pages into one file:

```bash
./dist/pdf_cutter --input input.pdf --extract --pages 2-5,9 --concat --output out
```

Concatenate selected pages into one PDF:

```bash
./dist/pdf_cutter --input input.pdf --format pdf --pages 2-5,9 --concat
```

Print page count plus per-page metadata:

```bash
./dist/pdf_cutter --input input.pdf --page-count
./dist/pdf_cutter --input input.pdf -n
```

The CLI writes structured JSON to stdout. Human-readable warnings are written to stderr.

## CLI Notes

- `--input` / `-i`: source PDF file
- `--pages` / `-p`: page list or inclusive ranges like `1-3,5,9-10`
- `--page`: alias for `--pages`
- `--extract` / `-e`: extracts text into `.txt` file(s); `--pages all` and `--concat` produce one file
- `--extract-structured`: extracts per-page text spans (font size, position, bold/italic) and a markdown reconstruction as JSON to stdout; mutually exclusive with `--extract` and `--format`
- `--structured-format`: controls structured extraction output; `full` (default) includes spans + markdown, `markdown` emits markdown only; requires `--extract-structured`
- `--format` / `-f`: required for page cutting; `pdf` or `png`; not valid with `--extract`
- `--scale` / `-s`: optional for `--format png`; if neither sizing option is provided it defaults to `2.0`; not valid with `--extract`
- `--scale-width-px`: optional for `--format png`; sets the output PNG width in pixels and preserves aspect ratio; cannot be used together with `--scale`
- `--concat` / `-c`: merges selected pages into one PDF, or one text file when used with `--extract`
- `--output` / `-o`: output directory
- `--page-count` / `-n`: prints the total page count plus per-page `width_px`, `height_px`, `contains_images`, `image_count`, `image_coverage`, and `llm_recommended` metadata as JSON
- extracted text starts each page with a `____PageN____` marker
- structured markdown reconstructs the page in visual reading order (bands, multi-column layouts, and `/Rotate`d pages included), detects tables with wrapped cells merged into logical rows, renders bold/italic inline styles, repairs line-break hyphenation, and escapes markdown syntax characters in extracted text
- pages whose text layer is statistically garbled (e.g. broken font mappings) are skipped with a per-page warning in both text extraction modes
- page extraction JSON includes per-page `width_px`, `height_px`, and `contains_images`
- inspection and PDF extraction report page dimensions in pixels at render scale `1.0`
- PNG extraction reports the actual rendered output dimensions in pixels for the chosen `--scale` or `--scale-width-px`
- extraction JSON includes an `errors` object keyed by page number when pages are skipped or fail

Use either `--extract` for text extraction or `--format` for page cutting.

## Output Examples

Page inspection with `--page-count`:

```json
{
  "page_count": 2,
  "pages": {
    "1": {
      "width_px": 595,
      "height_px": 842,
      "contains_images": false,
      "image_count": 0,
      "image_coverage": 0.0,
      "llm_recommended": false
    },
    "2": {
      "width_px": 842,
      "height_px": 595,
      "contains_images": true,
      "image_count": 2,
      "image_coverage": 0.43,
      "llm_recommended": true,
      "llm_reasons": [
        {
          "reason": "image_heavy",
          "detail": "large image coverage; text inside embedded images is not extractable"
        }
      ]
    }
  }
}
```

`image_count` is the total number of image objects on the page, including images nested inside Form XObjects (reusable page fragments). `image_coverage` is the estimated fraction of the page area (0.0–1.0) covered by top-level image bounding boxes; images inside Form XObjects are excluded from coverage because their bounds are in the form's local coordinate space, not the page's.

`llm_recommended` is `true` when a vision-capable LLM or OCR pass is likely to recover content that text-layer extraction cannot; `llm_reasons` lists why. Reasons:

- `no_text_layer`: the page has images but (almost) no extractable text — likely a scanned page
- `image_heavy`: at least 20% of the page area is covered by embedded images, whose contents (diagram labels, figure text) are invisible to the text layer
- `unreadable_text`: the text layer exists but is statistically garbled (e.g. a broken CID font mapping); rendered pixels are the only reliable source
- `complex_table`: the page contains a table with wrapped or merged cells across three or more columns; all cell text is extracted, but its cell-to-column association is approximate and a vision pass can verify or reconstruct the exact structure (two-column wrapped tables such as footnote lists reconstruct reliably and are not flagged)

Use this field to route pages in a hybrid pipeline: pdf_cutter for deterministic, verbatim text and table extraction everywhere, and a vision-LLM pass only for the flagged pages (optionally rendered via `--format png`).

Per-page PDF or PNG extraction:

```json
{
  "pages": {
    "1": {
      "path": "/abs/path/out/input_page_1.pdf",
      "size_kb": 12,
      "width_px": 595,
      "height_px": 842,
      "contains_images": true
    }
  }
}
```

PNG extraction with scale also includes the render scale:

```json
{
  "scale": 2.0,
  "pages": {
    "1": {
      "path": "/abs/path/out/input_page_1.png",
      "size_kb": 184,
      "width_px": 1191,
      "height_px": 1684,
      "contains_images": true
    }
  }
}
```

PNG extraction with a requested width:

```json
{
  "scale_width_px": 1400,
  "pages": {
    "1": {
      "path": "/abs/path/out/input_page_1.png",
      "size_kb": 184,
      "width_px": 1400,
      "height_px": 1980,
      "contains_images": true
    }
  }
}
```

Concatenated PDF output:

```json
{
  "concat": {
    "path": "/abs/path/out/input_concat.pdf",
    "pages": "2-5,9",
    "size_kb": 96
  },
  "page_info": {
    "2": {
      "width_px": 595,
      "height_px": 842,
      "contains_images": false
    },
    "3": {
      "width_px": 595,
      "height_px": 842,
      "contains_images": true
    }
  }
}
```

Text extraction:

```json
{
  "pages": {
    "1": {
      "path": "/abs/path/out/input_page_1.txt",
      "size_kb": 4
    }
  }
}
```

Structured text extraction (`--extract-structured`, default `full` format):

```json
{
  "pages": [
    {
      "page": 1,
      "markdown": "# Title\n\nSome paragraph text.",
      "spans": [
        {
          "text": "Title",
          "font_size": 18.0,
          "font_name": "Helvetica-Bold",
          "bold": true,
          "italic": false,
          "x": 72.0,
          "y": 720.0,
          "width": 45.2,
          "height": 20.0
        }
      ]
    }
  ],
  "selected_page_count": 1
}
```

Structured text extraction with `--structured-format markdown` (no spans):

```json
{
  "pages": [
    {
      "page": 1,
      "markdown": "# Title\n\nSome paragraph text."
    }
  ],
  "selected_page_count": 1
}
```

If requested pages are skipped or fail, the JSON includes an `errors` object keyed by page number.

## Library Usage

The binary is a thin CLI over the `pdf_cutter` library crate, which exposes the same operations for embedding:

```rust
use pdf_cutter::{
    PageSelection, StructuredOutputFormat, create_pdfium, extract_structured_text,
    inspect_document,
};

let pdfium = create_pdfium()?;

// Route pages: text extraction everywhere, vision-LLM where recommended.
let info = inspect_document(&pdfium, "input.pdf".as_ref())?;
let llm_pages: Vec<u16> = info
    .pages
    .iter()
    .filter(|page| page.llm_recommended())
    .map(|page| page.page_number)
    .collect();

let report = extract_structured_text(
    &pdfium,
    "input.pdf".as_ref(),
    PageSelection::All,
    StructuredOutputFormat::MarkdownOnly,
)?;
```

Run `cargo doc --open` for the full API documentation. `create_pdfium()` binds the native PDFium library from the executable's directory, the system, `./`, or `./dist/` (in that order).
