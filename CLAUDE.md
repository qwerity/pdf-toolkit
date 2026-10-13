# Working on pdf-toolkit

## The one rule that is easy to break

**`docs/json-api.md` is a contract with callers who cannot see the code.**

Applications drive this tool by spawning it and parsing stdout, and none of
them can tell that a field was renamed until it is in production. The `serve` HTTP mode is a second contract of the
same kind: `docs/service.md`, pinned by `tests/service_contract.rs`, changes in
the same commit as any route or response shape.

If a change alters what any command prints, update `docs/json-api.md` in the
same commit. That means any of:

- a new, renamed or removed field in any result shape
- a change to when a field is present, since an absent field is meaningful
- a new value for an enum a caller branches on — `llm_reasons[].reason`,
  `classification.class`, `confidence`, `georeference.source`,
  `failure.reason`, `crs`, `extraction.route`, `groups[].georeference.status`,
  `related_pages[].relation`, `graph.edges[].type`, `sources[].via`
- a new subcommand, or a flag that changes the output
- a change to `errors` / `warnings`, or to exit-code behaviour

Practical checklist when you touch output:

1. Change the code.
2. Update the matching table and example in `docs/json-api.md`. **Examples are
   copied from real runs — regenerate rather than hand-edit:**
   `pdf-toolkit <command> … | jq`. Numbers typed from memory are how a contract
   quietly becomes fiction.
3. If the shape changed incompatibly, say that a `version` bump is needed —
   the user cuts versions; never bump `version` in the workspace `Cargo.toml`
   yourself. It is how a caller detects a schema change, and a   caller may gate startup on the installed version matching its
   manifest. The reviewed dossiers in `data/annotations/<n>.json` carry the
   version they were reviewed against in `generated_by`; `dossier_e2e`
   ignores that version (it checks the output names the running one), so a
   bump needs no refresh of the annotations.
4. Add the change to `.github/release-notes/<tag>.md` — a caller reads the
   release, not the diff.
5. Re-read the "Integration notes" section of the contract. It makes promises
   about behaviour that code changes can invalidate.

The serialisable types are spread over the crates;
`grep -rlE 'derive\([^)]*Serialize' crates/*/src` lists every file whose
output a caller can see.

## What this tool is

`pdf-toolkit --help` lists the commands. `README.md` explains the pipeline and
the crate layout; `docs/json-api.md` is the output contract, `docs/service.md`
the HTTP one, and `docs/annotation-schema.md` is the field reference for the
`dossier` annotation.

## Principles worth keeping

**Report absence, not a vacuous pass.** Where something does not apply, the
field is absent rather than defaulted. A check that could not run must not look
like a check that passed — `printed_dimensions` on a sheet with no printed side
lengths, `llm_reasons` on a page needing no model, `declared_scale_denominator`
where none was readable.

**A page that fails does not fail the run.** Per-page problems are
`ExtractionWarning`s reported beside the results; only whole-document problems
abort. A run that produced nothing still explains itself, on stderr *and* in
`errors`.

**Geometry decides, not confidence.** A georeferencing fit is accepted because
its residual agrees with the drawing, not because a reader was sure. Keep this
shape for anything new: let a source propose, let the geometry confirm.

**Invalid states should be unrepresentable.** The CLI has one subcommand per
operation so that clap rejects a nonsensical request instead of the program
checking for one. When adding an option, put it on the command it applies to
rather than adding a validation.

**`pdf-core` depends on no workspace crate but the leaf `pdf-armscii`.** That
rule is what keeps the dependency graph acyclic; the two chains are in
`README.md`. `llm-providers` is likewise leaf-free and is the only crate that
talks to a model — every model surface (`--vlm`, `cadfix`, `building`,
`model --llm-params`) is opt-in per invocation and routes through it. The one
other network connection is `pdf-ocr` downloading the OCR models
(`fetch_models`). `pdf-model` never depends on
`pdf-vlm`: the model proposes parameters through a callback the CLI injects,
so the reconstruction crate stays offline and testable on its own.

**Every extraction is scored, and the score may only go up.**
`crates/pdf-toolkit/tests/scorecard.rs` grades each kind of reading on the
corpus — page classification, labels, routing, trust, entities, groups, the
request block, georeferencing, the facts, the cross-page checks, the storey
model — against a per-document ledger of what a person says is on the page
(`data/expected/<n>.json`, private; `scripts/seed-expected.py` seeds one from
the reviewed annotation, a person then corrects it), and compares every
number with the committed baseline (`tests/scorecard/baseline.json`, numbers
only). A kind that scores below its baseline fails the test; a kind that
rose is reported. The golden tests beside it say whether
the output changed at all; this one says whether it got better or worse,
per extraction, per document. **A ledger entry is truth, not output**: when
the toolkit is wrong, fix the ledger to what the page says and let the score
show the gap. Running it, accepting new scores and the text bench: the
`scorecard` skill.

**The ledgers are ground truth, and a failing test is never a reason to edit
one.** A value in `data/expected/<n>.json` may change for exactly two reasons:

1. **The page shows the value is wrong.** The commit says which page and what
   it prints, and the value is checked against the page image, not against the
   toolkit's output.
2. **New fields or new logic:** a new kind, a new section, a new check. The new
   entries are checked against the page before they go in, like any other entry.

Never change, remove or loosen a value so that a test passes, and never refresh
a ledger from a run (`seed-expected.py` is for a document that has no ledger).
When the output and a ledger disagree, the code is what gets fixed, or the gap
stays visible in the score. A `reviewed` flag turns `true` only after every
entry of its section was checked against the pages; it never turns `false` to
make room for unchecked output. The annotations in `data/annotations/` are the
other kind of file: golden snapshots of the output, regenerated when the output
legitimately changes, and only after the scorecard shows no kind dropped
against the ledgers.

## Conventions

- Numbers in the docs are measured, not estimated. If you quote one, say where
  it came from and re-measure when the code changes.

## Running things

```sh
PDF_TOOLKIT_REQUIRE_CORPUS=1 cargo test     # corpus tests must run, not skip
./build.sh                                  # dist/ bundle; runs the full gate
```

**`data/` is the test data set, committed with the code.** `data/plan/` holds
the fourteen real building-permit dossiers,
`data/annotations/` the reviewed annotations the `dossier` tests replay, with
the vision-pass answers, the cadastre parcels, the recorded `building` reports
and the accepted `model` parameters beside them, and
`data/expected/` the scorecard ledgers. **It is personal data** — owners,
addresses, identity documents — in a private repository: never publish it,
never copy it into another repository, never quote a person or a parcel from
it in a commit message or a doc. Only what no test reads is gitignored
(`data/misc/`, run logs, and the per-page GeoJSON exports `dossier -o`
regenerates).

The corpus tests skip themselves when the files are absent (a partial
checkout), so `cargo test` still passes; `PDF_TOOLKIT_REQUIRE_CORPUS=1` turns
that skip into a failure. There is no CI test job, so set it yourself whenever
a run has to prove the corpus was exercised.

## Releasing

**The user cuts versions and releases** — never bump `version`, push a tag or
publish a release yourself. **Tag with a bare version — `0.8.0`, not
`v0.8.0`**: a `v` prefix produces an asset neither consumer can fetch. The
build variants, what the archive carries and how consumers pick a release up
are in the `release` skill.
