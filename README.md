# pdf_cutter

Small CLI tool for extracting pages from a PDF as single-page PDFs or PNGs.

## Build

`build.sh` creates a runtime bundle in `./dist` with:

- `pdf_cutter`
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

The build script downloads the matching PDFium binary and places it next to the executable in `./dist`.

## Usage

Run the bundled binary from `./dist`:

```bash
./dist/pdf_cutter --input input.pdf
```

Extract specific pages as separate PDFs:

```bash
./dist/pdf_cutter --input input.pdf --pages 1,3,5
```

Use page ranges:

```bash
./dist/pdf_cutter --input input.pdf --pages 1-3,5-6,9
./dist/pdf_cutter --input input.pdf --page 1-3,5-6,9
```

Render selected pages as PNG files:

```bash
./dist/pdf_cutter --input input.pdf --pages 1-3 --format png --scale 2 --output out
```

Concatenate selected pages into one PDF:

```bash
./dist/pdf_cutter --input input.pdf --pages 2-5,9 --concat
```

Print only the total number of pages in the document:

```bash
./dist/pdf_cutter --input input.pdf --doc-numbers
./dist/pdf_cutter --input input.pdf -n
```

## CLI Notes

- `--input` / `-i`: source PDF file
- `--pages` / `-p`: page list or inclusive ranges like `1-3,5,9-10`
- `--page`: alias for `--pages`
- `--format` / `-f`: `pdf` or `png`
- `--scale` / `-s`: required for `--format png`
- `--concat` / `-c`: merges selected pages into one PDF, only valid with PDF output
- `--output` / `-o`: output directory
- `--doc-numbers` / `-n`: prints the total page count and exits

Default behavior extracts all pages as separate PDF files in the current directory.
