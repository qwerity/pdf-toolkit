#!/usr/bin/env python3
"""Scores text extractors against the reading of a page a person wrote down.

    python3 scripts/text-bench.py [--toolkit BIN ...] [--fetch-docling] [--show ID]

Every page in data/text/<id>.pdf has a ledger in data/text/expected/<id>.json
saying what is printed on it: the value in each table cell (blank cells
included, since a value moved into a blank cell is the error that matters),
passages that must read contiguously, whether the page has no table at all,
and the colour of coloured cells. Each extractor's output is graded against
that ledger, never against another extractor's output.

Extractors:
  pdftotext   `pdftotext -layout` (poppler), scored by column position
  <toolkit>   `pdf-toolkit structured --structured-format markdown`, one per
              --toolkit binary (label = the binary's version, or its path)
  docling     the Docling server's markdown, cached in
              data/text/results/docling/<id>.md; --fetch-docling refreshes it
              from DOCLING_SERVICE_URL with DOCLING_AUTH_TOKEN

Standard library only, like the other scripts here.
"""
import argparse
import json
import os
import re
import subprocess
import sys
import urllib.request
import uuid
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DATA = ROOT / 'data' / 'text'
DOCLING_CACHE = DATA / 'results' / 'docling'


def squash(s):
    s = s.lower()
    s = re.sub(r"[‘’‛′]", "'", s)
    s = re.sub(r'[“”‟″]', '"', s)
    s = re.sub(r'[‐-―−]', '-', s)
    return re.sub(r'\s+', '', s)


def plain_markdown(md):
    """Markdown without the markup that is not text: escapes, emphasis, bullets, pipes, headings."""
    md = md.replace('\\', '')
    md = re.sub(r'\*+', '', md)
    md = re.sub(r'(?m)^\s*#+\s*', '', md)
    md = re.sub(r'(?m)^\s*[-+]\s+', '', md)
    md = re.sub(r'(?m)^\s*\|?\s*-{3,}.*$', '', md)
    return md.replace('|', ' ')


# ── Extractors ─────────────────────────────────────────────────────

def run_pdftotext(pdf):
    return subprocess.run(['pdftotext', '-q', '-layout', str(pdf), '-'], capture_output=True, text=True, check=True).stdout


def run_toolkit(binary, pdf, ocr_models=None):
    ocr = ['--ocr', 'scans', '--ocr-models', ocr_models] if ocr_models else []
    out = subprocess.run([binary, 'structured', str(pdf), '--structured-format', 'markdown', *ocr],
                         capture_output=True, text=True, check=True).stdout
    return '\n\n'.join(p['markdown'] for p in json.loads(out)['pages'])


def toolkit_label(binary):
    try:
        return subprocess.run([binary, '--version'], capture_output=True, text=True).stdout.split()[-1]
    except (OSError, IndexError):
        return binary


def fetch_docling(pdf, url, token):
    boundary = uuid.uuid4().hex
    body = (f'--{boundary}\r\nContent-Disposition: form-data; name="file"; filename="{pdf.name}"\r\n'
            'Content-Type: application/pdf\r\n\r\n').encode() + pdf.read_bytes() + f'\r\n--{boundary}--\r\n'.encode()
    req = urllib.request.Request(f'{url}?chunk=false&per_page=false&full_markdown=true', data=body, method='POST',
                                 headers={'Content-Type': f'multipart/form-data; boundary={boundary}',
                                          'Authorization': f'Bearer {token}'})
    with urllib.request.urlopen(req, timeout=600) as res:
        return json.loads(res.read())['markdown'] or ''


# ── Markdown tables ────────────────────────────────────────────────

def markdown_tables(md):
    """Every markdown table as a list of rows of cell strings (separator rows dropped)."""
    tables, current = [], []
    for line in md.splitlines():
        s = line.strip()
        if s.startswith('|'):
            cells = [c.strip() for c in re.split(r'(?<!\\)\|', s.strip('|'))]
            if all(re.fullmatch(r':?-+:?', c) or c == '' for c in cells) and any(cells):
                continue
            current.append([plain_markdown(c).strip() for c in cells])
        elif current:
            tables.append(current)
            current = []
    if current:
        tables.append(current)
    # A table the page interrupts (a note printed across it) goes on under
    # its header repeated: one table.
    merged = []
    for table in tables:
        if merged and table[0] == merged[-1][0] and any(table[0]):
            merged[-1].extend(table[1:])
        else:
            merged.append(table)
    return merged


def row_matches(cell_texts, label):
    lab = squash(label)
    return any(squash(c) == lab or squash(c).startswith(lab) for c in cell_texts)


def cell_ok(got, want, prefix):
    g, w = squash(got), squash(want)
    if not w:
        return g == ''
    return g.startswith(w) if prefix else g == w


def score_markdown_table(md, spec):
    """(correct, total) for one ledger table against every markdown table in the output."""
    total = sum(len(r['cells']) for r in spec['rows'])
    prefix = set(spec.get('prefix_columns', []))
    best = 0
    for table in markdown_tables(md):
        header_rows = set()
        if spec['columns']:
            # A column is found by its label in the first rows (headers may span
            # two, or be merged as "Urban Residential - Ground Story"): an exact
            # cell first, else a cell containing the label that no other label took.
            index = {}
            for exact in (True, False):
                for j, label in enumerate(spec['columns']):
                    if j in index:
                        continue
                    for r_i, row in enumerate(table[:3]):
                        hits = [k for k, c in enumerate(row) if k not in index.values()
                                and (squash(c) == squash(label) if exact else squash(label) in squash(c))]
                        if hits:
                            index[j] = hits[0]
                            header_rows.add(r_i)
                            break
        else:
            # Values right after the row label, or, with `values_at_end`, the
            # table's last columns whatever label columns precede them.
            first = len(table[0]) - spec['column_count'] if spec.get('values_at_end') else 1
            index = {j: first + j for j in range(spec['column_count'])}
        body = [row for r_i, row in enumerate(table) if r_i not in header_rows]
        correct = 0
        for r in spec['rows']:
            row = next((row for row in body if row_matches(row[:3], r["label"])), None)
            if row is None:
                continue
            for j, want in enumerate(r['cells']):
                k = index.get(j)
                if k is not None and k < len(row) and cell_ok(row[k], want, j in prefix):
                    correct += 1
        best = max(best, correct)
    return best, total


# ── Layout text ────────────────────────────────────────────────────

def score_layout_table(text, spec):
    """(correct, total): a value counts only when it sits under its column's header."""
    lines = text.splitlines()
    total = sum(len(r['cells']) for r in spec['rows'])
    prefix = set(spec.get('prefix_columns', []))
    if spec['columns']:
        first = next((i for i, l in enumerate(lines) if spec['columns'][0] in l), None)
        if first is None:
            return 0, total
        # A header may stack its labels over a few lines ("Additional Regulations"
        # beside a two-line "Urban Residential / Ground Story"), so each label is
        # searched in a window around the first one; on one line, labels are read
        # left to right, so "Afternoon" is not found inside "Late Afternoon".
        spans = []
        for label in spec['columns']:
            found = None
            for i in range(max(first - 3, 0), min(first + 4, len(lines))):
                after = max((s[2] for s in spans if s[0] == i), default=-1) + 1
                col = lines[i].find(label, after)
                if col >= 0 and (col == 0 or not lines[i][col - 1].isalnum()):
                    found = (i, col, col + len(label))
                    break
            if found is None:
                return 0, total
            spans.append(found)
        header_line = max(s[0] for s in spans)
        cols = [(s[1], s[2]) for s in spans]
    else:
        anchor = next((i for i, l in enumerate(lines) if spec['columns_from_row'] in l), None)
        if anchor is None:
            return 0, total
        groups = [(m.start(), m.end()) for m in re.finditer(r'\S+(?: \S+)*', lines[anchor])]
        cols, header_line = groups[1:1 + spec['column_count']], anchor - 1
    bounds = []
    for j, (s, e) in enumerate(cols):
        left = (cols[j - 1][1] + s) // 2 if j else s - 2
        right = (e + cols[j + 1][0]) // 2 if j + 1 < len(cols) else 10_000
        bounds.append((max(left, 0), right))
    correct = 0
    for r in spec['rows']:
        idx = next((i for i in range(header_line + 1, len(lines))
                    if row_matches(re.split(r'\s{2,}', lines[i].strip()), r['label'])), None)
        if idx is None:
            continue
        for j, want in enumerate(r['cells']):
            left, right = bounds[j]
            if cell_ok(lines[idx][left:right], want, j in prefix):
                correct += 1
    return correct, total


# ── Scoring ────────────────────────────────────────────────────────

COLOURS = ('red', 'orange', 'yellow', 'green', 'blue', 'purple', 'pink', 'brown', 'grey')


def header_named(firsts, spec):
    """1 when one of `firsts` (each table's first row, squashed; for a layout
    text, each line) names every column of the spec, else 0 — values under a
    header lost to the data rows are values without names."""
    labels = [squash(label) for label in spec['columns']]
    return int(any(all(any(label in c for c in row) for label in labels) for row in firsts))


def units(text):
    """Every line and every blank-line-separated block of a text, squashed."""
    blocks = re.split(r'\n\s*\n', text)
    return {squash(u) for u in text.splitlines() + blocks if u.strip()}


def score(output, ledger, kind):
    """{check: (correct, total)} for one output against one ledger."""
    result = {}
    text = plain_markdown(output) if kind == 'markdown' else output
    tables = ledger.get('tables', [])
    if tables:
        got = [(score_layout_table if kind == 'layout' else score_markdown_table)(output, t) for t in tables]
        result['cells'] = (sum(g[0] for g in got), sum(g[1] for g in got))
        named = [t for t in tables if t['columns']]
        if named:
            firsts = ([[squash(c) for c in t[0]] for t in markdown_tables(output)] if kind == 'markdown'
                      else [[squash(line)] for line in output.splitlines()])
            result['headers'] = (sum(header_named(firsts, t) for t in named), len(named))
    if ledger.get('passages'):
        # A scan's ledger may excuse a passage OCR cannot read as printed.
        passages = [p for p in ledger['passages'] if p not in ledger.get('ocr_misses', [])]
        body = squash(text)
        result['passages'] = (sum(squash(p) in body for p in passages), len(passages))
    if ledger.get('separate'):
        found = units(text)
        result['separate'] = (sum(squash(p) in found for p in ledger['separate']), len(ledger['separate']))
    if ledger.get('no_tables'):
        invented = kind == 'markdown' and bool(markdown_tables(output))
        result['no_tables'] = (0 if invented else 1, 1)
    if ledger.get('chips'):
        ok = 0
        for chip in ledger['chips']:
            pattern = re.escape(chip['text']) + r'\s*[\[(]\s*(' + '|'.join(COLOURS) + r')\s*[\])]'
            m = re.search(r'(?<![\w-])' + pattern, text, re.IGNORECASE)
            ok += bool(m and m.group(1).lower() == chip['colour'])
        result['chips'] = (ok, len(ledger['chips']))
    return result


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--toolkit', action='append', default=[], help='pdf-toolkit binary; repeat to compare builds')
    ap.add_argument('--fetch-docling', action='store_true')
    ap.add_argument('--show', help='print every extractor\'s output for one page id')
    ap.add_argument('--json', help='write the scores to this file')
    ap.add_argument('--pdftotext', action='store_true', help='also grade `pdftotext -layout`')
    ap.add_argument('--ocr-models', help='run the toolkit with `--ocr scans` and these OCR models '
                    '(pages whose ledger says `needs_ocr` read their scan)')
    args = ap.parse_args()

    ids = sorted(p.stem for p in (DATA / 'expected').glob('*.json'))
    extractors = [('pdftotext', 'layout', run_pdftotext)] if args.pdftotext else []
    for binary in args.toolkit:
        name = f'tk {toolkit_label(binary)} {Path(binary).parent.name}'
        extractors.append((name, 'markdown', lambda pdf, b=binary: run_toolkit(b, pdf, args.ocr_models)))

    if args.fetch_docling:
        url, token = os.environ.get('DOCLING_SERVICE_URL'), os.environ.get('DOCLING_AUTH_TOKEN')
        if not url or not token:
            sys.exit('--fetch-docling needs DOCLING_SERVICE_URL and DOCLING_AUTH_TOKEN')
        DOCLING_CACHE.mkdir(parents=True, exist_ok=True)
        for i in ids:
            (DOCLING_CACHE / f'{i}.md').write_text(fetch_docling(DATA / f'{i}.pdf', url, token))
    if all((DOCLING_CACHE / f'{i}.md').exists() for i in ids):
        extractors.append(('docling', 'markdown', lambda pdf: (DOCLING_CACHE / f'{pdf.stem}.md').read_text()))

    scores = {}
    for name, kind, run in extractors:
        for i in ids:
            out = run(DATA / f'{i}.pdf')
            if args.show == i:
                print(f'\n===== {name} =====\n{out}')
            scores.setdefault(name, {})[i] = score(out, json.loads((DATA / 'expected' / f'{i}.json').read_text()), kind)

    names = [e[0] for e in extractors]
    width = max(len(i) for i in ids)
    print(f"{'page'.ljust(width)}  check      " + ''.join(n[:16].rjust(17) for n in names))
    totals = {n: [0, 0] for n in names}
    for i in ids:
        for check in scores[names[0]][i]:
            row = f'{i.ljust(width)}  {check.ljust(10)} '
            for n in names:
                c, t = scores[n][i][check]
                totals[n][0] += c
                totals[n][1] += t
                row += f'{c:>6}/{t:<4} {100 * c // t:>3}%  '
            print(row)
    print(f"{'ALL'.ljust(width)}  {'':10} " + ''.join(
        f'{totals[n][0]:>6}/{totals[n][1]:<4} {100 * totals[n][0] // totals[n][1]:>3}%  ' for n in names))
    if args.json:
        Path(args.json).write_text(json.dumps(scores, indent=2) + '\n')


if __name__ == '__main__':
    main()
