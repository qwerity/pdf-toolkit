# pdf-toolkit JSON API

`pdf-toolkit` is a command-line tool. Every subcommand prints **exactly one JSON
document on stdout and nothing else**, so it can be driven from another program.

This file is the contract. It is the reference for integrating the tool; if you
are wiring it into a service, this is all you need.

- Version described here: **0.9.0** (matches the package version)
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
| `--answers` | `dossier`, `building` | Vision-pass answers file (`dossier`: merged; `building`: labels sharpen sheet selection) |
| `--vlm` | `dossier` | In-process vision pass. Bare `--vlm` uses `gemini-3.8-flash`; see below |
| `--page` | `cadfix` | The one page holding the drawing to refine |
| `--model` | `cadfix`, `building` | Hosted model; defaults to `gpt-6-astra`. Same spec grammar as `--vlm`. |

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
`floor_plan`, `elevation_or_section`, `technical_detail`, `scanned_document`,
`scanned_drawing`, `text_document`, `unknown`. Branch on this, not on `reason`.

`confidence` is `certain` or `ambiguous`. **`ambiguous` is the common case** —
structural rules settle the pages that matter and leave the rest open. In
particular `unknown` means "a drawing, but its type cannot be told from
structure alone"; that is the page a vision model would be asked about. It does
not prevent georeferencing: a page with a readable coordinate table places
regardless of how it is classified.

`is_drawing` is broader than `is_mappable` and answers a different question — a
floor plan is unmistakably a drawing, it is simply not a drawing of terrain.

`features` are the measurements the verdict was made from, reported so a caller
that disagrees can see the basis.

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
| `source` | enum | `text_layer`, `document_table` or `references`. Absent when not placed. |
| `failure.reason` | enum | Absent when placed. See below. |
| `crs` | string | The CRS the **fit** was made in — the exported coordinates are WGS 84 either way. Absent if it could not be identified. |
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
pages the file answers are not re-asked. **The hosted-model surfaces —
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
  "generated_by": "pdf-toolkit 0.9.0 dossier: deterministic extraction (pdfium text, templates, rxing QR, vector classification and georeferencing); vision-pass answers pending",
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
      "page_number": 1
    }
  ],
  "schema_version": "0.2.0",
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
| `pages` | Array, one entry per page — field tables in `annotation-schema.md` |
| `groups` / `entities` / `graph` | Document sections, the identifier registry, and the derived edges |

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

Internal-consistency problems (a dangling group or entity reference, an
out-of-range related page) are reported as `Warning:` lines on stderr, per
the toolkit-wide rule; the JSON on stdout is emitted either way.

With `--output`, each drawing page's GeoJSON is written under
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
pdf-toolkit cadfix input.pdf --page 11 [--model gpt-6-astra] [-o dir]
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
| `geojson` | The refined FeatureCollection, inline |

## `building` *(experimental)*

Reconstructs, as map-native extruded GeoJSON, what the permit
says the finished site must look like — every house block of a development,
the perimeter fence, roof forms, facade colours. The drawing set's
**visualization section** (development scheme, facades and sections with
level marks, roof plan, indicator table, 3D renders — usually the last
sheets) is what the model reads; the **anchor sheet** — the placed page
where one exists, else the plan-view sheet richest in closed outlines —
supplies footprint candidates as exact extracted geometry. One request,
parallel tool calls: `add_part` per structure, `set_facts` once for the
printed indicator numbers. The model reads numbers and colours; it never
draws.

```
pdf-toolkit building input.pdf [--answers a.json] [--model gpt-6-astra] [-o dir]
```

`--answers` labels (`elevation_or_section`, `floor_plan`, `general_plan`,
`photo_montage`, `3d_view`, …) sharpen which sheets are shown; without them
the `vector` classification chooses, and the document's last pages stand in
for the render sheets. Copied from a real run (`jq 'del(.geojson)'` — the
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
    "footprint_area_m2": 0,
    "height_m": 7.6,
    "notes": "Three proposed identical houses A1–A3. Areas are printed per typical house, not development totals: first floor 62.15 m², second floor 57.58 m², total 119.73 m²; roof 66.76 m². No footprint-area indicator is printed in the supplied sheets; footprint_area_m2=0 denotes unavailable, not a stated zero area. Individual facades mark the parapet +7.600 relative to +0.000; overall site elevations additionally dimension 8000 mm from grade, including the 400 mm plinth offset. Perimeter wall is dimensioned 2400 mm. Flat parapet roofs and light warm-grey finishes follow the renders. Existing neighbouring buildings are outside the proposed development.",
    "roof_area_m2": 66.76,
    "total_area_m2": 119.73
  },
  "geojson_file": "building/1.geojson",
  "model": "gpt-6-astra",
  "pages_used": [
    11,
    14,
    15,
    16,
    12,
    13,
    18
  ],
  "parts": [
    {
      "base_m": 0.0,
      "color": "#d8d4d0",
      "floors": 2,
      "footprint_area": 64.3,
      "footprint_candidate": 24,
      "height_m": 7.6,
      "kind": "block",
      "name": "house A1"
    },
    {
      "base_m": 0.0,
      "color": "#d8d4d0",
      "floors": 2,
      "footprint_area": 64.3,
      "footprint_candidate": 20,
      "height_m": 7.6,
      "kind": "block",
      "name": "house A2"
    },
    {
      "base_m": 0.0,
      "color": "#d8d4d0",
      "floors": 2,
      "footprint_area": 64.3,
      "footprint_candidate": 16,
      "height_m": 7.6,
      "kind": "block",
      "name": "house A3"
    },
    {
      "base_m": 0.0,
      "color": "#d8d4d0",
      "floors": 0,
      "footprint_area": 549.0,
      "footprint_candidate": 0,
      "height_m": 2.4,
      "kind": "fence",
      "name": "perimeter yard wall"
    }
  ],
  "placed": false,
  "source": "data/plan/1.pdf"
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
| `warnings` | Proposals that could not be applied; absent when none |
| `geojson_file` | Present with `--output`: `building/<document>.geojson`, relative to that directory |
| `geojson` | The extruded FeatureCollection: `height`/`base`/`color` per feature, fences as outer-ring + inset-hole polygons — ready for a Mapbox/MapLibre `fill-extrusion` layer |

A run on a dossier with no drawing pages, no closed outline usable as a
candidate, or a model that (correctly) declines to match a structure to a
candidate fails with the reason on stderr — an invented building would be
worse than none.

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
  and the `geojson_file` of `cadfix` / `building` are relative to the
  `--output` directory — the report and its GeoJSON tree are meant to move
  together.
- `vector` is the only command that does not load PDFium; it needs no native
  library and spawns no subprocess.
