---
name: release
description: "Build, package and release pdf-toolkit: build.sh variants, what the archive carries, tag naming, release.yml and the GitHub release, hand-written release notes, consumer pins. Use when the user asks to prepare, package or cut a release; never start a release unprompted."
---

# Releasing pdf-toolkit

The user cuts versions and releases. Prepare what they ask for; never bump
`version`, push a tag or publish a release on your own.

```sh
./build.sh                                  # dist/ bundle for this machine
./build.sh linux --with-artifacts           # plus artifacts/*.tar.gz + .sha256
SKIP_BUILD=1 ./build.sh --with-artifacts    # repackage without re-running cargo
SKIP_TESTS=1 ./build.sh --with-artifacts    # checks, clippy and build, no cargo test
```

`build.sh` runs `cargo fmt --check`, `clippy -D warnings`, `cargo test` and
`cargo build --locked --release` before it packages. Only a local run is
gated by `cargo test`: the CI release build sets `SKIP_TESTS: "1"`
(`.github/workflows/release.yml`), so run `./build.sh` locally before tagging.
The archive carries the binary, the matching PDFium library, the
`ocr-models/` directory, `README.md`, `docs/json-api.md` and
`docs/annotation-schema.md` — consumers unpack it into their own `tools/`
directory, where the contract is the thing they need and cannot otherwise see.

**Tag with a bare version — `0.8.0`, not `v0.8.0`.** CI passes the tag through
to the archive name, and callers construct
`pdf-toolkit-<version>-<platform>-<arch>.tar.gz` (e.g. `…-linux-x64`,
`…-macos-arm64`) from a bare version. A `v` prefix produces an asset name
they cannot construct.

Pushing a tag runs `.github/workflows/release.yml`, which builds Linux and
macOS and creates the GitHub release with the bundles attached. Windows is
built on demand against a tag whose release already exists.

**Release notes are written by hand, per tag**: add
`.github/release-notes/<tag>.md` before pushing tag `<tag>`, in the
`## Added` / `## Changed` / `## Fixed` shape already in that directory. Say
there whatever `docs/json-api.md` or `docs/service.md` records as a contract
change.

Callers that pin a version fetch the GitHub release bundle, so a version
bump is not live for them until their pins are bumped too.
