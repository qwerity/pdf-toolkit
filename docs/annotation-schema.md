# Dossier annotation schema — draft 0.2.0

One JSON per dossier: `data/annotations/<n>.json`, describing the matching
`data/plan/<n>.pdf`. These are **ground truth**, produced by machine passes
(pdf-toolkit `inspect`/`text`/`vector`, OpenCV QR decoding) plus a
human/vision-model pass over every rendered page. The goal: a future
`pdf-toolkit` command reproduces this file.

The data itself lives under gitignored `data/` — the annotations quote private
content (names, passport numbers) and must never be committed. This schema and
`annotation-findings.md` are the committable record of the format and of what
the corpus taught us.

## Top level

| Field | Notes |
|---|---|
| `schema_version` | This document's version |
| `document` / `source` / `page_count` | Same meaning as the `vector` command |
| `stats` | Per-dossier counts (routes, QR pages, handwriting pages, placed pages) |
| `pages` | Array, one entry per page, in page order |
| `groups` | Document sections: pages that belong together |
| `entities` | Deduplicated real-world identifiers found anywhere in the dossier |
| `graph` | Edges connecting pages ↔ groups ↔ entities. **Derived** mechanically from the inline facts in `pages`/`groups`/`entities`; it repeats no independent information |

## `pages[]`

| Field | Notes |
|---|---|
| `page_number` | One-based |
| `labels` | Multi-label, see vocabulary below. Order: most specific first |
| `title` | One-line description of the page (from the visual pass) |
| `sheet` | The album sheet designation printed on the page (ԷՋ-03, Ճ-1, Է.02 …), when present |
| `classification` | `class`+`confidence` verbatim from `vector` — kept so the ground truth records what the current heuristic said, right or wrong |
| `content_kind` | `digital` (born-digital), `scan`, `mixed` |
| `languages` | ISO 639-1 of languages *visible on the page* (`hy`, `en`, `ru`) |
| `declared_scale_denominator` | Scale printed on the sheet, when one is |
| `extraction` | How text was/should be recovered — see below |
| `qr_codes` | Present only when the page carries QR codes. `data` null when detected/visible but undecodable |
| `handwriting` | Present only when handwriting is on the page: `extent` = `signatures` \| `fields` (form blanks filled by hand) \| `notes` \| `full_page`, plus `script` |
| `entities` | Ids into top-level `entities` that this page mentions/depicts |
| `groups` | Ids into top-level `groups` |
| `related_pages` | Non-group relations: `{"page": N, "relation": "continues" \| "duplicate_of"}` |
| `georeference` | Verbatim from `vector` **when placed**; absent otherwise |
| `cad` | Present on drawing pages: `{"geojson": "<relative path>", "crs", "features"}` — the geometry extraction lives in the GeoJSON file, georeferenced when the page placed |
| `anomaly` | Free-form flag for things a reviewer must see (e.g. a title block naming a different project) |

### `pages[].extraction`

| Field | Notes |
|---|---|
| `route` | `embedded_text` (pdfium layer is trustworthy) \| `vlm` (a vision model must read it) \| `none` (nothing to read — pure imagery) |
| `routing_signals` | The `inspect` `llm_reasons[].reason` slugs observed on the page |
| `text` | The embedded text (route `embedded_text` only) |
| `chars` | Length of `text` |
| `engine` | `pdfium` (route `embedded_text` only) |
| `encoding_issue` | `true` when a text layer exists but is wrong: legacy 8-bit font encodings (ArmSCII mojibake), glyph-outline CID fonts, custom phonetic-glyph fonts. Such pages are additionally VLM-flagged |
| `vlm` | `{"required": true, "reasons": [...], "extracted": {...}}` — `extracted` holds what the visual pass itself recovered; it is what the production VLM stage is expected to return |

Routing policy the ground truth encodes: embedded text when the layer is
present and not garbled (~0 ms); **everything else that carries information is
routed to the VLM** — scans, handwriting, photos of documents, unreadable or
mis-encoded text layers. Local OCR was evaluated on this corpus and dropped by
decision: see `annotation-findings.md` for the measurements. Pages that are
pure imagery (3D renders, photo sheets) are `none`.

`vlm.reasons` values seen in the ground truth: `scanned_or_unreadable_text`,
`legacy_encoding`, `handwritten_fields`, `handwritten_full_page`,
`passport_fields`, `id_card_fields`, `certificate_fields`, `stamp_entries`,
`small_qr`, `coordinate_table_on_raster`, `rotated_low_quality`,
`uncertain_name`, `uncertain_cadastral_code`. The list is open.

## Label vocabulary (extensible)

**Document types** — `application_form`, `cadastral_certificate` (ownership
certificate), `cadastral_reference` (ՏԵՂԵԿԱՆՔ), `passport`, `id_card`,
`social_card`, `power_of_attorney`, `consent_statement`, `official_letter`,
`decision`, `permit_document`, `receipt`, `contract`, `license`,
`technical_conditions`, `expert_conclusion`, `photo_montage`, `cover_page`,
`table_of_contents`, `text_document`

**Drawing types** — `cad_drawing`, plus the `vector` classes where they apply:
`site_plan`, `topographic_survey`, `cadastral_plan`, `floor_plan`,
`elevation_or_section`, `technical_detail`; and `roof_plan`, `general_plan`,
`scanned_drawing`

**Modifiers** — `handwritten`, `signature`, `e_signature`, `stamp`, `qr`,
`photo`, `map`, `table`, `low_quality_scan`, `empty_page`

## `groups[]`

`{"id", "kind", "title", "pages", "entities"}` — `kind` one of `identity`,
`ownership`, `application`, `administrative`, `financial`, `drawings`,
`technical`. A dossier can hold several `drawings` groups (e.g. cadastre
extracts vs. the design album vs. a demolition part); georeference application
respects that boundary:

`groups[].georeference` on `drawings` groups:
`{"status": "applied" | "same_site_unverified" | "none", "source_pages",
"applies_to_pages"}`. `applied` only where the placed fit is in the group's
own drawing frame; a floor plan of the same building is
`same_site_unverified` — it shares the site, not the coordinate frame.
Placing it needs its own references, per the toolkit's geometry-decides rule.

## `entities[]`

`{"id", "type", "value", "roles", "sources"}` where `type` ∈ `cadastral_code`,
`person`, `org`, `passport_number`, `id_number`, `psn` (social services
number), `address`, `application_number`, `certificate_number`,
`reference_number`, `permit_number`, `decision_number`, `conclusion_number`,
`license_number`, `date`, `phone`, `email`, `url`. `sources` =
`[{"page": N, "via": "embedded_text" | "qr" | "vlm"}]` — `via: "vlm"` marks
values only a vision pass could read.

Id convention: `<type>:<normalized-value>`, e.g.
`cadastral_code:07-013-0166-0029`.

## `graph.edges[]`

`{"from", "to", "type", "via"?}` with node refs `page:N`, `group:<id>`,
`entity:<id>`. Types: `member_of` (page→group), `mentions` (page→entity, with
`via`), `about` (group→cadastral-code entity), `continues` / `duplicate_of`
(page→page), `georeference_source` (page→group). Derived; a reviewer should
check the inline facts, the edges follow.
