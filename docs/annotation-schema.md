# Dossier annotation schema — 0.4.0

The output of `pdf-toolkit dossier` (command contract: the `dossier` section
of `json-api.md`). One JSON per document: every page's labels, extraction
routing, QR payloads, entities and groups, plus a graph derived from those
facts.

The command runs deterministically; pages only a vision model can read carry
an `extraction.vlm` work order. Running again with `--answers <file>` merges
the vision pass's results — the answers file format is at the end of this
document. The reviewed reference corpus is a local test data set, not distributed
(`data/plan/` + `data/annotations/`; personal data), and
the corpus test requires `dossier --answers` to reproduce those annotations
exactly.

## Top level

| Field | Notes |
|---|---|
| `schema_version` | This document's version |
| `document` / `source` / `page_count` | Same meaning as the `vector` command |
| `stats` | Per-dossier counts (routes, QR pages, handwriting pages, placed pages) |
| `request` | What the application form asks for: `{"page", "via", "action", "object"?, "use"?, "use_page"?, "works"?, "storeys"?, "address"?, "planned"?}` — see the `request` subsection of the `dossier` contract in `json-api.md` for the vocabularies. Deterministic, read off the Ձև N 1-1 form's fixed slots; absent when no embedded-text application form states a request |
| `indicators` | The ՏՏՑ table as printed: `{"page", "plot"?, "footprint"?, "green"?}`, each figure `{"m2", "pct"?}`, `footprint` a list of rows — see the `indicators` subsection of the `dossier` contract in `json-api.md`. Deterministic; absent when no embedded-text page prints the table |
| `facts` | Every figure the dossier states about its parcel, one entry per page that states it, with the page's `trust` — see the `facts` and `checks` subsection of the `dossier` contract in `json-api.md`. Absent when nothing was read |
| `checks` | Each repeated figure compared across the pages that state it, the official page as the reference. Absent when there was nothing to compare |
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
| `trust` | Who vouches for the page: `official` (issued by the Cadastre Committee — certificate, reference letter, fee table, map extract), `applicant` (the form, the drawings, the notes), `identity` (a person's papers), `unknown` (a page nothing has read yet) |
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
| `cad` | Present on drawing pages **and on every placed page**: `{"geojson": "<relative path>", "crs", "features"}` — the geometry extraction lives in the GeoJSON file, georeferenced when the page placed. A page the structural heuristic could not classify but that was fitted to the survey is labelled `cad_drawing` by the deterministic pass — geometry decides |
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

`{"id", "kind", "trust", "title", "pages", "entities", "access"?}` — `kind` one of `identity`,
`ownership`, `application`, `administrative`, `financial`, `drawings`,
`technical`. A deterministic-only run (no answers) derives structural groups
with fixed ids — `application`, `certificate:<number>` (one per ownership
certificate, `ownership` when its parcel matches the application form's,
`administrative` otherwise), `cadastre_references`, `drawings` — and
generated English titles; answered pages carry whatever groups their answers
name instead. `trust` is the document's: `official` once any page of the group
is the Committee's, else `identity` for identity papers, else `applicant`. A dossier can hold several `drawings` groups (e.g. cadastre
extracts vs. the design album vs. a demolition part); georeference application
respects that boundary:

`groups[].access[]` on `ownership` and `administrative` groups (0.4.0): the
credentials the document prints on itself — the number and passcode
(«գաղտնաբառ») from a certificate's or reference letter's header: `{"kind":
"certificate" | "reference", "number", "passcode", "page"}`. Absent when the
pages print none. The toolkit reads the pair and nothing more; it builds no
link and retrieves nothing.

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
`cadastral_code:07-013-0166-0029`. A person/org id is the value's first two
words transliterated to Latin and then **sorted** — Armenian documents
alternate name order (a certificate prints given-surname, a passport
surname-given), and sorting makes the same person land on one node
whichever document named them.

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
      "facts": [{"kind": "signatory", "value": "name as the hand wrote it"},
                {"kind": "identity_holder", "value": "name the passport prints"},
                {"kind": "applicant", "value": "printed name", "label": "ՊԱՏՎԻՐԱՏՈՒ՝"}],
      "access": [{"kind": "certificate", "number": "…", "passcode": "…"}],
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

Every field but `labels` is optional. `facts[]` carries the figures and names
only a look at the page can supply; `signatory` (the name written beside a
signature) and `identity_holder` (whom a passport or ID card names) are the
two the `signature` check compares, and `access[]` the credentials a scanned
Committee header prints. Both are person-valued and canonicalised to the same
`person:` ids the owner and applicant facts use. An `owner` or `applicant` fact
may carry `label`, the words printed right before the name; an applicant whose
label is not «Դիմող», «Պատվիրատու», «Կառուցապատող» or «հայտատու» (a title
block's designer) is dropped, and on a page with an Armenian text layer either
one is kept only when its surname or given name is in that text — a scan keeps what the model
read. Answers recorded without labels meet only the text test. `groups` metadata is keyed by the ids
pages reference, in the order the final report should list them; `people`
supplies display values and roles for person/org entity ids. Pages without an
answer keep their deterministic facts, so a partial answers file is a valid
merge. Closed vocabularies are validated at load: an unknown `relation` value
is a parse error naming the field, never a new edge type in the graph.
