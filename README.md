# pdf-toolkit

CLI tool for inspecting PDFs, extracting pages as single-page PDFs or PNGs, and extracting text.

Built as a quicker alternative to [Docling](https://github.com/docling-project/docling)
for turning PDFs into text and structured markdown: a single native binary over
PDFium, with no Python runtime to install.

## Build

`build.sh` creates a runtime bundle in `./dist` with:

- `pdf-toolkit`
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

One subcommand per operation. Every command takes the source PDF as its first
argument, and writes a single JSON document to stdout; warnings are written to
stderr, so stdout stays parseable.

```
pdf-toolkit <COMMAND> <INPUT> [OPTIONS]

  inspect     page count and per-page metadata
  cut         selected pages as single-page PDFs, or one merged PDF
  render      selected pages as PNG images
  text        plain text into .txt files
  structured  positioned spans and a markdown reconstruction, to stdout
  vector      CAD geometry + classification + georeferenced GeoJSON
```

### inspect

```bash
./dist/pdf-toolkit inspect input.pdf
```

### cut

```bash
./dist/pdf-toolkit cut input.pdf                              # every page
./dist/pdf-toolkit cut input.pdf --pages 1,3,5
./dist/pdf-toolkit cut input.pdf --pages 1-3,5-6,9 -o out
./dist/pdf-toolkit cut input.pdf --pages 2-5,9 --concat       # one merged PDF
```

### render

```bash
./dist/pdf-toolkit render input.pdf --pages 1-3 --scale 2 -o out
./dist/pdf-toolkit render input.pdf --pages 1-3 --scale-width-px 1400 -o out
```

With neither sizing option, `render` uses scale `2.0`.

### text

```bash
./dist/pdf-toolkit text input.pdf -o out                      # all pages, one file
./dist/pdf-toolkit text input.pdf --pages 1,3,5 -o out        # one file per page
./dist/pdf-toolkit text input.pdf --pages 2-5,9 --concat -o out
```

`text` writes one aggregate file whenever the whole document is selected (the
default) or `--concat` is given, and reports it under `concat`. A selection of
specific pages without `--concat` writes one file per page, reported under
`pages`.

### vector

Recovers the CAD geometry a drawing is actually made of — by interpreting the
PDF's content streams, not by tracing a rendering — classifies every page, and
writes GeoJSON.

```bash
./dist/pdf-toolkit vector input.pdf
./dist/pdf-toolkit vector input.pdf --pages 9
./dist/pdf-toolkit vector input.pdf --output out        # also write .geojson files
```

A survey sheet usually prints a table of boundary turning-point coordinates,
and that is enough on its own: the table is read from the text layer, the same
polygon is found in the drawing, and a transform is fitted between the two. The
residual comes out in **millimetres**, and that is the number that says the
placement is right — the fit is scored against the drawing, not against itself.

Where a sheet prints no such table, supply the georeference:

```bash
./dist/pdf-toolkit vector input.pdf --pages 10 \
  --georeference -565.87,-399.92,44.5100,40.1500 \
  --georeference  552.41, 399.92,44.5231,40.5620
```

Each `--georeference` is `local_x,local_y,longitude,latitude`: a point in the
drawing's own frame — the coordinates an unplaced page's GeoJSON is written in
— and where that point actually is. Two settle position, rotation and scale
together; one is refused, because it would leave rotation and scale free. A
page that can place itself from its own printed table ignores them.

**Every page yields GeoJSON**, placed or not. `crs` says which frame it is in:

| `crs` | meaning |
|---|---|
| `EPSG:4326` | placed; coordinates are longitude/latitude |
| `local_metres` | not placed, but the sheet's scale was readable, so shapes are metric |
| `page_units` | not placed and no scale found; shapes are right, size is not |

Placement is a **document-level** result, not a per-page one. A page that fits
to centimetres establishes which coordinate system the dossier is drawn in, and
its siblings are then retried against that system with a looser tolerance — so
running a whole document places pages that asking for one page alone does not.

### structured

```bash
./dist/pdf-toolkit structured input.pdf
./dist/pdf-toolkit structured input.pdf --pages 1-3
./dist/pdf-toolkit structured input.pdf --structured-format markdown
```

## CLI Notes

- `--pages` / `-p` (alias `--page`): page list or inclusive ranges like
  `1-3,5,9-10`, or `all` (the default). Accepted by every command except
  `inspect`, which always reports the whole document.
- `--output` / `-o`: output directory, for the commands that write files
  (`cut`, `render`, `text`). Defaults to the current directory.
- `--concat` / `-c`: `cut` merges the selection into one PDF; `text` merges it
  into one `.txt` file. `text` also merges when the selection is `all`, which is
  the default — so only an explicit page selection yields per-page files.
- `--scale` / `-s` and `--scale-width-px`: `render` only, and mutually
  exclusive. `--scale-width-px` sets the output width and preserves the aspect
  ratio.
- `--structured-format`: `structured` only; `full` (default) includes spans and
  markdown, `markdown` emits markdown only.
- extracted text starts each page with a `____PageN____` marker
- structured markdown reconstructs the page in visual reading order (bands,
  multi-column layouts, and `/Rotate`d pages included), detects tables with
  wrapped cells merged into logical rows, renders bold/italic inline styles,
  repairs line-break hyphenation, and escapes markdown syntax characters in
  extracted text
- pages whose text layer is statistically garbled (e.g. broken font mappings)
  are skipped with a per-page warning in both text extraction modes
- `inspect` and `cut` report page dimensions in pixels at render scale `1.0`;
  `render` reports the actual rendered output dimensions
- extraction JSON includes an `errors` object keyed by page number when pages
  are skipped or fail

## Output Examples

Page inspection with `inspect`:

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

Use this field to route pages in a hybrid pipeline: pdf-toolkit for deterministic, verbatim text and table extraction everywhere, and a vision-LLM pass only for the flagged pages (optionally rendered via `render`).

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

Structured text extraction (`structured`, default `full` format):

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

The binary is a thin CLI over the `pdf-toolkit` library crate, which re-exports the workspace's operations behind one import:

```rust
use pdf_toolkit::{
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

### Crate layout

The workspace is split so that the parts with reach beyond this CLI can be
depended on without it. `pdf-render` in particular exists to be consumed by
other Rust code that would otherwise shell out to Poppler's `pdftoppm` for
the same job.

| crate | owns |
|---|---|
| `pdf-core` | PDFium runtime, page addressing, output naming, errors, warnings, text-quality heuristics |
| `pdf-geometry` | the content-stream interpreter: real vector paths and text runs |
| `pdf-classify` | which pages carry a map-placeable drawing |
| `pdf-geo` | coordinate tables, transform fitting, projection, GeoJSON export |
| `pdf-vector` | ties those three together for the `vector` command |
| `pdf-render` | `PngSize` and rasterising a page to PNG |
| `pdf-text` | plain and structured text extraction |
| `pdf-inspect` | per-page metadata and the vision-pass routing signals |
| `pdf-pages` | cutting and concatenation |
| `pdf-toolkit` | the CLI, plus the facade that re-exports all of the above |

Nothing in `pdf-core` depends on another workspace crate. The rest form two
chains meeting at the CLI:

```
core -> {render, text} -> inspect -> pages ----+
geometry -> geo -> classify -> vector ---------+--> toolkit
```

**Two PDF parsers, deliberately.** `pdf-core` binds PDFium, which renders and
gives a good text layer but will not tell you what path a drawing is made of.
`pdf-geometry` interprets content streams with `lopdf`, which recovers exactly
that but renders nothing. Each does what the other cannot, so both stay.

### Test corpus

The `vector` end-to-end tests run against fourteen real building-permit
dossiers, 377 pages, in `test_assets/samples/`. They are ~198 MB and live in a
release rather than in git:

```bash
./scripts/fetch-samples.sh          # install
./scripts/fetch-samples.sh --check  # report only, exits non-zero when missing
```

Without them `cargo test` still passes and the corpus tests print `SKIP`. In CI
set `PDF_TOOLKIT_REQUIRE_CORPUS=1`, which turns that skip into a failure — a
silent skip would hide exactly the regression the corpus exists to catch.
