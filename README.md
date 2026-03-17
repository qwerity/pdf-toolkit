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
./dist/pdf_cutter --input input.pdf --format pdf
```

Extract specific pages as separate PDFs:

```bash
./dist/pdf_cutter --input input.pdf --format pdf --pages 1,3,5
```

Use page ranges:

```bash
./dist/pdf_cutter --input input.pdf --format pdf --pages 1-3,5-6,9
./dist/pdf_cutter --input input.pdf --format pdf --page 1-3,5-6,9
```

Render selected pages as PNG files:

```bash
./dist/pdf_cutter --input input.pdf --pages 1-3 --format png --scale 2 --output out
```

Extract text for all pages into one file:

```bash
./dist/pdf_cutter --input input.pdf --extract --output out
```

Extract text for specific pages into separate files:

```bash
./dist/pdf_cutter --input input.pdf --extract --pages 1,3,5 --output out
```

Extract text for selected pages into one file:

```bash
./dist/pdf_cutter --input input.pdf --extract --pages 2-5,9 --concat --output out
```

Concatenate selected pages into one PDF:

```bash
./dist/pdf_cutter --input input.pdf --format pdf --pages 2-5,9 --concat
```

Print only the total number of pages in the document:

```bash
./dist/pdf_cutter --input input.pdf --page-count
./dist/pdf_cutter --input input.pdf -n
```

## CLI Notes

- `--input` / `-i`: source PDF file
- `--pages` / `-p`: page list or inclusive ranges like `1-3,5,9-10`
- `--page`: alias for `--pages`
- `--extract` / `-e`: extracts text into `.txt` file(s); `--pages all` and `--concat` produce one file
- `--format` / `-f`: required for page cutting; `pdf` or `png`; not valid with `--extract`
- `--scale` / `-s`: optional for `--format png`, defaults to `2.0`; not valid with `--extract`
- `--concat` / `-c`: merges selected pages into one PDF, or one text file when used with `--extract`
- `--output` / `-o`: output directory
- `--page-count` / `-n`: prints the total page count and exits as JSON
- extraction JSON includes an `errors` object keyed by page number when pages are skipped or fail

Use either `--extract` for text extraction or `--format` for page cutting.
