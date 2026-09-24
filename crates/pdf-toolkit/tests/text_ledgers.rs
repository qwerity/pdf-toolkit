//! The structured markdown, graded against what a person says each page of
//! `data/text` prints (`data/text/expected/<page>.json`): every table cell,
//! blank cells included; passages that must read contiguously; pages that
//! carry no table; the colour of coloured cells.
//!
//! The same grading as `scripts/text-bench.py`, which also scores pdftotext
//! and Docling on these pages; this test keeps the toolkit's own score from
//! going down. Every page is at full marks, so any lost fact fails the test
//! and names itself.

mod common;

use common::{data_dir, pdfium};
use pdf_toolkit::{PageSelection, StructuredOutputFormat, extract_structured_text};
use serde_json::Value;
use std::collections::HashSet;
use std::path::PathBuf;

fn squash(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| match c {
            '\u{2018}' | '\u{2019}' | '\u{201b}' | '\u{2032}' => '\'',
            '\u{201c}' | '\u{201d}' | '\u{201f}' | '\u{2033}' => '"',
            '\u{2010}'..='\u{2015}' | '\u{2212}' => '-',
            c => c,
        })
        .filter(|c| !c.is_whitespace())
        .collect()
}

/// Markdown without the markup that is not text.
fn plain(md: &str) -> String {
    md.lines()
        .map(|l| {
            let t = l.trim().trim_start_matches('|').trim();
            if t.starts_with("---") || t.starts_with(":--") {
                return String::new();
            }
            let l = l.trim_start();
            let l = l.trim_start_matches('#').trim_start();
            let l = l.strip_prefix("- ").unwrap_or(l);
            l.replace(['\\', '*', '|'], " ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn tables(md: &str) -> Vec<Vec<Vec<String>>> {
    let mut out = Vec::new();
    let mut current: Vec<Vec<String>> = Vec::new();
    for line in md.lines() {
        let s = line.trim();
        if s.starts_with('|') {
            let cells: Vec<String> = s
                .trim_matches('|')
                .split(" | ")
                .map(|c| plain(c.trim()).trim().to_string())
                .collect();
            if cells
                .iter()
                .all(|c| c.is_empty() || c.chars().all(|ch| ch == '-' || ch == ':'))
            {
                continue;
            }
            current.push(cells);
        } else if !current.is_empty() {
            out.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    // A table the page interrupts (a note printed across it) goes on under
    // its header repeated: one table.
    let mut merged: Vec<Vec<Vec<String>>> = Vec::new();
    for table in out {
        match merged.last_mut() {
            Some(last) if last[0] == table[0] && table[0].iter().any(|c| !c.is_empty()) => {
                last.extend(table.into_iter().skip(1))
            }
            _ => merged.push(table),
        }
    }
    merged
}

fn cell_ok(got: &str, want: &str, prefix: bool) -> bool {
    let (g, w) = (squash(got), squash(want));
    if w.is_empty() {
        g.is_empty()
    } else if prefix {
        g.starts_with(&w)
    } else {
        g == w
    }
}

/// (correct, total) for one ledger table: the best any markdown table scores.
fn score_table(md: &str, spec: &Value) -> (usize, usize) {
    let rows = spec["rows"].as_array().expect("rows");
    let total: usize = rows
        .iter()
        .map(|r| r["cells"].as_array().expect("cells").len())
        .sum();
    let prefix: Vec<u64> = spec["prefix_columns"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_u64).collect())
        .unwrap_or_default();
    let mut best = 0;
    for table in tables(md) {
        let mut header_rows: Vec<usize> = Vec::new();
        let index: Vec<Option<usize>> = match spec["columns"].as_array() {
            Some(labels) => {
                let mut index = vec![None; labels.len()];
                for exact in [true, false] {
                    for (j, label) in labels.iter().enumerate() {
                        if index[j].is_some() {
                            continue;
                        }
                        let label = squash(label.as_str().expect("label"));
                        'rows: for (r, row) in table.iter().take(3).enumerate() {
                            for (k, c) in row.iter().enumerate() {
                                let taken = index.contains(&Some(k));
                                let hit = if exact {
                                    squash(c) == label
                                } else {
                                    squash(c).contains(&label)
                                };
                                if hit && !taken {
                                    index[j] = Some(k);
                                    header_rows.push(r);
                                    break 'rows;
                                }
                            }
                        }
                    }
                }
                index
            }
            None => {
                let n = spec["column_count"].as_u64().expect("column_count") as usize;
                (0..n).map(|j| Some(j + 1)).collect()
            }
        };
        let body: Vec<&Vec<String>> = table
            .iter()
            .enumerate()
            .filter(|(r, _)| !header_rows.contains(r))
            .map(|(_, row)| row)
            .collect();
        let mut correct = 0;
        for r in rows {
            let label = squash(r["label"].as_str().expect("label"));
            let Some(row) = body.iter().find(|row| {
                row.iter().take(3).any(|c| {
                    let c = squash(c);
                    c == label || c.starts_with(&label)
                })
            }) else {
                continue;
            };
            for (j, want) in r["cells"].as_array().expect("cells").iter().enumerate() {
                if let Some(k) = index[j]
                    && k < row.len()
                    && cell_ok(
                        &row[k],
                        want.as_str().expect("cell"),
                        prefix.contains(&(j as u64)),
                    )
                {
                    correct += 1;
                }
            }
        }
        best = best.max(correct);
    }
    (best, total)
}

/// Whether some table names every column of `spec` in its first row: values
/// under a header lost to the data rows are values without names.
fn header_named(firsts: &[Vec<String>], spec: &Value) -> bool {
    let labels: Vec<String> = spec["columns"]
        .as_array()
        .expect("columns")
        .iter()
        .map(|l| squash(l.as_str().expect("label")))
        .collect();
    firsts.iter().any(|row| {
        labels
            .iter()
            .all(|label| row.iter().any(|c| c.contains(label.as_str())))
    })
}

/// Every line and every block between blank lines of the markdown, squashed.
fn units(md: &str) -> HashSet<String> {
    let text = plain(md);
    let mut units: HashSet<String> = HashSet::new();
    let mut block = String::new();
    for line in text.lines().chain(std::iter::once("")) {
        if line.trim().is_empty() {
            if !block.trim().is_empty() {
                units.insert(squash(&block));
            }
            block.clear();
        } else {
            units.insert(squash(line));
            block.push_str(line);
            block.push('\n');
        }
    }
    units
}

/// Every failed check of one page, as "check: correct/total".
fn grade(md: &str, ledger: &Value) -> Vec<String> {
    let mut failures = Vec::new();
    let mut report = |name: &str, (c, t): (usize, usize)| {
        if c < t {
            failures.push(format!("{name}: {c}/{t}"));
        }
    };
    if let Some(specs) = ledger["tables"].as_array() {
        let got = specs
            .iter()
            .map(|s| score_table(md, s))
            .fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1));
        report("cells", got);
        let named: Vec<&Value> = specs.iter().filter(|s| s["columns"].is_array()).collect();
        let firsts: Vec<Vec<String>> = tables(md)
            .iter()
            .map(|t| t[0].iter().map(|c| squash(c)).collect())
            .collect();
        let headed = named.iter().filter(|s| header_named(&firsts, s)).count();
        report("headers", (headed, named.len()));
    }
    if let Some(separate) = ledger["separate"].as_array() {
        let found = units(md);
        let ok = separate
            .iter()
            .filter(|p| found.contains(&squash(p.as_str().expect("separate"))))
            .count();
        report("separate", (ok, separate.len()));
    }
    if let Some(passages) = ledger["passages"].as_array() {
        let body = squash(&plain(md));
        let found = passages
            .iter()
            .filter(|p| body.contains(&squash(p.as_str().expect("passage"))))
            .count();
        report("passages", (found, passages.len()));
    }
    if ledger["no_tables"].as_bool() == Some(true) {
        report("no_tables", (usize::from(tables(md).is_empty()), 1));
    }
    if let Some(chips) = ledger["chips"].as_array() {
        let text = plain(md).to_lowercase();
        let found = chips
            .iter()
            .filter(|chip| {
                let label = chip["text"].as_str().expect("text").to_lowercase();
                let colour = chip["colour"].as_str().expect("colour");
                text.contains(&format!("{label} [{colour}]"))
            })
            .count();
        report("chips", (found, chips.len()));
    }
    failures
}

#[test]
fn structured_markdown_reads_every_ledger_fact() {
    let dir = data_dir().join("text");
    let mut ledgers: Vec<PathBuf> = std::fs::read_dir(dir.join("expected"))
        .map(|entries| entries.filter_map(|e| e.ok().map(|e| e.path())).collect())
        .unwrap_or_default();
    ledgers.sort();
    let pdfs: Vec<PathBuf> = ledgers
        .iter()
        .map(|l| {
            dir.join(format!(
                "{}.pdf",
                l.file_stem().expect("stem").to_string_lossy()
            ))
        })
        .collect();
    if ledgers.is_empty()
        || !common::corpus_available("structured_markdown_reads_every_ledger_fact", &pdfs)
    {
        return;
    }

    let pdfium = pdfium();
    let mut failures = Vec::new();
    for (ledger_path, pdf) in ledgers.iter().zip(&pdfs) {
        let ledger: Value =
            serde_json::from_str(&std::fs::read_to_string(ledger_path).expect("read ledger"))
                .expect("ledger parses");
        let report = extract_structured_text(
            &pdfium,
            pdf,
            PageSelection::parse("all").expect("selection"),
            StructuredOutputFormat::MarkdownOnly,
        )
        .expect("structured extraction");
        let md: String = report
            .pages
            .iter()
            .map(|p| p.markdown.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        for failure in grade(&md, &ledger) {
            failures.push(format!(
                "{}: {failure}",
                pdf.file_name().expect("name").to_string_lossy()
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "facts the markdown lost:\n  {}",
        failures.join("\n  ")
    );
}
