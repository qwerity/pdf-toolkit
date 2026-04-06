# pdf_cutter

CLI tool for inspecting PDFs, extracting pages as single-page PDFs or PNGs, and extracting text.

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

Render PNG files with a required output width:

```bash
./dist/pdf_cutter --input input.pdf --pages 1-3 --format png --scale-width-px 1400 --output out
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

Print page count plus per-page metadata:

```bash
./dist/pdf_cutter --input input.pdf --page-count
./dist/pdf_cutter --input input.pdf -n
```

The CLI writes structured JSON to stdout. Human-readable warnings are written to stderr.

## CLI Notes

- `--input` / `-i`: source PDF file
- `--pages` / `-p`: page list or inclusive ranges like `1-3,5,9-10`
- `--page`: alias for `--pages`
- `--extract` / `-e`: extracts text into `.txt` file(s); `--pages all` and `--concat` produce one file
- `--format` / `-f`: required for page cutting; `pdf` or `png`; not valid with `--extract`
- `--scale` / `-s`: optional for `--format png`; if neither sizing option is provided it defaults to `2.0`; not valid with `--extract`
- `--scale-width-px`: optional for `--format png`; sets the output PNG width in pixels and preserves aspect ratio; cannot be used together with `--scale`
- `--concat` / `-c`: merges selected pages into one PDF, or one text file when used with `--extract`
- `--output` / `-o`: output directory
- `--page-count` / `-n`: prints the total page count plus per-page `width_px`, `height_px`, and `contains_images` metadata as JSON
- extracted text starts each page with a `____PageN____` marker
- page extraction JSON includes per-page `width_px`, `height_px`, and `contains_images`
- inspection and PDF extraction report page dimensions in pixels at render scale `1.0`
- PNG extraction reports the actual rendered output dimensions in pixels for the chosen `--scale` or `--scale-width-px`
- extraction JSON includes an `errors` object keyed by page number when pages are skipped or fail

Use either `--extract` for text extraction or `--format` for page cutting.

## Output Examples

Page inspection with `--page-count`:

```json
{
  "page_count": 2,
  "pages": {
    "1": {
      "width_px": 595,
      "height_px": 842,
      "contains_images": false
    },
    "2": {
      "width_px": 842,
      "height_px": 595,
      "contains_images": true
    }
  }
}
```

Per-page PDF or PNG extraction:

```json
{
  "pages": {
    "1": {
      "path": "/abs/path/out/input_page_1.pdf",
      "size_kb": 12,
      "width_px": 595,
      "height_px": 842,
      "contains_images": true
    }
  }
}
```

PNG extraction with scale also includes the render scale:

```json
{
  "scale": 2.0,
  "pages": {
    "1": {
      "path": "/abs/path/out/input_page_1.png",
      "size_kb": 184,
      "width_px": 1191,
      "height_px": 1684,
      "contains_images": true
    }
  }
}
```

PNG extraction with a requested width:

```json
{
  "scale_width_px": 1400,
  "pages": {
    "1": {
      "path": "/abs/path/out/input_page_1.png",
      "size_kb": 184,
      "width_px": 1400,
      "height_px": 1980,
      "contains_images": true
    }
  }
}
```

Concatenated PDF output:

```json
{
  "concat": {
    "path": "/abs/path/out/input_concat.pdf",
    "pages": "2-5,9",
    "size_kb": 96
  },
  "page_info": {
    "2": {
      "width_px": 595,
      "height_px": 842,
      "contains_images": false
    },
    "3": {
      "width_px": 595,
      "height_px": 842,
      "contains_images": true
    }
  }
}
```

Text extraction:

```json
{
  "pages": {
    "1": {
      "path": "/abs/path/out/input_page_1.txt",
      "size_kb": 4
    }
  }
}
```

If requested pages are skipped or fail, the JSON includes an `errors` object keyed by page number.
