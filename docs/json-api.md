# pdf-toolkit JSON API

`pdf-toolkit` is a command-line tool. Every subcommand prints **exactly one JSON
document on stdout and nothing else**, so it can be driven from another program.

This file is the contract. It is the reference for integrating the tool; if you
are wiring it into a service, this is all you need.

- Version described here: **0.14.0** (matches the package version)
- Warnings and progress go to **stderr**; stdout is only ever the JSON document
- Files written under `--output` are the same whether or not you parse stdout
- Exit code is `0` on success and non-zero on failure. **A failed run prints no
  JSON at all** — the message is on stderr. Do not parse stdout without first
  checking the exit code.

Examples below are copied from real runs. Regenerate rather than hand-edit:

```bash
pdf-toolkit <command> … | jq
```

---

## Invocation

```
pdf-toolkit <command> <input.pdf> [options]
```

| Command | What it does | Writes files |
|---|---|---|
| `inspect` | Page count and per-page metadata | no |
| `cut` | Selected pages as single-page PDFs, or one merged PDF | yes |
| `render` | Selected pages as PNG images | yes |
| `text` | Plain text into `.txt` files | yes |
| `structured` | Positioned spans and a markdown reconstruction | no |
| `vector` | CAD geometry, classification and georeferenced GeoJSON | with `--output` |
| `dossier` | Whole-dossier annotation: labels, routing, QR payloads, entities, groups, graph | with `--output` |
| `cadfix` | *(experimental, networked)* Model-proposed CAD refinement of one page's vector geometry | with `--output` |
| `building` | *(experimental, networked)* 3D building massing read from the drawings, as extruded GeoJSON | with `--output` |
| `downsample` | A smaller copy of the PDF: raster images drawn above a resolution are resampled; text, fonts and vector content are carried through byte-identical | yes (`--output`, one PDF) |

### Options

| Option | Commands | Meaning |
|---|---|---|
| `--pages`, `-p` (alias `--page`) | `cut`, `render`, `text`, `structured`, `vector` | `all` (default), or a list/range like `1,3,5-7` |
| `--output`, `-o` | `cut`, `render`, `text`, `vector`, `dossier`, `cadfix`, `building` | Output directory. Defaults to the working directory (`dossier`, `cadfix`, `building`: no files unless given). |
| `--concat`, `-c` | `cut`, `text` | Merge the selection into one file |
| `--scale`, `-s` | `render` | Scale factor. Default `2.0`. |
| `--scale-width-px` | `render` | Exact output width in pixels; preserves aspect ratio |
| `--structured-format` | `structured` | `full` (default) or `markdown` |
| `--georeference`, `-g` | `vector` | `local_x,local_y,longitude,latitude`; repeatable |
| `--parcel` | `vector`, `dossier`, `building` | A GeoJSON file of WGS 84 parcel polygon(s) — the cadastre's statement of the boundary. Matched by shape to sheets that draw the parcel but print no coordinate table; see `georeference.source: "parcel_polygon"` |
| `--answers` | `dossier`, `building` | Vision-pass answers file (`dossier`: merged; `building`: labels sharpen sheet selection) |
| `--vlm` | `dossier` | In-process vision pass. Bare `--vlm` uses `gemini-3.8-flash`; see below |
| `--page` | `cadfix` | The one page holding the drawing to refine |
| `--model` | `cadfix`, `building` | Hosted model; defaults to `gemini-3.8-flash`, like `--vlm`. Same spec grammar. |
| `--output`, `-o` | `downsample` | **Required.** The PDF file to write (not a directory). Naming the input is refused. |
| `--dpi` | `downsample` | Resolution to resample to, pixels per displayed inch, `36`–`2400`. Default `200`. |
| `--threshold-dpi` | `downsample` | Only images drawn above this are resampled. Default 10 % above `--dpi` (`220` for `200`); below `--dpi` is refused. |
| `--quality` | `downsample` | JPEG quality `1`–`100` for images the PDF stores as JPEG. Default `85`. |

An option that does not apply to a command does not exist on it, and using it is
a usage error rather than a silently ignored flag.

---

## Errors and partial results

Two different things, and a caller must not confuse them.

**A failed run** — unreadable input, a rejected command line, a missing output
directory — exits non-zero and prints nothing on stdout. The reason is one line
on stderr, prefixed `Error:`.

**A skipped page** does not fail the run. The page is left out of the results,
a line is written to stderr prefixed `Warning:`, and — for `cut`, `render` and
`text` — an `errors` object appears in the JSON, keyed by page number:

```json
{
  "errors": {
    "99": "page 99 is out of bounds (1..=17). Skipping."
  },
  "pages": {
    "1": { "path": "/abs/out/input_page_1.txt", "size_kb": 3 }
  }
}
```

A run in which *every* page was skipped still exits `0` and still reports its
`errors`; it simply has no `pages`. Warnings reach stderr whether or not any
page succeeded.

`vector` reports unreadable pages in a top-level `warnings` array instead, and
omits the key when it is empty.

---

## `inspect`

```json
{
  "page_count": 17,
  "pages": {
    "1": {
      "width_px": 792,
      "height_px": 612,
      "contains_images": true,
      "image_count": 1,
      "image_coverage": 0.004133374895900488,
      "llm_recommended": true,
      "llm_reasons": [
        {
          "reason": "complex_table",
          "detail": "table with wrapped or merged cells; extracted cell-to-column association is approximate"
        }
      ]
    }
  }
}
```

| Field | Type | Notes |
|---|---|---|
| `page_count` | int | Pages in the document |
| `pages` | object | Keyed by one-based page number, as a **string** |
| `pages.*.width_px` / `height_px` | int | Page size in pixels at scale `1.0` |
| `pages.*.contains_images` | bool | True when `image_count > 0` |
| `pages.*.image_count` | int | Image objects, including inside Form XObjects |
| `pages.*.image_coverage` | float | Fraction of page area, `0.0`–`1.0`, top-level images only |
| `pages.*.llm_recommended` | bool | True when a vision/OCR pass would recover something |
| `pages.*.llm_reasons` | array | **Absent** when `llm_recommended` is false |

`image_coverage` excludes images inside Form XObjects because their bounds are
in the form's coordinate space, not the page's, so counting them would inflate
coverage arbitrarily.

### `llm_reasons[].reason`

Branch on this, not on `detail`.

| Value | Meaning |
|---|---|
| `no_text_layer` | The page has images but almost no extractable text — likely a scan |
| `image_heavy` | At least 20% of the page is covered by images, whose contents the text layer cannot see |
| `unreadable_text` | A text layer exists but is statistically garbled (e.g. a broken CID font map) |
| `complex_table` | A table with wrapped or merged cells across three or more columns; cell-to-column association is approximate |

This field is a routing signal: run deterministic extraction everywhere, and
spend a vision model only on the flagged pages.

---

## `cut`

One entry per page written:

```json
{
  "pages": {
    "1": {
      "path": "/abs/out/input_page_1.pdf",
      "size_kb": 126,
      "width_px": 792,
      "height_px": 612,
      "contains_images": true
    }
  }
}
```

With `--concat`, one merged file plus the metadata of every source page:

```json
{
  "concat": {
    "path": "/abs/out/input_concat.pdf",
    "pages": "1-2",
    "size_kb": 298
  },
  "page_info": {
    "1": {
      "width_px": 792, "height_px": 612, "contains_images": true,
      "image_count": 1, "image_coverage": 0.004133374895900488,
      "llm_recommended": true,
      "llm_reasons": [
        { "reason": "complex_table", "detail": "table with wrapped or merged cells; extracted cell-to-column association is approximate" }
      ]
    }
  }
}
```

`concat.pages` echoes the selection **as it was written on the command line**,
so `1-2` stays `1-2` rather than becoming `1,2`. `pages` and `concat` are
mutually exclusive: a concat run has no per-page map.

---

## `render`

Same per-page shape as `cut`, plus the sizing that was used — `scale` or
`scale_width_px`, never both:

```json
{
  "scale": 2.0,
  "pages": {
    "1": { "path": "/abs/out/input_page_1.png", "size_kb": 432, "width_px": 1584, "height_px": 1224, "contains_images": true }
  }
}
```

```json
{
  "scale_width_px": 1400,
  "pages": {
    "1": { "path": "/abs/out/input_page_1.png", "size_kb": 361, "width_px": 1400, "height_px": 1082, "contains_images": true }
  }
}
```

For `render`, `width_px`/`height_px` are the **rendered image's** dimensions.
For `inspect` and `cut` they are the page at scale `1.0`. A page is not an exact
whole number of points, so a rendered dimension is not always the reported one
multiplied by the scale.

---

## `text`

One file per page:

```json
{
  "pages": {
    "1": { "path": "/abs/out/input_page_1.txt", "size_kb": 3 },
    "2": { "path": "/abs/out/input_page_2.txt", "size_kb": 3 }
  }
}
```

One aggregate file — when `--concat` is given **or the selection is `all`,
which is the default**:

```json
{
  "concat": { "path": "/abs/out/input_concat.txt", "pages": "1-2", "size_kb": 5 }
}
```

Each page in an aggregate file begins with a `____PageN____` marker. Pages whose
text layer is statistically garbled are skipped and reported through `errors`
rather than emitted as plausible-looking nonsense.

---

## `structured`

```json
{
  "pages": [
    {
      "page": 1,
      "markdown": "## DEPARTMENT OF CITY PLANNING …",
      "spans": [
        {
          "text": "CP-7150 (1.20.2026)",
          "font_size": 9.960000038146973,
          "font_name": "ArialMT",
          "bold": false,
          "italic": false,
          "x": 36.0,
          "y": 44.099998474121094,
          "width": 93.30000305175781,
          "height": 13.600000381469727
        }
      ]
    }
  ],
  "selected_page_count": 1
}
```

| Field | Type | Notes |
|---|---|---|
| `pages` | **array** | Not an object keyed by page — unlike every other command |
| `pages[].page` | int | One-based, absolute in the document |
| `pages[].markdown` | string | Visual reading order: bands, columns, `/Rotate`d pages, tables |
| `pages[].spans` | array | **Absent** with `--structured-format markdown` |
| `selected_page_count` | int | Pages in the selection, not in the document |

Coordinates are PDF points with the origin bottom-left.

`x`, `y`, `width` and `height` are rounded to one decimal and `font_size` to
two — but they are serialised from 32-bit floats, so the JSON carries the
representation noise: `9.96` prints as `9.960000038146973` and `44.1` as
`44.099998474121094`. Compare with a tolerance, or round on your side; do not
test these for exact equality.

---

## `vector`

Recovers the geometry a CAD exporter wrote by interpreting content streams,
classifies each page, and georeferences what it can.

```json
{
  "document": "3",
  "source": "data/plan/3.pdf",
  "page_count": 34,
  "pages": [ { "…": "one entry per selected page" } ]
}
```

| Field | Type | Notes |
|---|---|---|
| `document` | string | Source file stem; also the `document` property on every feature |
| `source` | string | Path as given on the command line |
| `page_count` | int | Pages in the **document**, not in the selection |
| `pages` | array | One entry per selected page, in page order |
| `warnings` | array | Pages that could not be read at all. **Absent** when empty. |

### `pages[]`

| Field | Type | Notes |
|---|---|---|
| `page_number` | int | One-based |
| `classification` | object | What the page is |
| `georeference` | object | Where it is, or why it is nowhere |
| `crs` | string | The frame `geojson` is written in |
| `summary` | object | What the export kept and dropped |
| `geojson` | object | A GeoJSON `FeatureCollection`, inline |

**Every page yields `geojson`**, placed or not. `crs` says what the coordinates
mean:

| `crs` | Meaning |
|---|---|
| `EPSG:4326` | Placed. Coordinates are longitude/latitude. |
| `local_metres` | Not placed, but the sheet's printed scale was readable, so shapes are metric |
| `page_units` | Not placed and no scale found. Shapes are correct; size is not. |

### `classification`

```json
{
  "class": "topographic_survey",
  "confidence": "certain",
  "reason": "turning-point coordinate table present in the text layer",
  "is_drawing": true,
  "is_mappable": true,
  "features": {
    "path_count": 700, "text_count": 16, "image_count": 1,
    "image_coverage": 0.015521, "vector_coverage": 0.905522, "line_density": 31.8199,
    "distinct_colors": 4, "layer_count": 0, "cadastral_codes": 0,
    "declared_scales": [], "has_control_table": true, "control_rows": 5
  }
}
```

`class` is one of `topographic_survey`, `site_plan`, `cadastral_plan`,
`floor_plan`, `roof_plan`, `elevation_or_section`, `technical_detail`,
`scanned_document`, `scanned_drawing`, `text_document`, `unknown`. Branch on
this, not on `reason`.

`confidence` is `certain` or `ambiguous`. **`ambiguous` is the common case** —
structural rules settle the pages that matter and leave the rest open. In
particular `unknown` means "a drawing, but its type cannot be told from
structure alone"; that is the page a vision model would be asked about. It does
not prevent georeferencing: a page with a readable coordinate table places
regardless of how it is classified.

`is_drawing` is broader than `is_mappable` and answers a different question — a
floor plan is unmistakably a drawing, it is simply not a drawing of terrain.

`features` are the measurements the verdict was made from, reported so a caller
that disagrees can see the basis. Since 0.10.0 they include what the sheet's
**title block** says — the strongest evidence a drawing sheet carries about
itself, and what settles floor plans, roof plans, elevations and site plans
with `certain` confidence:

| Field | Type | Notes |
|---|---|---|
| `sheet_title` | string | The sheet name printed after the title-block label (`Անվանում :`, `Наименование:`, `Title:`), whitespace collapsed. **Absent** when the page prints no title block. |
| `sheet_role` | string | What the title names: `floor_plan`, `roof_plan`, `elevation`, `section`, `site_plan`, `situation_plan`, `fence`, `views3d`, `sheet_index`. **Absent** when there is no title or it names no known kind. |
| `sheet_level_m` | number | A level the title names (`+3,60 նիշով` → `3.6`), metres. **Absent** when none. |
| `level_marks` | int | Distinct level marks printed on the page (`+7,200`, `±0,000`, `-0,350`). A plan prints one, an elevation one per storey, eaves and parapet. |
| `filled_paths` | int | Painted fills, as opposed to strokes. A CAD plan is made of them. |

A titled drawing takes its class from `sheet_role` (`elevation` and `section`
both map to `elevation_or_section`, `situation_plan` to `cadastral_plan`,
`fence` to `technical_detail`; `views3d` and `sheet_index` decide nothing).
An untitled drawing with four or more level marks is reported as
`elevation_or_section` with `ambiguous` confidence.

### `georeference`

Placed from the sheet's own printed turning-point table:

```json
{
  "placed": true,
  "source": "text_layer",
  "crs": "EPSG:28408",
  "rmse_m": 0.003,
  "max_residual_m": 0.0038,
  "control_points_used": 5,
  "fitted_scale_denominator": 500.0,
  "center": [44.5914659, 39.9411836]
}
```

Placed from supplied `--georeference` references:

```json
{
  "placed": true,
  "source": "references",
  "crs": "EPSG:4326",
  "rmse_m": 0.0,
  "max_residual_m": 0.0,
  "control_points_used": 2,
  "rotation_degrees": 53.033,
  "center": [44.6619451, 40.1579272]
}
```

Not placed:

```json
{
  "placed": false,
  "failure": { "reason": "no_control_table" }
}
```

| Field | Type | Notes |
|---|---|---|
| `placed` | bool | The only field always present. Branch on it first. |
| `source` | enum | `text_layer`, `document_table`, `parcel_polygon` or `references`. Absent when not placed. |
| `failure.reason` | enum | Absent when placed. See below. |
| `crs` | string | The CRS the **fit** was made in — the exported coordinates are WGS 84 either way. Absent if it could not be identified. The Gauss-Krüger tables are read **without a Pulkovo datum shift**: the Yerevan cadastre publishes the same parcels that way, and the surveyor's turning points land on the city's parcel to the centimetre only under that reading (measured on two corpus parcels; the EPSG Pulkovo 1942 → WGS 84 shift put them 105 m off). |
| `rmse_m` | float | RMS residual over the control points, in metres |
| `max_residual_m` | float | Worst single residual, in metres |
| `control_points_used` | int | Points the fit was built on |
| `fitted_scale_denominator` | float | Sheet scale the fit implies. Absent for a reference placement. |
| `declared_scale_denominator` | float | Scale printed on the sheet, when readable. Often absent. |
| `rotation_degrees` | float | Rotation to true north, CCW. Only for a reference placement. |
| `center` | `[lon, lat]` | Centre of the placed geometry. Absent if the CRS could not be converted. |

`document_table` says the sheet printed no coordinates of its own: a
confidently placed sibling sheet's table was lent to it, and this sheet's
drawn parcel outline matched that table's world polygon by shape — under
the same RANSAC consensus, scale bounds and spread guards as any fit,
plus a stricter evidence gate (five or more matched corners, or a
complete small parcel at confident residual). The `rmse_m` is this
sheet's own fit; the `fitted_scale_denominator` is this sheet's own
drawing scale. `printed_dimensions` never appears with a lent table —
this sheet did not print the side lengths.

`parcel_polygon` says the sheet was placed from a polygon the caller
supplied with `--parcel` — typically the cadastre's own polygon for the
subject parcel — matched to the drawn outline exactly like a lent sibling
table, under a stricter gate: at least two thirds of the polygon's corners
must match at a residual of 0.1 m or better, because an external polygon is
tried against every unplaced page of the dossier and five corners of fifteen
at decimetre residual is what a facade's window grid produces. On the
corpus's dossier 1, which prints no coordinate table at all, the cadastre
polygon places five sheets with all fifteen corners at 6–9 mm (the 1:2000
extract at 51 mm) and leaves the table of contents and the elevations
unplaced. The parcel used appears in the page's collection as a
`kind: "reference_parcel"` polygon (`coordinate_source: "external:parcel"`),
distinct from a `survey_parcel`, which is the surveyor's own statement.

**`rmse_m` is the number that says a placement is right.** A survey sheet is
plotted from the same coordinates its table lists, so a correct fit agrees to
millimetres. It is not a completeness measure: it cannot catch a misread digit,
because the transform is fitted to the very numbers in question.

`failure.reason` is one of:

| Value | Meaning | Extra fields |
|---|---|---|
| `no_control_table` | No turning-point table was found in the text layer | — |
| `unknown_crs` | A table was found but its coordinates are in no recognised system | — |
| `no_marker_matches` | Table rows exist but no drawing marker matched them | `candidates` |
| `no_consistent_fit` | Markers were found but no transform explains them | `candidates`, `tolerance_m` |

**Placement is a document-level result.** A page that fits to centimetres fixes
the coordinate system for the whole dossier, and pages that failed are retried
against it with a looser tolerance. Running a whole document therefore places
pages that asking for one page alone does not — the same page can honestly
report `no_consistent_fit` alone and place in a full run.

### `summary`

```json
{
  "features": 58, "polygons": 1, "linestrings": 57,
  "dropped_out_of_roi": 0, "dropped_too_small": 0, "dropped_furniture": 723,
  "truncated": false
}
```

`features` equals the length of `geojson.features`. The `dropped_*` counts are
the sheet's border, title block, coordinate table, lettering and hatch stipple —
geometry that is on the page but is not terrain. `truncated` is true when the
export hit its feature cap.

### `geojson`

A standard `FeatureCollection` with a `metadata` foreign member.

Feature properties: `document`, `page`, `stroke` (source colour as `#rrggbb`),
`length_m`, and `layer` when the CAD export named one.

**A placed page's collection ends with the subject parcel itself**: one
`Polygon` feature with `kind: "survey_parcel"`, built from the turning-point
table the fit was made on rather than from the line work — so a caller can
draw the boundary, or measure it, without guessing which drawn ring it is.
Copied from a real run (`jq '.pages[0].geojson.features[-1].properties'`):

```json
{
  "area_m2": 512.40,
  "coordinate_source": "text_layer",
  "document": "2",
  "kind": "survey_parcel",
  "page": 11,
  "perimeter_m": 92.810,
  "turning_points": 10,
  "verified": true
}
```

`verified` is always `true` here — the ring is the very table the drawing
was fitted to. With a lent table (`georeference.source: "document_table"`)
the ring is the sibling sheet's survey, which is the parcel all the same.
It is counted in `summary.features` and `summary.polygons`. Unplaced pages
have no such feature: nothing on them says where the parcel is.

Placed pages carry the fit in `metadata`:

```json
{
  "document": "3", "page": 9,
  "source_crs": "EPSG:28408 (Pulkovo 1942 / Gauss-Kruger zone 8)",
  "source_epsg": 28408,
  "fit_rmse_m": 0.003, "fit_max_residual_m": 0.0038,
  "control_points_used": 5, "fitted_scale_denominator": 500.0
}
```

Unplaced pages carry the local frame and say so plainly:

```json
{
  "document": "7", "page": 10,
  "local_frame": {
    "coordinate_system": "local_metres", "scale_denominator": 2000.0,
    "scale_source": "declared", "units_per_page_unit": 0.7055,
    "origin_page": [841.58, 594.82], "extent": [1131.74, 799.85],
    "rotation": "unknown"
  },
  "warning": "Coordinates are in this drawing's own frame, not WGS 84. …"
}
```

`printed_dimensions` appears in `metadata` when the sheet prints its boundary
side lengths, comparing them against the coordinates — two independent
statements by the draughtsman, and the only check that catches a misread digit:

```json
{ "sides_printed": 5, "sides_agreeing": 3, "max_error_m": 22.99, "all_agree": false }
```

Sheets that print no side lengths report nothing rather than a vacuous pass.

---

## `dossier`

Assembles the **whole-dossier annotation**: one JSON describing every page's
labels, text-extraction routing, decoded QR payloads, entities, group
membership, per-page georeferencing (verbatim from `vector`), and a graph
derived from those facts. The full field reference is
[`annotation-schema.md`](annotation-schema.md); this section is the command's
contract.

The command is deterministic and offline, and it runs in **two phases**:

1. `dossier input.pdf` — the deterministic pass. Pages a binary cannot read
   (scans, handwriting, mis-encoded text layers) are *routed*, not guessed:
   each carries `extraction.vlm` — `{"required": true, "reasons": [...]}` — as
   a work order for a vision model. Which pages, and why, is the point:
   deterministic extraction is exact and free, a vision model is neither.
2. `dossier input.pdf --answers answers.json` — the same run, with the vision
   pass's answers merged. An answered page's semantic fields (labels, title,
   entities, groups, handwriting, …) are taken from the answer wholesale; the
   deterministic fields (text, routing signals, QR, georeferencing) are
   recomputed identically. The answers file format is in
   `annotation-schema.md` (`crates/pdf-dossier/src/answers.rs` is the parser).

The vision pass can also run **in-process**: `--vlm [model]` renders each
flagged page and asks a hosted vision model, then merges exactly as an
answers file would. Bare `--vlm` uses the corpus benchmark's winner,
`gemini-3.8-flash` (`annotation-findings.md`); a `.env` in the working
directory is honoured. `--answers` and `--vlm` combine:
pages the file answers are not re-asked. A page whose vision call fails —
an unparseable reply, an exhausted retry — degrades to its deterministic
facts with a `Warning:` line and keeps its standing `vlm` work order;
already-answered pages are kept, and the run still prints its JSON.
**The hosted-model surfaces —
`--vlm` here, and the `cadfix` and `building` commands — are the toolkit's
only networked paths.** Each sends page images (in this domain: identity
documents) to the configured provider, so each is opt-in per invocation and
never happens as a side effect of another command. The model spec's shape
picks the provider: `gemini-*` uses the Gemini API (`GEMINI_API_KEY`, with
`GOOGLE_API_KEY` accepted as a fallback), `gpt-*` uses the OpenAI API
(`OPENAI_API_KEY`), and a `vendor/model` id uses OpenRouter
(`OPENROUTER_API_KEY`); `.env.sample` at the repo root names them all.
Every hosted call logs its token usage and estimated cost to stderr
(`llm: <model>: N in (M cached) + K out ≈ $x`), priced from
`crates/llm-providers/models.json` — update that file when a provider
changes its rates. Model quality on the annotation task is compared in
`annotation-findings.md`.

```json
{
  "document": "letter",
  "entities": [],
  "generated_by": "pdf-toolkit 0.14.0 dossier: deterministic extraction (pdfium text, templates, rxing QR, vector classification and georeferencing); vision-pass answers pending",
  "graph": {
    "edges": []
  },
  "groups": [],
  "page_count": 1,
  "pages": [
    {
      "classification": {
        "class": "unknown",
        "confidence": "ambiguous",
        "is_drawing": false,
        "is_mappable": false
      },
      "content_kind": "digital",
      "extraction": {
        "chars": 62,
        "engine": "pdfium",
        "route": "embedded_text",
        "text": "A short letter about nothing in particular, set in plain type."
      },
      "labels": [
        "text_document"
      ],
      "languages": [
        "en"
      ],
      "page_number": 1,
      "trust": "applicant"
    }
  ],
  "schema_version": "0.4.0",
  "source": "letter.pdf",
  "stats": {
    "embedded_text": 1,
    "handwriting_pages": 0,
    "none": 0,
    "placed_pages": 0,
    "qr_pages": 0,
    "vlm": 0,
    "vlm_flagged": 0
  }
}
```

| Field | Notes |
|---|---|
| `schema_version` | Version of the annotation schema, independent of the tool version |
| `document` / `source` / `page_count` | As in `vector` |
| `generated_by` | Names the passes; says whether answers were merged or are pending |
| `stats` | Per-dossier counts: route totals, `vlm_flagged`, `qr_pages`, `handwriting_pages`, `placed_pages` |
| `request` | What the application form asks for — see below. Absent when no embedded-text application form states a request. |
| `facts` / `checks` | Every figure the dossier states about its parcel, and each repeated figure compared across the pages that state it — see *`facts` and `checks`* below. Absent when nothing was read / nothing could be compared. |
| `pages` | Array, one entry per page — field tables in `annotation-schema.md` |
| `groups` / `entities` / `graph` | Document sections, the identifier registry, and the derived edges |
| `warnings` | Pages whose geometry extraction failed, verbatim from the vector pass; also printed as `Warning:` stderr lines. Absent when none — a page listed here has no classification/`cad`, and this is what says why. |
| `llm_usage` | Present with `--vlm`: one entry per page the vision pass sent, with tokens and estimated cost — see *Token usage* below. Absent when no model was called. |
| `answers_file` | With `--vlm` **and** `--output`: `answers/<document>.json`, relative to the output directory — the merged vision-pass answers in the `--answers` format, so a following `building --answers` reuses them instead of asking again. Absent otherwise. |

Branch on these enums, not on prose:

- `pages[].extraction.route` — `embedded_text` \| `vlm` \| `none`
- `pages[].extraction.encoding_issue` — present (and `true`) only when a text
  layer exists but is wrong: a legacy 8-bit font encoding, CID glyph outlines,
  or a custom glyph mapping. Such pages keep `route: "embedded_text"` (the raw
  layer may still be mapped deterministically) but are VLM-flagged with reason
  `legacy_encoding`. A CID-garbled layer that extraction refused has no `text`
  and routes `vlm`.
- `pages[].qr_codes[].data` — the payload, or `null` with a `note` when a code
  is visible (per the vision pass) but undecodable. QR decoding is two-tier:
  a 1400 px pass over every page, and a 3000 px retry only for scans and
  pages whose text names e-cadastre.
- `pages[].extraction.vlm.reasons` — an **open** list
  (`scanned_or_unreadable_text`, `legacy_encoding`, `handwritten_fields`, …);
  treat unknown values as "needs a vision pass", never as an error.
- `groups[].georeference.status` — `applied` \| `same_site_unverified` \|
  `none`. `applied` only where a placed fit is in that group's own drawing
  frame; a floor plan of the same building is `same_site_unverified` —
  geometry decides, never proximity.
- `groups[].access[]` — on `ownership` and `administrative` groups, the
  credentials the Committee's document prints on itself: `{"kind":
  "certificate" \| "reference", "number", "passcode", "page"}`. The number and
  passcode («գաղտնաբառ») are verbatim from the certificate's or reference
  letter's header, and together they are what the register asks for before it
  will show its own copy. The toolkit reads them and stops there: it builds no
  links and fetches nothing, so what to do with the pair is the caller's.
- `pages[].trust` / `groups[].trust` — `official` \| `applicant` \|
  `identity` \| `unknown`. Official pages are the Cadastre Committee's own
  documents (certificate, reference letter, fee table, map extract): the
  record of what the parcel is. Everything the applicant or their designer
  wrote is `applicant`: a claim about it. `unknown` is a page nothing has
  read yet.
- `checks[].status` — `agree` \| `near_miss` \| `disagree` \|
  `single_source` \| `unreadable`.

Internal-consistency problems (a dangling group or entity reference, an
out-of-range related page) are reported as `Warning:` lines on stderr, per
the toolkit-wide rule; the JSON on stdout is emitted either way.

### `request`

The Ձև N 1-1 application form has one fixed slot for what is asked, filled
from a closed list the form itself prints (կառուցում, վերակառուցում,
վերականգնում, ուժեղացում, գործառնական նշանակության փոփոխում), so the
**action is a lookup, not a reading**. The object description, site address
and the «1. Նախատեսում եմ» parameters line are carried verbatim; `use`,
`works` and `storeys` are keyword facts over those strings. Copied from a
real run on a synthetic form (`jq .request`):

```json
{
  "action": "new_construction",
  "address": "Երևան, Օրինակ վարչական շրջան Նմուշի 1-ին փողոց 2/3 հողամաս",
  "object": "Երկհարկանի բնակելի տուն և պարիսպ",
  "page": 1,
  "planned": "հիմնական շենք 10,00x8,00 մ, 160,00 քմ, պարիսպ 0,2x40 մ",
  "storeys": 2,
  "use": "residential",
  "use_page": 1,
  "via": "embedded_text",
  "works": [
    "fence"
  ]
}
```

| Field | Notes |
|---|---|
| `page` / `via` | The application-form page read, and how (`embedded_text`; the block is deterministic) |
| `action` | `new_construction` \| `reconstruction` \| `restoration` \| `strengthening` \| `change_of_use` \| `other` — the form's own list; `other` when the slot holds something outside it (the verbatim text is then in `object`) |
| `object` | The description filled in beside the action, verbatim. Absent when the applicant wrote none. |
| `use` | `residential` \| `commercial` \| `industrial` \| `mixed`. **Absent when no page names one** — an auxiliary annex says nothing about use, and absence is the honest answer. |
| `use_page` | The page whose text named the use: the form itself, else the first other embedded-text page (an album title block). Cadastre certificates and references are never consulted — their «բնակելի կառուցապատման» is the land's designation, not the request. Present with `use`. |
| `works` | Secondary works the request names: `demolition`, `annex`, `extension`, `fence`, `garage`, `basement`, `attic`, `canopy`, `summer_kitchen`, `replanning`. «առանց նկուղի» (without a basement) is honoured. Absent when empty. |
| `storeys` | Storey count the request states, as a digit («2 հարկից») or an Armenian compound («երկհարկանի», «միահարկ»). Absent when unstated. |
| `address` | The site address as the form states it, with the cadastral code removed — the code is an entity of its own (`entities[]`, and the subject parcel) |
| `planned` | The «1. Նախատեսում եմ» line verbatim: dimensions, areas, whatever was typed. Absent when empty. |

A scanned application form is routed to the vision pass like any scan and
yields no `request` in this version; the block is deterministic by design.

### `facts` and `checks`

A dossier states the same figures several times by several hands: the
certificate prints the parcel's area in hectares, the explanatory note
repeats it in square metres, the architect's general plan carries the
turning-point table again, the application form quotes the fence's area and
the note quotes another. `facts[]` lists every such figure with the page it
was read from and that page's trust; `checks[]` compares each figure across
the pages, the official page as the reference where there is one, the
applicant's copies checked against it, and the applicant's own copies
against each other where there is none. **A mismatch is reported, never
resolved**: the check names both pages and both values and says how far
apart they are. Copied from a real run (`jq '.checks[] | select(.kind ==
"planned_area")'`):

```json
{
  "kind": "planned_area",
  "subject": "fence",
  "status": "disagree",
  "reference": {
    "page": 1, "trust": "applicant", "printed": "3.0 քմ", "number": 3.0,
    "status": "agree", "delta": 0.0
  },
  "witnesses": [
    { "page": 8, "trust": "applicant", "printed": "5,20 քմ", "number": 5.2,
      "status": "disagree", "delta": 2.2 }
  ],
  "note": "fence area on page 1 is 3.0 քմ; page 8 says 5,20 քմ (+2.20)"
}
```

`facts[]` entries: `{"kind", "number"?, "text"?, "printed", "page", "trust",
"via", "subject"?, "about"?}`. `kind` is one of `cadastral_code`,
`parcel_area`, `footprint_area`, `building_area`, `planned_area`, `owner`,
`applicant`, `address`, `certificate_number`, `survey_area`,
`survey_perimeter`, `turning_points`. `number` is the figure in canonical
units (square metres, metres, a count); `text` the canonical form of a code,
a person entity id or an address key; `printed` is verbatim. `subject` says
what the fact is about where the kind needs it — a building's code for
`building_area`, `fence` \| `building` for `planned_area`, `map` \| `mention`
\| `partial` for a cadastral code a map labels a parcel with, a note names
in passing, or a fee table prints by its last two groups. `about` is the
parcel an official page is about (a neighbour's certificate states the
neighbour's owner and area, and is never compared as the subject's).

Reading is deterministic: the certificate's fixed labels («Մակերեսի չափը
(հա)», section 5's buildings table), the form's «Նախատեսում եմ» line, the
reference letter's «Դիմող», and a small vocabulary of context words on the
same line before a printed area on every other page (հողամաս → parcel,
կառուցապատման → footprint, ցանկապատ / պարիսպ → fence); a figure whose
context is not recognised is not a fact. Legacy 8-bit Armenian fonts (the
fee table, some extracts, many title blocks) are decoded for this reading
alone. A vision answer may add facts for a scanned page (`facts[]` in the
answers file, kinds as above, `unit` `ha` \| `m2` for areas); they merge
with `via: "vlm"`.

`checks[]` entries: `{"kind", "subject"?, "status", "reference"?,
"witnesses", "note"}`, each witness `{"page", "trust", "printed", "number"?,
"text"?, "status", "delta"?}`.

| `kind` | Compares |
|---|---|
| `cadastral_code` | Every code an applicant page declares as the site's (the form, a title block, a code introduced by «ծածկագիր») against the subject parcel's, and a fee table's partial code by its tail. Codes the official pages name (neighbours) are allowed; any other is `disagree`; one or two digits off the subject's is `near_miss`. Codes a map labels its parcels with, or a note mentions, are never findings. |
| `parcel_area` | The certificate's hectares against every other statement of the plot's area, the turning-point tables' polygons included. |
| `building_area` | A registered building's area (section 5) against a page quoting it by code or by its `-001` suffix. |
| `planned_area` (`fence`) | The fence's area on the application form against every other page that states one; a page agrees when any of its fence figures does. |
| `applicant` | Whoever applies, commissions, or asked the Committee for the map, against the registered owner(s). |
| `address` | The site address on the certificate against the form's and the notes' — the same numbers and a street word in common. |
| `certificate_number` | The certificate's number against every citation of it. |
| `certificate_access` | Whether every page labelled as an ownership certificate prints the number and passcode that identify it to the register (`groups[].access`); a page without them is an `unreadable` witness, and a dossier with no certificate page at all is one `unreadable` check with no witnesses — the absence is the finding. |
| `official_readability` | `unreadable` with the pages nothing has read yet, so a caller knows the vision pass is still owed before the official figures are settled. |

Tolerances: an area copy agrees within 0.2 % (or half a square metre when
printed without decimals — the note's `350 քմ` against the certificate's
`0.03504 հա`), is `near_miss` within 2 %, `disagree` beyond. A code or a
certificate number one or two edits away is `near_miss`. `single_source`
means the figure appears once and nothing could be compared with it.

With `--output`, each drawing page's GeoJSON — and every **placed** page's,
whatever it is labelled — is written under
`<dir>/geojson/<document>/<document>_pNN.geojson` and referenced from
`pages[].cad.geojson` by that relative path; without it, `cad` still reports
`crs` and `features` but no path. An absent field is not a false one, here as
everywhere: `qr_codes`, `handwriting`, `georeference`, `entities`, `groups`
appear only when they have content.

---

## `cadfix` *(experimental)*

Model-proposed refinement of one page's recovered CAD geometry. The page is
rendered and extracted (`vector`'s pipeline), and the configured model — one
of the **networked, opt-in** surfaces above — proposes operations against an
inventory of the extracted features through parallel tool calls: assign
features to semantic layers, drop plotting noise, close nearly-closed rings.
The operations are applied deterministically; **geometry never comes from
the model**, and a proposal referencing a feature that does not exist
degrades to a `Warning:` stderr line and a `warnings` entry, not a change.

```
pdf-toolkit cadfix input.pdf --page 11 [--model gemini-3.8-flash] [-o dir]
```

Copied from a real run (`jq 'del(.geojson)'` — the `geojson` field carries
the full refined FeatureCollection inline, `vector`-style):

```json
{
  "closed": 0,
  "crs": "EPSG:4326",
  "document": "2",
  "dropped": 0,
  "features_after": 99,
  "features_before": 99,
  "geojson_file": "2_p11.fixed.geojson",
  "layers": {
    "building": 47,
    "parcel_boundary": 47
  },
  "model": "gpt-6-astra",
  "page": 11,
  "placed": true,
  "source": "data/plan/2.pdf"
}
```

| Field | Notes |
|---|---|
| `crs` / `placed` | Verbatim from `vector` for that page |
| `features_before` / `features_after` | Counts around the applied drops |
| `layers` | Features assigned a `properties.layer`, per layer name |
| `closed` / `dropped` | Rings closed, features removed |
| `warnings` | Proposals that could not be applied; absent when none |
| `geojson_file` | Present with `--output`: `<document>_pNN.fixed.geojson`, relative to that directory |
| `llm_usage` | The refinement call's tokens and estimated cost — see *Token usage* below |
| `geojson` | The refined FeatureCollection, inline |

## `building` *(experimental)*

Reconstructs, as map-native extruded GeoJSON, what the permit
says the finished site must look like — every house block of a development,
the perimeter fence, roof forms, facade colours. The drawing set's
**visualization section** (development scheme, facades and sections with
level marks, roof plan, indicator table, 3D renders — usually the last
sheets) is what the model reads; the **anchor sheet** — among placed pages
the sheet drawn at 1:1000 or finer that holds the most structure-sized
closed outlines, a plan-view label (site plan, general plan, cadastral
plan, over a floor plan whose rings are rooms) and then confidence of fit
as tie-breaks; else the plan-view sheet richest in outlines. Outlines that
are the parcel boundary do not count. A placed sheet holding no building
outline at all while an unplaced plan sheet holds some yields the anchor to
the latter — a reconstruction in the sheet's own metres over a georeferenced
refusal. Outlines drawn as loose polylines are stitched into rings first —
supplies footprint candidates as exact extracted geometry. One request,
parallel tool calls: `add_part` per structure, `set_facts` once for the
printed indicator numbers. The model reads numbers and colours; it never
draws.

```
pdf-toolkit building input.pdf [--answers a.json] [--parcel parcel.geojson] [--model gemini-3.8-flash] [-o dir]
```

`--parcel` is what puts a reconstruction on the map for a dossier that prints
no coordinate table: the anchor sheet is placed from the supplied polygon
(`georeference.source: "parcel_polygon"`), the footprints come out in WGS 84,
and the measurements are taken against that polygon (`parcel.source:
"reference_parcel"`). Without it such a dossier reconstructs in
`local_metres` — right in shape, unknown in position.

`--answers` labels (`elevation_or_section`, `floor_plan`, `general_plan`,
`photo_montage`, `3d_view`, …) sharpen which sheets are shown; without them
the `vector` classification chooses and the render sheets are simply left
out with a `warnings` entry — pages are never uploaded on a guess, because
dossiers routinely end in identity-document scans. Copied from a real run (`jq 'del(.geojson)'` — the
`geojson` field carries the full extruded FeatureCollection inline):

```json
{
  "anchor_page": 11,
  "crs": "local_metres",
  "document": "1",
  "facts": {
    "anchor_scale_denominator": 500,
    "buildings": 3,
    "floors": 2,
    "footprint_area_m2": 63.7,
    "height_m": 8,
    "notes": "Development of 3 identical two-storey residential houses (Ա1, Ա2, Ա3) with a stone perimeter fence. Each house has footprint 7.0m x 9.1m = 63.7 m2, total area 119.73 m2, flat roof with parapet at 7.6m (8.0m above ground). Perimeter fence height 2.4m.",
    "roof_area_m2": 66.76,
    "total_area_m2": 359.19
  },
  "geojson_file": "building/1.geojson",
  "measurements_file": "building/1.measurements.geojson",
  "model": "gemini-3.8-flash",
  "pages_used": [
    11,
    14,
    15,
    16,
    12,
    13,
    18
  ],
  "parcel": {
    "area_m2": 548.97,
    "perimeter_m": 99.5,
    "sides": 13,
    "source": "fence"
  },
  "parts": [
    {
      "base_m": 0.0,
      "color": "#e8e8e8",
      "floors": 2,
      "footprint_area": 64.3,
      "footprint_candidate": 24,
      "height_m": 8.0,
      "kind": "block",
      "name": "House Ա1"
    },
    {
      "base_m": 0.0,
      "color": "#e8e8e8",
      "floors": 2,
      "footprint_area": 64.3,
      "footprint_candidate": 20,
      "height_m": 8.0,
      "kind": "block",
      "name": "House Ա2"
    },
    {
      "base_m": 0.0,
      "color": "#e8e8e8",
      "floors": 2,
      "footprint_area": 64.3,
      "footprint_candidate": 16,
      "height_m": 8.0,
      "kind": "block",
      "name": "House Ա3"
    },
    {
      "base_m": 0.0,
      "color": "#e0e0e0",
      "footprint_area": 549.0,
      "footprint_candidate": 0,
      "height_m": 2.4,
      "kind": "fence",
      "name": "Perimeter fence"
    }
  ],
  "placed": false,
  "source": "data/plan/1.pdf",
  "warnings": [
    "House Ա1: outline page 12: outline does not align with the candidate (mean corner distance 1.84 m); using the candidate ring",
    "House Ա2: outline page 12: outline does not align with the candidate (mean corner distance 1.84 m); using the candidate ring",
    "House Ա3: outline page 12: outline does not align with the candidate (mean corner distance 1.84 m); using the candidate ring"
  ]
}
```

| Field | Notes |
|---|---|
| `placed` / `crs` | Whether the anchor sheet is georeferenced; the frame the footprints are in. A `page_units` anchor whose printed scale the model reported (`facts.anchor_scale_denominator`) is converted to `local_metres` arithmetically. |
| `anchor_page` / `pages_used` | The footprint sheet, and every sheet the model saw |
| `parts[]` | One entry per structure. `kind` — `block` (a building volume) or `fence` (a thin wall ring along its candidate). `height_m` top of walls (eaves for pitched roofs), `base_m` bottom, both metres above ground. |
| `parts[].roof` / `ridge_m` | `gabled` \| `hipped` \| `shed` and the ridge top; flat/parapet roofs omit both. Pitched roofs also appear in `geojson` as two stepped inset features (`role: "roof"`). |
| `parts[].color` | Facade colour read off the 3D renders, normalised `#rrggbb` |
| `parts[].footprint_candidate` / `footprint_area` | Which extracted ring the part stands on, and the extruded footprint's area (m² when the frame is metric) |
| `parts[].outline_page` | Present when the part extrudes the **exact outline drawn on that sheet** (a floor plan), scaled by its printed scale and rigidly aligned onto the candidate's position — instead of the site plan's block. Guarded by area ratio and alignment residual; a failed guard falls back to the candidate ring with a `warnings` entry. |
| `facts` | The printed indicator numbers, verbatim: `height_m`, `floors`, `buildings`, areas, `anchor_scale_denominator`, `notes`. Absent when none were reported. |
| `warnings` | Proposals that could not be applied (a building declared on the parcel boundary among them), guards that fell back, and why measurements were omitted; absent when none |
| `geojson_file` | Present with `--output`: `building/<document>.geojson`, relative to that directory |
| `geojson` | The extruded FeatureCollection: `height`/`base`/`color` per feature, fences as outer-ring + inset-hole polygons — ready for a Mapbox/MapLibre `fill-extrusion` layer |
| `parcel` | The ring the measurements are taken against: `source` — `survey_parcel` (the placed anchor's own turning points) \| `reference_parcel` (the `--parcel` polygon the anchor was placed from) \| `fence` (the declared perimeter fence's ring) \| `enclosing_ring` (the largest candidate holding every building) — with `area_m2`, `perimeter_m`, `sides`. **Absent, with a `warnings` entry, when the frame is not metric or no ring qualifies**; `measurements` is absent with it. |
| `measurements_file` | Present with `--output` and `parcel`: `building/<document>.measurements.geojson` |
| `llm_usage` | The reconstruction call's tokens and estimated cost — see *Token usage* below |
| `measurements` | The dimensions layer, inline — see below |

### `measurements`

Deterministic geometry over the parcel ring and the realized footprints,
in the same frame as `geojson`, so it draws on the same map and toggles as
one layer. Every length is the two-point `LineString` it was measured along,
with `length_m` and a ready `label`; features are typed by `kind`:

| `kind` | Geometry | Properties | Meaning |
|---|---|---|---|
| `parcel` | `Polygon` | `source`, `area_m2`, `perimeter_m` | The ring itself |
| `side` | `LineString` | `of` (`parcel` or a part name), `index` | One edge's length — the parcel's dimensions, and each footprint's |
| `setback` | `LineString` | `from` (part), `to: "parcel"`, `side` (index) | Shortest distance from a building to that parcel side; `0.00` when it touches or crosses |
| `clearance` | `LineString` | `from`, `to` (part names) | Shortest distance building↔building, and building↔fence where the fence is not the parcel ring |

Copied from the run above (`jq '.measurements.features[] | select(.properties.kind == "clearance")'`,
first entry — the gap between two houses of the row):

```json
{
  "geometry": {
    "coordinates": [
      [
        -16.8380833,
        4.445
      ],
      [
        -15.9855558,
        2.8182382
      ]
    ],
    "type": "LineString"
  },
  "properties": {
    "from": "House Ա1",
    "kind": "clearance",
    "label": "1.84 m",
    "length_m": 1.84,
    "to": "House Ա2"
  },
  "type": "Feature"
}
```

On a placed anchor the coordinates are degrees and the lengths are metres
all the same (measured in a local metric frame from the WGS 84
metres-per-degree series, centimetre-accurate over a parcel). Each block
gets one `setback` per parcel side — three houses on a thirteen-sided
ring is thirty-nine — so a map should filter by `kind`, or by `from`, rather
than draw everything at once.

A reply with no `add_part` call is asked again, up to three times, with the
previous reply quoted and the instruction that this one must declare the
main building; each empty attempt is a `warnings` entry (`attempt 1/3: the
model proposed no structure: …`) and `llm_usage` carries every attempt. A
run on a dossier with no drawing pages, no closed outline usable as a
candidate, or a model that declines three times fails with the reasons on
stderr — an invented building would be worse than none.

---

## `model`

Reconstructs the building storey by storey from the CAD-exported plan
sheets, **deterministically**: every wall, room and envelope on a vector plan
is a filled path with the fill colour of its element type, the sheets are
printed at a known scale, and the storeys are aligned on the printed grid.
Openings are what the envelope minus the rooms minus the walls leaves —
edge pieces are glazing, interior pieces door gaps; the parts of the
ground-floor walls outside the envelope are the stone piers, less the slivers
under 10 cm a wall drawn past the envelope leaves; terrace edges become
railings; the roof plan gives the roof outline and the parapet it draws — its
7–15 cm white bands joined into a ring on the roof, or a ring along the outline
with a `warnings` entry when it draws none. No rendering, no tracing, and —
unless asked — no model call. The Rust port of the earlier prototype; on
the reference document every polygon matches the prototype's output to within
0.1 % of its area, once the stone rule (the prototype kept whole pieces) is
applied to the prototype's ground floor — except the parapet, which the
prototype ringed along the outline.

```
pdf-toolkit model input.pdf [--params p.json] [--answers a.json] [--llm-params [MODEL]] [--parcel parcel.geojson] [-g lx,ly,lon,lat]… [-o dir]
```

What a drawing set does not print as geometry is a **parameter**: which
sheets are the floor plans and the roof plan, the plan scale, the storey
levels, the fill colours, the grid labels. Without `--params` the command
**detects** them from the document — title blocks (`+3,60 նիշով հարկի
հատակագիծ`), printed scales, level marks on the elevations and section, the
plan's own fill statistics, the lettered grid bubbles — and lists every
decision in `params_notes`. `--params <file>` overrides detection with a
reviewed file (the `params` block of a previous run, or the
`model/<document>.params.json` it writes). `--llm-params [MODEL]` shows the
detected parameters and the drawing sheets to a hosted model
(`gemini-3.8-flash` bare; any `--vlm` spec) and takes its `set_params`
proposal **after checking it against the drawings** — the pages it names
must be plan sheets, the levels must stack, the building it yields must be
building-sized — falling back to detection with a `warnings` entry
otherwise. It is networked and opt-in like every model call. **Coordinates
never come from a parameter source**; only these numbers do.

`--answers a.json` (the dossier's reviewed or vision-pass answers) lends its
page labels — `floor_plan`, `roof_plan`, `elevation_or_section` — to sheets
whose title blocks the classifier cannot read, which is most offices' exports
outside the ArchiCAD reference set. Detection then has three fallbacks it
names in `params_notes` and `warnings`: storey and roof levels read off the
elevations' level marks when the plan titles state none (each storey the
first mark at least 2.4 m above the last); a storey aligned on the centre of
its filled drawing when the sheet prints no grid labels
(`sheets[].origin_source: "fill_centre"`); and a building box derived from
the outline of the walls and rooms when the sheet draws no envelope fill.
Each is approximate where the drawing is unusual — a set whose "floor
plans" include existing-state or ceiling plans gets too many storeys — which
is what `--llm-params` is for: the proposal names the plan pages, the levels,
the scale and the colours, is checked against the drawings, and its accepted
`params` block can be kept and passed back as `--params` so the
reconstruction stays deterministic.

The model is **placed on the map** when the dossier allows: the ground
floor's envelope is matched by shape (rotation and translation, both
already in metres) to the building box drawn on a georeferenced sibling
sheet — the site plan placed from its own table, a sibling's, or the
`--parcel` polygon — and the sheet's other line work settles which way round
a symmetric box stands. `--georeference` references (two or more,
`local_x,local_y,longitude,latitude` in the model's own metres) place it
where the caller says instead. Otherwise the model stays in `local_metres`:
right in shape, unknown in position, for a map to let someone put it down by
hand. Copied from a real run (`jq 'del(.model, .geojson)'` — those two carry
the model and the extruded FeatureCollection inline):

```json
{
  "bounds": [
    [
      44.63044517659083,
      39.93789471181501
    ],
    [
      44.63061079146217,
      39.93802544132125
    ]
  ],
  "crs": "EPSG:4326",
  "document": "4",
  "floors": [
    {
      "door_gaps": 6,
      "glazing": 10,
      "level_m": 0,
      "name": "+0,00 նիշով հարկի հատակագիծ",
      "page": 13,
      "rails": 0,
      "rooms": 14,
      "rooms_area_m2": 72.52,
      "slab_area_m2": 87.79,
      "top_m": 3.6,
      "walls": 13
    },
    {
      "door_gaps": 8,
      "glazing": 12,
      "level_m": 3.6,
      "name": "+3,60 նիշով հարկի հատակագիծ",
      "page": 14,
      "rails": 6,
      "rooms": 20,
      "rooms_area_m2": 89.46,
      "slab_area_m2": 93.87,
      "top_m": 7.2,
      "walls": 18
    }
  ],
  "geojson_file": "model/4.geojson",
  "georeference": {
    "linework_residual_m": 0.0146,
    "origin": [
      44.630522,
      39.9379549
    ],
    "page": 10,
    "placed": true,
    "residual_m": 0.0036,
    "rotation_degrees": 227.13,
    "source": "sheet_outline",
    "transform": {
      "a": -0.680337217099822,
      "b": -0.7328992229692085,
      "tx": 1.239633377739945,
      "ty": 5.671490905290517
    }
  },
  "glb_file": "model/4.glb",
  "model_file": "model/4.model.json",
  "params": {
    "axis": {
      "column_label": "1",
      "font_size": 11.7,
      "row_label": "Ա"
    },
    "colours": {
      "envelope": [
        0.953,
        0.953,
        0.953
      ],
      "room": [
        1,
        1,
        1
      ],
      "tolerance": 0.02,
      "wall": [
        0.286,
        0.267,
        0.251
      ]
    },
    "elevation_pages": [
      16,
      17,
      18,
      19
    ],
    "floors": [
      {
        "level_m": 0,
        "name": "+0,00 նիշով հարկի հատակագիծ",
        "page": 13,
        "top_m": 3.6
      },
      {
        "level_m": 3.6,
        "name": "+3,60 նիշով հարկի հատակագիծ",
        "page": 14,
        "top_m": 7.2
      }
    ],
    "parapet_level_m": 7.78,
    "parapet_width_m": 0.15,
    "plinth_m": 0.35,
    "rail_height_m": 1.1,
    "roof_level_m": 7.2,
    "roof_page": 15,
    "scale_denominator": 75,
    "section_pages": [
      20
    ],
    "slab_thickness_m": 0.5,
    "source": "detected"
  },
  "params_file": "model/4.params.json",
  "params_notes": [
    "roof level 7.2 from the roof plan's title (page 15)",
    "roof plan: page 15",
    "elevations: page(s) 16, 17, 18, 19",
    "sections: page(s) 20",
    "parapet level 7.78 from the highest level mark on the elevations",
    "slab thickness 0.50 from paired level marks on the section",
    "plinth 0.35 from the lowest level mark",
    "floors: page 13 at 0 to 3.6, page 14 at 3.6 to 7.2",
    "scale 1:75 from the plan sheet's title block",
    "fill colours from page 13: walls [0.286, 0.267, 0.251] (69 fills), envelope [0.953, 0.953, 0.953], rooms [1.0, 1.0, 1.0]",
    "grid origin: column '1' x row 'Ա' from the bubbles on page 13"
  ],
  "placed": true,
  "roof": {
    "area_m2": 94.39,
    "page": 15,
    "parapet_pieces": 1
  },
  "sheets": [
    {
      "envelope_fills": 22,
      "origin_page": [
        399.8500614898803,
        334.47206472260586
      ],
      "origin_source": "axis_bubbles",
      "page": 13,
      "role": "floor",
      "room_fills": 119,
      "wall_fills": 66
    },
    {
      "envelope_fills": 14,
      "origin_page": [
        412.85828332492383,
        340.1626238121826
      ],
      "origin_source": "axis_bubbles",
      "page": 14,
      "role": "floor",
      "room_fills": 137,
      "wall_fills": 96
    },
    {
      "envelope_fills": 4,
      "origin_page": [
        437.86839508853154,
        368.5303832191421
      ],
      "origin_source": "axis_bubbles",
      "page": 15,
      "role": "roof",
      "room_fills": 775,
      "wall_fills": 0
    }
  ],
  "source": "data/plan/4.pdf",
  "stone_piers": 2,
  "warnings": [
    "walls: the callouts name travertine (pages 16, 17, 18) and plaster (page 19); travertine is kept"
  ]
}
```

| Field | Notes |
|---|---|
| `params` | The parameters the model was built from. `source` — `detected` (read off the document) \| `file` (`--params`) \| `model` (proposed by `--llm-params` and confirmed against the drawings). `floors[]` bottom first, one-based `page`, `level_m` / `top_m`; `roof_page` **absent** when the set has no roof plan; `elevation_pages` the elevation sheets the stone piers' heights are read from, **absent** when the set has none; `section_pages` the section sheets the slabs' edge bands are read from, **absent** when the set has none; `colours` RGB in 0..1 with a per-channel `tolerance`; `axis` the grid labels the storeys are aligned on, `font_size` **absent** when not pinned. The block is a valid `--params` file. |
| `params_notes` | One line per detection decision, in reading order. **Absent** for a `--params` file. |
| `placed` / `crs` | Whether the model is on the map; `geojson` is in `EPSG:4326` when placed and `local_metres` otherwise. `model` is always in local metres. |
| `georeference` | How it was placed: `source` — `sheet_outline` (matched to the box on `page`, with `residual_m` the mean corner distance and `linework_residual_m` how close the placed slab sits to the sheet's other line work, the latter **absent** when the sheet draws nothing else nearby) \| `references` (`--georeference`, `residual_m` the fit's RMSE). `rotation_degrees` CCW from the model's x axis to east, `origin` the tangent-plane origin, and `transform` `{a, b, tx, ty}` — the model's metres to metres east/north of `origin` as `x' = a·x − b·y + tx`, `y' = b·x + a·y + ty` (`a² + b² = 1`); degrees follow from `origin` and the WGS 84 metres-per-degree at its latitude, which is how a consumer brings the parcel into the model's frame. **Absent** when not placed, with a `warnings` entry. |
| `bounds` | The placed ground-floor slab's extent, `[[west, south], [east, north]]`. **Absent** when not placed. |
| `sheets[]` | How each sheet was read: `role` `floor` \| `roof`, the grid `origin_page` in page units, `origin_source` — `axis_bubbles` (both labels sit in drawn circles) \| `axis_text` (no circle; the text position stood in, with a `warnings` entry) \| `fill_centre` (no grid labels at all; the storey is aligned on the centre of its filled drawing, with a `warnings` entry) — and the fill counts by element type. A storey whose sheet the rules could not read, and a roof sheet likewise, are left out with a `warnings` entry rather than failing the run. |
| `floors[]` | Per-storey counts and areas: `walls`, `glazing`, `door_gaps`, `rooms`, `rooms_area_m2` (sum of the room fills), `slab_area_m2` (envelope plus terraces), `rails`; `empty` **present only when** the storey drew nothing the rules could read — no rooms and no walls, or a slab under 2 m², which is what a sheet whose fills do not read leaves behind. |
| `roof` | `page`, `area_m2` (the outline's, less the edge band where one is read), `parapet_pieces`; `empty` as a storey's, for an outline under 2 m². **Absent** when the set has no roof plan. |
| `stone_piers` | How many stone pieces `model.stone` holds: the parts of the ground-floor walls outside the envelope, cut at it — a pier that runs on into the house is stone only outside, the rest a wall — and without the parts nowhere thicker than 10 cm, which are walls drawn past the envelope. |
| `warnings` | Why the model is not placed, a rejected or failed parameter proposal, a sheet whose grid origin came from text, a roof plan that draws no parapet (a ring along the outline stands in), an elevation whose level marks or grid axes do not read, a stone pier whose elevations disagree on its top or that fewer than two elevations draw (it stands to the parapet), a section whose level marks or grid axes do not read, a slab a section cuts with an edge band that no plan draws (it keeps a square edge), a part whose callouts name more than one material (the one the most sheets name is kept), a storey that drew nothing the rules read and a roof outline too small to be one (both kept, marked `empty`); absent when none. |
| `model_file` / `glb_file` / `geojson_file` / `params_file` | Present with `--output`: `model/<document>.model.json`, `.glb`, `.geojson`, `.params.json`, relative to that directory. |
| `model` | The model itself, inline — the schema below. |
| `geojson` | The extruded FeatureCollection, inline — the shape below. |
| `llm_usage` | Present with `--llm-params`: that call's tokens and estimated cost — see *Token usage* below |

### `model` (the `model.json` schema)

`{units: "m", up: "z", source, scale: "1:75", levels, floors[], roof?, stone[], materials?}`.
Polygons are `{outer: [[x, y], …], holes: [[[x, y], …], …]}` in metres
relative to the grid intersection the parameters name, outer rings
counter-clockwise, holes clockwise, no closing repeat. `levels` names
`ground`, one entry per storey (`first`, `second`, …), `roof` and `parapet`.
Each floor: `name`, one-based `page`, `z`, `top`, `envelope` (the building
box as drawn), `slab` (envelope plus terraces, exterior only) with `slab_z`
`[bottom, top]`, `walls[]`, `glazing[]` with `glazing_z`, `door_gaps[]`,
`rooms[]` (`area` in m² plus the polygon), `rails[]` (open polylines along
the terrace edges, upper floors only), `edge`, the band the slab's edge
ends in — **absent** when no section cuts one, or no plan draws where it runs —
and `empty`, **present only when** the storey drew nothing the rules could
read: no rooms and no walls, or a slab under 2 m². Such a storey is carried as
the sheet gave it, and is not a reconstruction of a floor.
`roof`: `outline` (the envelope, terraces and edge strips the roof plan draws,
less its edge band), `slab_z`, `parapet[]` (the band the roof plan draws, cut
to the outline — or, when it draws none, a `parapet_width_m` ring along the
outline), `parapet_z`, `edge` as a floor's and `empty` for an outline under
2 m²; **absent** without a roof plan.
An `edge` is `{runs[], z, depth_m, tip_m, chamfer_m, face, section_page,
plan_page}`: the profile the section on `section_page` cuts at both ends of the
slab — `z` the band's `[bottom, top]` at full height, `depth_m` from its tip to
the slab, `tip_m` its height at the tip, `face` (`top` or `bottom`) the face
that slopes from the tip to full height `chamfer_m` in from it — and the runs
it takes along the slab's edges, where the plan on `plan_page` (the storey's
own, or else the roof plan) fills strips as wide as the band beside them. Each
run is `{inner, outer}`, two open polylines with as many vertices: `inner`
along the slab's edge and `outer` along the tip, vertex *i* of one facing
vertex *i* of the other, so a consumer that can slope lofts the band without
geometry of its own — full height lies `chamfer_m / depth_m` of the way from
`outer` to `inner`. `stone[]`: the
piers, each polygon with `top` — the level its face stands to on the
elevations that draw it — and the `top_pages` it was read from, extruded by
consumers from the plinth to `top`, or to the parapet when `top` is **absent**
(fewer than two elevations draw the pier — one alone may be a section filed as
an elevation — or they disagree by more than 5 cm). `materials`: what the
callouts on the elevations, sections and roof plan say each part is made of,
keyed by part — `wall`, `glazing`, `rail`, `stone`, `roof`, `parapet`, `slab`,
`plinth` (the ground storey's slab where it shows) — each `{material, text,
pages}`: `material` one of `travertine`, `basalt`, `felsite`, `tuff`,
`rough_stone`, `stone`, `plaster`, `concrete`, `metal`, `metal_sheet`,
`glass`; `text` the first callout naming it, as printed; `pages` the sheets
that call it out. A part called out in two materials keeps the one the most
sheets name; **absent** when no callout names a part and a material the rules
know. Any consumer
that can extrude a polygon between two z values rebuilds the building — an
edge band in steps; the glTF writer and the GeoJSON below are two such
consumers.

### `geojson`

One `Polygon` feature per element with `role` — `slab`, `wall`, `glazing`,
`rail`, `stone`, `roof`, `parapet` — plus `base` and `height` (metres above
±0.000), `color` (`#rrggbb`: the material the callouts give the element's part,
the ground storey's slab as the plinth, or the role's own), `area_m2`, and for storey elements `floor`
(index) and `floor_name`; a `stone` feature's `height` is the pier's `top`,
or the parapet without one. A slab's or the roof's edge band adds `slab` or
`roof` features, three per run from the tip in: two across the slope, each as
high as the slope stands halfway along it, then the rest of the band at full
height. Ready for a Mapbox/MapLibre `fill-extrusion`
layer, the same shape `building` emits. Rooms are carried flat
(`role: "room"`, no `height`) with `level`, `area_m2` and a ready `label`
for a map that names them. `metadata` carries `document`, `source`, `crs`,
`placed` and `params_source`.

A run on a dossier with no sheet titled as a floor plan fails with the
reason on stderr — pass `--params` naming the plan pages. A plan whose grid
labels the text layer does not carry fails the same way, because without
the grid the storeys cannot be stacked.

---

## `downsample`

Writes a smaller copy of the PDF by resampling its raster images, and nothing
else. Content streams, fonts and their `ToUnicode` maps are carried through
byte-identical, so text extraction reads the same characters from the output
as from the input. That is the difference from a general rewriter such as
Ghostscript's `pdfwrite`, which re-embeds fonts and can lose a ligature's
Unicode mapping ("offices" extracted as "ofÏces").

Resolution is measured against the size each image is **drawn** at — pixels
over the area of its largest placement, through the CTM and any form
XObjects — not a DPI stored in the file. An image drawn on several pages keeps
enough pixels for its largest placement; a soft mask is resampled with its
image. A JPEG stays a JPEG (grey, RGB, Adobe CMYK and Adobe YCCK, in the layout
it had); anything stored losslessly stays lossless, so resampling is the only
loss introduced.

From a run on a 12-page planning handbook (`downsampled` cut to one entry,
paths shortened):

```json
{
  "input": "handbook.pdf",
  "output": "/abs/out/handbook.200dpi.pdf",
  "pages": 12,
  "target_dpi": 200,
  "threshold_dpi": 220,
  "jpeg_quality": 85,
  "bytes_before": 17855319,
  "bytes_after": 7994546,
  "written": "downsampled",
  "images": { "total": 83, "downsampled": 51, "unchanged": 32 },
  "downsampled": [
    {
      "object": "10 0",
      "pages": [2],
      "from_px": [3092, 1490],
      "to_px": [1500, 723],
      "effective_dpi": 412.3,
      "bytes_before": 89939,
      "bytes_after": 75106,
      "encoding": "flate"
    }
  ],
  "unchanged": { "below_threshold": 11, "color_space": 3, "no_gain": 17, "not_placed": 1 },
  "warnings": []
}
```

| Field | Meaning |
|---|---|
| `input`, `output` | The paths as given on the command line. |
| `bytes_before`, `bytes_after` | Size of the input and of the file written. `bytes_after` is never larger. |
| `written` | `downsampled` — the rewritten document; `original` — the input's own bytes, because no image was resampled or the rewrite was not smaller. Either way `output` exists. |
| `images` | Image XObjects in the document: `total` = `downsampled` + `unchanged`. Inline images are not counted and never touched. |
| `downsampled[]` | One entry per resampled image, in object order. `object` is `"<number> <generation>"`; `pages` the one-based pages that draw it; `effective_dpi` pixels over its largest drawn area, one decimal; `encoding` `jpeg` \| `flate`. |
| `unchanged` | Count per reason an image was left as it is. A reason with no images is **absent**. |
| `warnings` | A page whose content stream could not be parsed; its images count as `not_placed`. Also on stderr. |

### `unchanged` reasons

| Reason | Meaning |
|---|---|
| `below_threshold` | Drawn at or below `threshold_dpi`, or already no larger than the target size. |
| `not_placed` | No page's content draws it (a pattern fill, an annotation appearance, an unused object), so its display size is unknown. |
| `no_gain` | Resampled, but the result was not smaller than the stored image. |
| `image_mask` | A 1-bit stencil mask. |
| `color_key_mask` | A colour-key `/Mask` array, which lossy samples would break. |
| `bits_per_component` | Not 8 bits per component. |
| `color_space` | Indexed, Separation, DeviceN, Lab, or unreadable: resampling would mean converting. |
| `unsupported_filter` | JPEG 2000, JBIG2, CCITT, or a filter chain around a JPEG. |
| `unsupported_cmyk_jpeg` | A four-channel JPEG without an Adobe CMYK or YCCK marker, whose ink direction readers disagree about. |
| `too_large` | Decoded samples over 1 GiB. |
| `matte_pair` | Could be resampled, but it is an image or a `/Matte` soft mask whose partner is left as it is; the two must stay the same pixel size, so both are kept. |
| `decode_failed` | The stream did not decode to the size its dictionary declares. |

An encrypted input fails the run: rewriting it would drop its protection.

---

## Token usage (`llm_usage`)

Every command that calls a hosted model reports each call it made, with the
provider's token counts and the cost estimated from
`crates/llm-providers/models.json` (USD per million tokens, the cache-read
rate for the cached share). `building` and `cadfix` always carry the array;
`dossier` carries it only with `--vlm`, `model` only with `--llm-params`, and
both omit it entirely when no model was called. Copied from a real
`dossier --vlm` run (one entry per flagged page):

```json
{
  "label": "vlm page 5",
  "model": "gemini-3.8-flash",
  "provider": "gemini",
  "usage_reported": true,
  "input_tokens": 1483,
  "cached_input_tokens": 0,
  "output_tokens": 212,
  "cost_usd": 0.001907,
  "elapsed_ms": 4180
}
```

| Field | Notes |
|---|---|
| `label` | What the call was for, in the command's words: `building`, `cadfix page 11`, `vlm page 5`, `model params`. Empty when the caller did not say. |
| `model` / `provider` | The model spec as given, and `gemini` \| `openai` \| `openrouter` — the provider its shape names. |
| `usage_reported` | Whether the provider returned token counts. When `false` the three counts are `0` and `cost_usd` is absent — an unreported usage is not a free one. |
| `input_tokens` | Prompt tokens, the cached share **included**, as every provider reports them. |
| `cached_input_tokens` | The share of the prompt served from the provider's cache, billed at the cache-read rate. |
| `output_tokens` | Reasoning/thinking tokens included, as providers bill them. |
| `cost_usd` | Estimated at the catalogued rates, six decimals. **Absent** when the model is not in `models.json` or no usage was reported. |
| `elapsed_ms` | Wall-clock time of the call. |

The same figures are printed on stderr as `llm:` lines as each call returns.

---

## Integration notes

- **Check the exit code before parsing.** Failure prints no JSON.
- **Branch on enums, not on prose.** `reason`, `class`, `failure.reason` and the
  `llm_reasons[].reason` slugs are stable; `detail`, `reason` text and error
  messages are not.
- **An absent field is not a false one.** `llm_reasons`, `warnings`,
  `declared_scale_denominator` and `printed_dimensions` are omitted when they do
  not apply, so that a check which could not run is distinguishable from one
  that passed.
- **Page keys are strings** in `inspect`, `cut`, `render` and `text`. `structured`
  and `vector` use arrays with a numeric `page`/`page_number` instead.
- **File paths in `cut`, `render` and `text` results are absolute.** `source`
  echoes the input path exactly as typed; `dossier`'s `pages[].cad.geojson`
  and the `geojson_file` / `measurements_file` of `cadfix` / `building` and
  the `model_file` / `glb_file` / `geojson_file` / `params_file` of `model`
  are relative to the `--output` directory — the report and its GeoJSON
  tree are meant to move together.
- **The subject parcel is tagged, not inferred.** Every placed page's
  collection ends with a `kind: "survey_parcel"` polygon; `building`
  measures against it. Draw that feature for the boundary rather than
  picking the largest ring.
- `vector` and `model` (without `--llm-params`) do not load PDFium; they need
  no native library and spawn no subprocess.
