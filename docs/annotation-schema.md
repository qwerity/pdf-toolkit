# Dossier annotation schema — 0.2.0

The output of `pdf-toolkit dossier` (command contract: the `dossier` section
of `json-api.md`). One JSON per document: every page's labels, extraction
routing, QR payloads, entities and groups, plus a graph derived from those
facts.

The command runs deterministically; pages only a vision model can read carry
an `extraction.vlm` work order. Running again with `--answers <file>` merges
the vision pass's results — the answers file format is at the end of this
document. The reviewed reference corpus lives in gitignored `data/`
(`data/plan/` + `data/annotations/`, private data), and the corpus test
requires `dossier --answers` to reproduce those annotations exactly.

## Top level

| Field | Notes |
|---|---|
| `schema_version` | This document's version |
| `document` / `source` / `page_count` | Same meaning as the `vector` command |
| `stats` | Per-dossier counts (routes, QR pages, handwriting pages, placed pages) |
| `pages` | Array, one entry per page, in page order |
| `groups` | Document sections: pages that belong together |
| `entities` | Deduplicated real-world identifiers found anywhere in the dossier |
| `warnings` | Pages whose geometry extraction failed outright, verbatim from the vector pass — such a page has no classification/`cad`/georeference, and this records why. Absent when none. |
| `georeference_note` / `note` | Document-level free-text notes carried over from the answers file (`georef_note` / `note`); absent unless the reviewer wrote them |
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
| `handwriting` | Present only when handwriting is on the page: `present` (always `true` — the block's presence is the fact), `extent` = `signatures` \| `fields` (form blanks filled by hand) \| `notes` \| `full_page`, plus `script` |
| `entities` | Ids into top-level `entities` that this page mentions/depicts |
| `groups` | Ids into top-level `groups` |
| `related_pages` | Non-group relations: `{"page": N, "relation": "continues" \| "duplicate_of"}` |
| `georeference` | Verbatim from `vector` **when placed**; absent otherwise |
| `cad` | Present on drawing pages: `{"geojson": "<relative path>", "crs", "features"}` — the geometry extraction lives in the GeoJSON file, georeferenced when the page placed |
| `anomaly` | Free-form flag for things a reviewer must see (e.g. a title block naming a different project) |
| `notes` | Reviewer's free-text page note, carried over from the answers file; absent unless written |

### `pages[].extraction`

| Field | Notes |
|---|---|
| `route` | `embedded_text` (pdfium layer is trustworthy) \| `vlm` (a vision model must read it) \| `none` (nothing to read — pure imagery) |
| `routing_signals` | The `inspect` `llm_reasons[].reason` slugs observed on the page |
| `text` | The embedded text (route `embedded_text` only) |
| `chars` | Length of `text` |
| `engine` | `pdfium` (route `embedded_text` only) |
| `encoding_issue` | `true` when a text layer exists but is wrong: legacy 8-bit font encodings (ArmSCII mojibake), glyph-outline CID fonts, custom phonetic-glyph fonts. Such pages are additionally VLM-flagged |
| `vlm` | `{"required": true, "reasons": [...], "extracted": {...}}` — `required` is invariantly `true` (the block's presence is the fact); `extracted` holds what the visual pass itself recovered (`summary` and/or `notes`, free text) and appears only when an answer carried one; it is what the production VLM stage is expected to return |

Routing policy: embedded text when the layer is present and not garbled
(~0 ms); **everything else that carries information is routed to the VLM** —
scans, handwriting, photos of documents, unreadable or mis-encoded text
layers. Local OCR was evaluated on the corpus and dropped by decision: see
`annotation-findings.md` for the measurements. Pages that are pure imagery
(3D renders, photo sheets) are `none`.

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
`elevation_or_section`, `technical_detail`, `3d_view`; and `roof_plan`, `general_plan`,
`scanned_drawing`

**Modifiers** — `handwritten`, `signature`, `e_signature`, `stamp`, `qr`,
`photo`, `map`, `table`, `low_quality_scan`, `empty_page`

## `groups[]`

`{"id", "kind", "title", "pages", "entities"}` — `kind` one of `identity`,
`ownership`, `application`, `administrative`, `financial`, `drawings`,
`technical`. A deterministic-only run (no answers) derives structural groups
with fixed ids — `application`, `certificate:<number>` (one per ownership
certificate, `ownership` when its parcel matches the application form's,
`administrative` otherwise), `cadastre_references`, `drawings` — and
generated English titles; answered pages carry whatever groups their answers
name instead. A dossier can hold several `drawings` groups (e.g. cadastre
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
`[{"page": N, "via": "embedded_text" | "qr" | "vlm" | "none"}]` — `via:
"vlm"` marks values a vision pass (or the reviewer looking at the page) had
to read; `via: "none"` appears when an answer names an entity on a page
whose route is `none` (pure imagery — the reviewer saw it, no machine did).
Treat the list as closed but branch defensively; `mentions` edges carry the
same values.

Id convention: `<type>:<normalized-value>`, e.g.
`cadastral_code:07-013-0166-0029`.

## `graph.edges[]`

`{"from", "to", "type", "via"?}` with node refs `page:N`, `group:<id>`,
`entity:<id>`. Types: `member_of` (page→group), `mentions` (page→entity, with
`via`), `about` (group→cadastral-code entity), `continues` / `duplicate_of`
(page→page), `georeference_source` (page→group). Derived; a reviewer should
check the inline facts, the edges follow.

## The answers file (`dossier --answers`)

What the vision pass (or a human reviewer) returns; parsed by
`crates/pdf-dossier/src/answers.rs`. An answered page's semantic fields
replace the deterministic guess wholesale — the answer came from something
that looked at the page.

```json
{
  "pages": {
    "6": {
      "labels": ["cadastral_certificate", "id_card", "qr", "stamp"],
      "kind": "scan",
      "lang": ["hy", "en"],
      "title": "one-line description of the page",
      "notes": "anything the reviewer should see",
      "entities": ["cadastral_code:01-011-0388-0007", "person:example-name"],
      "groups": ["ownership", "identity"],
      "related": [{"page": 7, "relation": "continues"}],
      "handwriting": {"extent": "fields", "script": ["hy"]},
      "vlm": ["id_card_fields"],
      "declared_scale": 500,
      "sheet": "ԷՋ-03",
      "encoding_issue": false
    }
  },
  "groups": {
    "ownership": {"kind": "ownership", "title": "…"},
    "identity": {"kind": "identity", "title": "…"}
  },
  "people": {
    "person:example-name": {"value": "printed name", "roles": ["owner"]}
  },
  "georef_note": "optional document-level note",
  "note": "optional document-level note"
}
```

Every field but `labels` is optional. `groups` metadata is keyed by the ids
pages reference, in the order the final report should list them; `people`
supplies display values and roles for person/org entity ids. Pages without an
answer keep their deterministic facts, so a partial answers file is a valid
merge. Closed vocabularies are validated at load: an unknown `relation` value
is a parse error naming the field, never a new edge type in the graph.
