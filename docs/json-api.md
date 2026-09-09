# pdf-toolkit JSON API

`pdf-toolkit` is a command-line tool. Every subcommand prints **exactly one JSON
document on stdout and nothing else**, so it can be driven from another program.

This file is the contract. It is the reference for integrating the tool; if you
are wiring it into a service, this is all you need.

- Version described here: **0.8.0** (matches the package version)
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

### Options

| Option | Commands | Meaning |
|---|---|---|
| `--pages`, `-p` (alias `--page`) | all but `inspect` | `all` (default), or a list/range like `1,3,5-7` |
| `--output`, `-o` | `cut`, `render`, `text`, `vector` | Output directory. Defaults to the working directory. |
| `--concat`, `-c` | `cut`, `text` | Merge the selection into one file |
| `--scale`, `-s` | `render` | Scale factor. Default `2.0`. |
| `--scale-width-px` | `render` | Exact output width in pixels; preserves aspect ratio |
| `--structured-format` | `structured` | `full` (default) or `markdown` |
| `--georeference`, `-g` | `vector` | `local_x,local_y,longitude,latitude`; repeatable |

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
  "source": "test_assets/samples/3.pdf",
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
| `source` | enum | `text_layer` or `references`. Absent when not placed. |
| `failure.reason` | enum | Absent when placed. See below. |
| `crs` | string | The CRS the **fit** was made in — the exported coordinates are WGS 84 either way. Absent if it could not be identified. |
| `rmse_m` | float | RMS residual over the control points, in metres |
| `max_residual_m` | float | Worst single residual, in metres |
| `control_points_used` | int | Points the fit was built on |
| `fitted_scale_denominator` | float | Sheet scale the fit implies. Absent for a reference placement. |
| `declared_scale_denominator` | float | Scale printed on the sheet, when readable. Often absent. |
| `rotation_degrees` | float | Rotation to true north, CCW. Only for a reference placement. |
| `center` | `[lon, lat]` | Centre of the placed geometry. Absent if the CRS could not be converted. |

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
- **Paths in results are absolute.**
- `vector` is the only command that does not load PDFium; it needs no native
  library and spawns no subprocess.
