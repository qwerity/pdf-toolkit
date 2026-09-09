# Working on pdf-toolkit

Notes for anyone — human or model — changing this codebase.

## The one rule that is easy to break

**`docs/json-api.md` is a contract with callers who cannot see the code.**

Applications drive this tool by spawning it and parsing stdout, and none of
them can tell that a field was renamed until it is in production.

If a change alters what any command prints, update `docs/json-api.md` in the
same commit. That means any of:

- a new, renamed or removed field in any result shape
- a change to when a field is present, since an absent field is meaningful
- a new value for an enum a caller branches on — `llm_reasons[].reason`,
  `classification.class`, `confidence`, `georeference.source`,
  `failure.reason`, `crs`
- a new subcommand, or a flag that changes the output
- a change to `errors` / `warnings`, or to exit-code behaviour

Practical checklist when you touch output:

1. Change the code.
2. Update the matching table and example in `docs/json-api.md`. **Examples are
   copied from real runs — regenerate rather than hand-edit:**
   `pdf-toolkit <command> … | jq`. Numbers typed from memory are how a contract
   quietly becomes fiction.
3. If the shape changed incompatibly, bump `version` in the workspace
   `Cargo.toml`. It is how a caller detects a schema change, and a   caller may gate startup on the installed version matching its
   manifest.
4. Add the change to `.github/release-notes/<tag>.md` — a caller reads the
   release, not the diff.
5. Re-read the "Integration notes" section of the contract. It makes promises
   about behaviour that code changes can invalidate.

The serialisable types are all in one place, which makes this easy to check:

| Shape | Defined in |
|---|---|
| `inspect`, `cut`, `render`, `text`, `structured` output | `crates/pdf-toolkit/src/cli_output.rs` |
| `PageInfo`, `LlmReason` | `crates/pdf-inspect/src/page_info.rs` |
| `vector` output | `crates/pdf-vector/src/report.rs` |
| GeoJSON properties and `metadata` | `crates/pdf-geo/src/geojson.rs` |
| `PageClass`, `Confidence` | `crates/pdf-classify/src/heuristic.rs` |
| `GeoreferenceFailure` | `crates/pdf-geo/src/pipeline.rs` |
| Errors and per-page warnings | `crates/pdf-core/src/{error,warning}.rs` |

## What this tool is

Six commands over one PDF: `inspect`, `cut`, `render`, `text`, `structured`,
`vector`. `README.md` explains the pipeline and the crate layout;
`docs/json-api.md` is the output contract.

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

**`pdf-core` depends on no other workspace crate.** That rule is what keeps the
dependency graph acyclic; the two chains are in `README.md`.

## Conventions

- `cargo fmt --all` and `cargo clippy --all-targets --workspace -- -D warnings`
  must both be clean.
- Comments explain *why*. The code says what it does; a comment earns its place
  by recording a constraint, an invariant, or a decision that is not obvious.
- Numbers in the docs are measured, not estimated. If you quote one, say where
  it came from and re-measure when the code changes.

## Running things

```sh
cargo test --workspace                      # 294 tests
./scripts/fetch-samples.sh                  # the 14-dossier corpus, ~198 MB
PDF_TOOLKIT_REQUIRE_CORPUS=1 cargo test     # corpus tests must run, not skip
./build.sh                                  # dist/ bundle; runs the full gate
```

Integration tests spawn the built binary and share one serialized `Pdfium`
handle: pdfium is not safe under interleaved multi-threaded use, and a panicking
test would otherwise poison the global lock. The harness binds
`<workspace>/dist/<library>` explicitly, because cargo runs an integration test
with the working directory at the *package* root, where the runtime's own
relative `./dist/` lookup cannot see it.

The corpus tests skip themselves when the dossiers are absent, so `cargo test`
passes on a fresh clone. Set `PDF_TOOLKIT_REQUIRE_CORPUS=1` in CI — a silent
skip there would hide exactly the regression the corpus exists to catch.

## Releasing

```sh
./build.sh                                  # dist/ bundle for this machine
./build.sh linux --with-artifacts           # plus artifacts/*.tar.gz + .sha256
SKIP_BUILD=1 ./build.sh --with-artifacts    # repackage without re-running cargo
```

`build.sh` runs `cargo fmt --check`, `clippy -D warnings`, `cargo test` and
`cargo build --locked --release` before it packages, so a release cannot be cut
from a tree that does not pass the gate. The archive carries the binary, the
matching PDFium library, `README.md` and `docs/json-api.md` — consumers unpack
it into their own `tools/` directory, where the contract is the thing they need
and cannot otherwise see.

**Tag with a bare version — `0.8.0`, not `v0.8.0`.** CI passes the tag through
to the archive name, and callers construct
`pdf-toolkit-<version>-<platform>.tar.gz` from a bare version. A `v` prefix
produces an asset name they cannot construct.

Pushing a tag runs `.github/workflows/release.yml`, which builds Linux and macOS
and creates the release. Windows is built on demand against a tag whose release
already exists.

**Release notes are written by hand, per tag**: add `.github/release-notes/<tag>.md`
before pushing tag `<tag>`, in the `## Added` / `## Changed` / `## Fixed` shape
already in that directory. Say there whatever `docs/json-api.md` records as a
contract change.

Callers that pin a version fetch the release asset, so a version bump is not
live for them until their pins are bumped too.
