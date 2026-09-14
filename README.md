# pdf-toolkit

CLI tool for inspecting PDFs, extracting pages as single-page PDFs or PNGs,
extracting text, recovering CAD geometry as georeferenced GeoJSON,
annotating whole building-permit dossiers, and reconstructing the building
storey by storey from its plan sheets — with experimental hosted-model
passes that refine the CAD and reconstruct the building's massing.

Built as a quicker alternative to [Docling](https://github.com/docling-project/docling)
for turning PDFs into text and structured markdown: a single native binary over
PDFium, with no Python runtime to install.

Every command prints one JSON document on stdout.
**[`docs/json-api.md`](docs/json-api.md) is the contract** — field by field,
with the enums to branch on and when a field is absent. Read that if you are
wiring this into a service; the examples below are a tour, not a reference.

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
- `SKIP_TESTS=1`: keeps the checks, clippy and the release build but skips `cargo test` (what the release workflow sets)

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
  dossier     whole-dossier annotation: labels, routing, QR, entities, graph
  model       storey-by-storey 3D reconstruction from the plan sheets (deterministic)
  cadfix      model-proposed CAD refinement of one page   (experimental, networked)
  building    3D building massing from the drawings       (experimental, networked)
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
./dist/pdf-toolkit vector input.pdf --parcel parcel.geojson   # place table-less sheets from a known parcel polygon
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
The survey sheet's coordinates then reach further still: sheets that print no
table of their own — general plans, cadastral extracts, situation maps — draw
the *same parcel*, and matching that outline by shape against the placed
table's world polygon places them too (`georeference.source:
"document_table"`). On the corpus this turns 9 placed pages into 30; a sheet
that does not show the parcel finds no consistent transform and honestly
stays local.

### dossier

Assembles the whole-dossier annotation: per-page labels, text-extraction
routing, decoded QR payloads, entities, groups and a derived document graph,
with the `vector` classification and georeferencing folded in. Pages a binary
cannot read — scans, handwriting, mis-encoded text layers — carry a vision-model
work order instead of a guess; a second run merges what that pass returned.
The application form's fixed slots are read into a `request` block — the
action from the form's own closed list (build, rebuild, restore, strengthen,
change use), the object, use, secondary works, storeys, site address and
the parameters line — so a caller learns what is being asked without
reading prose.

```bash
./dist/pdf-toolkit dossier input.pdf                          # deterministic pass
./dist/pdf-toolkit dossier input.pdf --answers answers.json   # merge a reviewed vision pass
./dist/pdf-toolkit dossier input.pdf --vlm                    # vision pass, default model
./dist/pdf-toolkit dossier input.pdf --vlm openai/gpt-4o      # …or any other hosted model
./dist/pdf-toolkit dossier input.pdf --answers a.json -o out  # also write .geojson files
```

Bare `--vlm` uses `gemini-3.8-flash`, the corpus benchmark's winner
(`docs/annotation-findings.md`). It needs an API key in the environment
(`GEMINI_API_KEY` for `gemini-*` specs, `OPENROUTER_API_KEY` for
`vendor/model` specs, `OPENAI_API_KEY` for `gpt-*`; a `.env` is honoured —
see `.env.sample`) and sends the flagged pages' images to that provider.
Like `cadfix` and `building`, it is networked and always opt-in; nothing
else in the toolkit touches the network.

Every page and group also carries a `trust` — `official` for the Cadastre
Committee's own documents, `applicant`, `identity`, `unknown` — and the
report lists every figure the pages state about the parcel (`facts[]`: codes,
areas, registered buildings, owner, applicant, address, certificate number,
the survey polygons) and compares each repeated one across pages with the
official page as the reference (`checks[]`: agree, near miss, disagree,
stated once, unreadable). A mismatch names both pages and both values and is
never resolved by picking a side.

The output shape is `docs/annotation-schema.md`; the command contract is the
`dossier` section of `docs/json-api.md`.

### cadfix and building (experimental)

Both drive a hosted model — `gemini-3.8-flash` by default, the same as
`--vlm` (`--model` takes any spec `--vlm` takes; `GEMINI_API_KEY` for
`gemini-*`, `OPENAI_API_KEY` for `gpt-*`, see `.env.sample`) — and
both keep the toolkit's discipline: **the model proposes, extracted geometry
decides**. Coordinates never come from a model.

```bash
./dist/pdf-toolkit cadfix input.pdf --page 11 -o out      # layers, noise, ring closing
./dist/pdf-toolkit building input.pdf --answers a.json -o out
./dist/pdf-toolkit building input.pdf --parcel parcel.geojson -o out   # on the map without a survey table
```

`cadfix` shows the model the rendered sheet beside an inventory of the
extracted vector features; the model answers with parallel tool calls —
assign layers, drop plotting noise, close nearly-closed rings — and the
operations are applied deterministically to the exact geometry.

`building` reads the set's visualization section — development scheme,
facades with level marks, roof plan, indicator table, the 3D renders at the
end — and reconstructs every built structure: one part per house block of a
development, the perimeter fence as a thin wall ring, pitched roofs as
stepped insets, facade colours off the renders, plus a `facts` block with
the printed indicator numbers. Footprints are exact extracted rings from
the anchor sheet (georeferenced where it was placed; a printed scale like
`Մ 1:500` converts page units to metres). The result is map-native
extruded GeoJSON — `height`/`base`/`color` properties that Mapbox or
MapLibre `fill-extrusion` renders directly. Footprints are always
exact extracted CAD rings; when a floor plan draws a richer *closed*
exterior outline, the model names that sheet (`outline_page`) and the
toolkit extrudes the exact drawn outline, aligned onto the site position —
loose CAD polylines are stitched first, and an outline that fails the area
or alignment guard falls back to the site ring with a warning. Beside the
extrusions, `building` measures the site deterministically — parcel and
footprint side lengths, each building's setback to every parcel side,
building-to-building and building-to-fence clearances — as a second,
labelled GeoJSON layer (`measurements`) a map can toggle. Both commands are
networked and opt-in, like `--vlm`, and every hosted call logs its token
usage and estimated cost on stderr, priced from
`crates/llm-providers/models.json`.

### model

Reconstructs the building storey by storey from the CAD-exported plan
sheets — deterministically, from the fills the exporter wrote. Walls, the
building envelope and the rooms are told apart by fill colour, the storeys
are stacked on the printed grid axes, and the openings are what the envelope
minus the rooms minus the walls leaves behind: edge pieces are glazing,
interior pieces door gaps. Levels come from the section's level marks, the
scale from the title block.

```bash
./dist/pdf-toolkit model input.pdf -o out                  # model.json, .glb, .geojson, .params.json
./dist/pdf-toolkit model input.pdf --params out/model/input.params.json   # a reviewed parameter file
./dist/pdf-toolkit model input.pdf --answers answers.json  # the dossier's labels name the plan sheets
./dist/pdf-toolkit model input.pdf --llm-params            # gemini-3.8-flash confirms the parameters
./dist/pdf-toolkit model input.pdf --parcel parcel.geojson # place it from the site plan matched to the cadastre
./dist/pdf-toolkit model input.pdf -g 0,0,44.51,40.15 -g 10,0,44.5101,40.15   # place it by hand
```

The parameters — which sheets are the floor plans and the roof plan, the
plan scale, the storey levels, the fill colours, the grid labels — are
**detected from the document** (title blocks, printed scales, level marks,
the plan's own fills, the lettered grid bubbles); `params_notes` lists every
decision, and the `params` block is a valid `--params` file to review and
edit. `--answers` lends the dossier's page labels to sets whose title blocks
the classifier cannot read (most offices' exports), and detection then
falls back to the elevations' level marks for the storeys, to a sheet's
drawing centre when it prints no grid labels, and to the outline of walls
and rooms when it draws no envelope fill — each named in the notes.
`--llm-params` asks a hosted model to confirm or correct the parameters; the
proposal is checked against the drawings before it is used, and its
accepted `params` block can be kept and passed back as `--params`. Geometry
never comes from a parameter source.

The model is **placed on the map** when a georeferenced sibling sheet draws
the building's box (the site plan, placed from its table, a sibling's, or
`--parcel`): the ground-floor envelope is matched to it by shape, and the
sheet's other line work settles which way round it stands. On the reference
dossier that fit is 4 mm. Without such a sheet the model stays in
`local_metres` — right in shape, unknown in position — for a map to let a
reviewer put it down by hand.

Outputs: `model.json` (2D polygons with z ranges, metres about the grid
origin), a binary glTF for any 3D viewer, and the same map-native extruded
GeoJSON `building` emits. The shapes are the `model` section of
`docs/json-api.md`.

### structured

```bash
./dist/pdf-toolkit structured input.pdf
./dist/pdf-toolkit structured input.pdf --pages 1-3
./dist/pdf-toolkit structured input.pdf --structured-format markdown
```

## CLI Notes

- `--pages` / `-p` (alias `--page`): page list or inclusive ranges like
  `1-3,5,9-10`, or `all` (the default). Accepted by `cut`, `render`, `text`,
  `structured` and `vector`; the other commands always work on the whole
  document (`cadfix` names its single page with `--page`; `model` names its
  plan sheets through detection or `--params`).
- `--output` / `-o`: output directory. `cut`, `render` and `text` default to
  the current directory; `vector`, `dossier`, `cadfix`, `building` and
  `model` write their files only when it is given.
- `--model`: `cadfix` and `building` only; the hosted model spec, default
  `gemini-3.8-flash`. `--page`: `cadfix` only; the one page to refine.
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

A tour of the common shapes. **[`docs/json-api.md`](docs/json-api.md) is the
full contract** — every field, every enum value, and which fields are absent
rather than defaulted. `vector`'s result shape is documented there only, and `dossier`'s there and
in [`docs/annotation-schema.md`](docs/annotation-schema.md).

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

If requested pages are skipped or fail, the JSON includes an `errors` object
keyed by page number — the run still exits `0`. A *failed* run is different: it
exits non-zero and prints no JSON at all. See
[`docs/json-api.md`](docs/json-api.md#errors-and-partial-results).

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

The dossier pipeline is a library too: `pdf_dossier::analyze` reads a
document once (text, routing signals, QR, vector geometry) and
`pdf_dossier::assemble` turns that analysis plus options — an answers file,
an output directory — into the annotation, any number of times.
`pdf_vlm::VlmClient` renders and annotates the flagged pages against a hosted
model; `pages_awaiting_vision` / `seed_vision_reasons` are the seams the CLI
itself uses.

### Crate layout

The workspace is split so that the parts with reach beyond this CLI can be
depended on without it.

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
| `pdf-dossier` | the whole-dossier annotation for the `dossier` command |
| `pdf-vlm` | the opt-in hosted-vision passes: page annotation, CAD refinement, building massing |
| `llm-providers` | hosted-model transport: Gemini / OpenAI / OpenRouter, vision input, parallel tool calls |
| `pdf-toolkit` | the CLI, plus the facade that re-exports all of the above |

Nothing in `pdf-core` depends on another workspace crate. The rest form two
chains meeting at the CLI:

```
core -> {render, text} -> inspect -> pages ----+
geometry -> geo -> classify -> vector ---------+--> toolkit
{core, render, inspect, vector} -> dossier ----+
{dossier, vector, llm-providers} -> vlm -------+
```

`llm-providers` depends on no other workspace crate: it is transport, and
the only crate that opens a network connection.

**Two PDF parsers, deliberately.** `pdf-core` binds PDFium, which renders and
gives a good text layer but will not tell you what path a drawing is made of.
`pdf-geometry` interprets content streams with `lopdf`, which recovers exactly
that but renders nothing. Each does what the other cannot, so both stay.

### Test corpus

The `vector` and `dossier` end-to-end tests run against real building-permit
dossiers in `data/plan/`. The `dossier` tests additionally replay the reviewed
annotations and vision-pass answers beside them (`data/annotations/`,
`data/annotations/answers/`), and require the command to reproduce the
annotations exactly.

**Those dossiers are committed as the test data set** (`data/`), and they are
personal data in a private repository: never published, never copied
elsewhere, never quoted. Only unrelated files (`data/misc/`), run logs and the
regenerable per-page GeoJSON exports stay gitignored.

Everything that needs them skips itself when they are absent, printing `SKIP`,
so `cargo test` passes on a partial checkout; `PDF_TOOLKIT_REQUIRE_CORPUS=1`
turns a skip into a failure, so the only tests that check the georeferencing
is right cannot be silently dropped.

To enable them, copy the dossiers to `data/plan/` as `1.pdf`, `2.pdf`
and so on. The `vector` tests read the directory rather than a fixed list, so
adding a dossier needs no code change — but one that places a page fails
`the_corpus_places_exactly_the_recorded_pages` until that placement is recorded
in `PLACEMENTS`, which is deliberate: a new placement is a new claim about the
world and should be written down. The `dossier` parity tests are stricter
still: they replay the fourteen reviewed annotations by number, so a new
dossier joins them only once its annotation is reviewed into
`data/annotations/`.
