//! The extraction scorecard: every kind of thing the toolkit reads off the
//! corpus, graded against a per-document ledger of what it should read, and
//! ratcheted against a committed baseline.
//!
//! The golden tests beside this one ask "is the output byte-identical to the
//! reviewed annotation?" — which a change to any field answers with "no",
//! and regenerating the annotation then answers with "yes" whatever else
//! changed. This test asks the question a change actually raises: for each
//! extraction — page classification, labels, routing, trust, entities,
//! groups, the request block, georeferencing, the facts, the cross-page
//! checks, the storey model — how much of what a person said is on the page
//! does the toolkit now read, and is that more or less than last time?
//!
//! The ledgers live beside the corpus (`data/expected/<n>.json`,
//! `scripts/seed-expected.py` seeds them from the reviewed annotations). The
//! baseline — numbers only — is committed beside this file. A kind whose
//! score falls below its baseline fails the test; a kind that rose is
//! reported, and `PDF_TOOLKIT_SCORECARD_UPDATE=1` makes the new numbers the
//! baseline. Improve, never regress: the ratchet.

mod cli;
mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use cli::run_argv;
use serde_json::{Value, json};

/// Scores below the baseline by more than this fail. Scores are means over a
/// document's pages or items, so a change of one item shows far above it.
const TOLERANCE: f64 = 1e-6;

const KINDS: &[&str] = &[
    "classification",
    "labels",
    "route",
    "trust",
    "entities",
    "groups",
    "request",
    "georeference",
    "facts",
    "checks",
    "model",
    "building",
];

fn baseline_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("scorecard")
        .join("baseline.json")
}

fn expected_path(n: u32) -> PathBuf {
    common::data_dir()
        .join("expected")
        .join(format!("{n}.json"))
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).expect("readable json"))
        .expect("valid json")
}

fn mean(values: &[f64]) -> Option<f64> {
    (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
}

fn jaccard(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f64 {
    let union = a.union(b).count();
    if union == 0 {
        return 1.0;
    }
    a.intersection(b).count() as f64 / union as f64
}

/// The harmonic mean of precision and recall over two sets; 1 when both are empty.
fn f1(expected: &BTreeSet<String>, found: &BTreeSet<String>) -> f64 {
    if expected.is_empty() && found.is_empty() {
        return 1.0;
    }
    let hit = expected.intersection(found).count() as f64;
    if hit == 0.0 {
        return 0.0;
    }
    let precision = hit / found.len() as f64;
    let recall = hit / expected.len() as f64;
    2.0 * precision * recall / (precision + recall)
}

fn strings(value: &Value) -> BTreeSet<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn within(found: f64, expected: f64, fraction: f64, floor: f64) -> bool {
    (found - expected).abs() <= (expected.abs() * fraction).max(floor)
}

fn page_by_number<'a>(report: &'a Value, number: &str) -> Option<&'a Value> {
    report["pages"]
        .as_array()?
        .iter()
        .find(|p| p["page_number"].as_u64().map(|n| n.to_string()) == Some(number.to_string()))
}

/// Per-page attributes: one score per page, averaged.
fn score_pages(expected: &Value, report: &Value, attribute: &str) -> Option<f64> {
    let pages = expected["pages"].as_object()?;
    let mut scores = Vec::new();
    for (number, want) in pages {
        let Some(want_value) = want.get(attribute) else {
            continue;
        };
        let found = page_by_number(report, number);
        let score = match attribute {
            "labels" => jaccard(
                &strings(want_value),
                &found.map(|p| strings(&p["labels"])).unwrap_or_default(),
            ),
            "class" => f64::from(
                found.and_then(|p| p["classification"]["class"].as_str()) == want_value.as_str(),
            ),
            "route" => f64::from(
                found.and_then(|p| p["extraction"]["route"].as_str()) == want_value.as_str(),
            ),
            "trust" => f64::from(found.and_then(|p| p["trust"].as_str()) == want_value.as_str()),
            _ => unreachable!(),
        };
        scores.push(score);
    }
    mean(&scores)
}

fn score_entities(expected: &Value, report: &Value) -> Option<f64> {
    let want = strings(&expected["entities"]);
    let found: BTreeSet<String> = report["entities"]
        .as_array()?
        .iter()
        .filter_map(|e| e["id"].as_str().map(str::to_string))
        .collect();
    Some(f1(&want, &found))
}

/// Each expected group against its best match: pages by Jaccard, halved for
/// a wrong kind, halved for a wrong trust.
fn score_groups(expected: &Value, report: &Value) -> Option<f64> {
    let want = expected["groups"].as_array()?;
    let found = report["groups"].as_array()?;
    let pages_of = |g: &Value| -> BTreeSet<String> {
        g["pages"]
            .as_array()
            .map(|p| p.iter().map(|n| n.to_string()).collect())
            .unwrap_or_default()
    };
    let scores: Vec<f64> = want
        .iter()
        .map(|w| {
            found
                .iter()
                .map(|f| {
                    let mut s = jaccard(&pages_of(w), &pages_of(f));
                    if w["kind"] != f["kind"] {
                        s *= 0.5;
                    }
                    if w["trust"] != f["trust"] {
                        s *= 0.5;
                    }
                    s
                })
                .fold(0.0, f64::max)
        })
        .collect();
    mean(&scores)
}

fn score_request(expected: &Value, report: &Value) -> Option<f64> {
    let want = expected.get("request")?;
    let found = &report["request"];
    if want.is_null() {
        return Some(f64::from(found.is_null()));
    }
    let fields = want.as_object()?;
    let hits: Vec<f64> = fields
        .iter()
        .map(|(key, value)| {
            if key == "works" {
                f64::from(strings(value) == strings(&found[key]))
            } else {
                f64::from(&found[key] == value)
            }
        })
        .collect();
    mean(&hits)
}

/// The placed pages as a set (F1), and each expected survey polygon's area
/// within half a percent.
fn score_georeference(expected: &Value, report: &Value) -> Option<f64> {
    let geo = expected.get("georeference")?;
    let want: BTreeSet<String> = geo["placed_pages"]
        .as_array()?
        .iter()
        .map(|n| n.to_string())
        .collect();
    let found: BTreeSet<String> = report["pages"]
        .as_array()?
        .iter()
        .filter(|p| !p["georeference"].is_null())
        .filter_map(|p| p["page_number"].as_u64().map(|n| n.to_string()))
        .collect();
    let mut parts = vec![f1(&want, &found)];
    if let Some(areas) = geo["survey_area_m2"].as_object()
        && !areas.is_empty()
    {
        let facts = report["facts"].as_array().cloned().unwrap_or_default();
        let hits: Vec<f64> = areas
            .iter()
            .map(|(page, area)| {
                let expected_area = area.as_f64().unwrap_or(0.0);
                f64::from(facts.iter().any(|f| {
                    f["kind"] == "survey_area"
                        && f["page"].as_u64().map(|n| n.to_string()) == Some(page.clone())
                        && f["number"]
                            .as_f64()
                            .is_some_and(|a| within(a, expected_area, 0.005, 0.05))
                }))
            })
            .collect();
        parts.push(mean(&hits).unwrap_or(1.0));
    }
    mean(&parts)
}

/// A fact as a comparable key: kind, page, subject, and the value rounded.
fn fact_key(fact: &Value) -> String {
    let value = match (fact["number"].as_f64(), fact["text"].as_str()) {
        (Some(n), _) => format!("{:.2}", n),
        (None, Some(t)) => t.to_string(),
        (None, None) => String::new(),
    };
    format!(
        "{}|{}|{}|{}",
        fact["kind"].as_str().unwrap_or(""),
        fact["page"],
        fact["subject"].as_str().unwrap_or(""),
        value
    )
}

fn score_facts(expected: &Value, report: &Value) -> Option<f64> {
    let want: BTreeSet<String> = expected["facts"].as_array()?.iter().map(fact_key).collect();
    let found: BTreeSet<String> = report["facts"]
        .as_array()
        .map(|f| f.iter().map(fact_key).collect())
        .unwrap_or_default();
    Some(f1(&want, &found))
}

fn check_key(check: &Value) -> String {
    format!(
        "{}|{}|{}",
        check["kind"].as_str().unwrap_or(""),
        check["subject"].as_str().unwrap_or(""),
        check["status"].as_str().unwrap_or("")
    )
}

/// Every expected check found with the same status — and no check the
/// ledger does not expect (a new false positive is a regression too).
fn score_checks(expected: &Value, report: &Value) -> Option<f64> {
    let want: BTreeSet<String> = expected["checks"]
        .as_array()?
        .iter()
        .map(check_key)
        .collect();
    let found: BTreeSet<String> = report["checks"]
        .as_array()
        .map(|c| c.iter().map(check_key).collect())
        .unwrap_or_default();
    Some(f1(&want, &found))
}

/// The storey model's facts within tolerance: levels to a centimetre, areas
/// to one percent, counts and placement exactly.
fn score_model(expected: &Value, model: Option<&Value>) -> Option<f64> {
    let want = expected.get("model")?;
    if want.is_null() {
        return None;
    }
    let Some(found) = model else {
        return Some(0.0);
    };
    let found_floors = found["floors"].as_array().cloned().unwrap_or_default();
    // A ledger that only knows what the drawings PRINT — the storey count and
    // the ground floor's footprint — states them as scalars; a ledger seeded
    // from an accepted run states every storey.
    if let Some(count) = want["floors"].as_u64() {
        let mut hits = vec![f64::from(found_floors.len() as u64 == count)];
        if let Some(area) = want["ground_slab_area_m2"].as_f64() {
            let ground = found_floors
                .first()
                .and_then(|f| f["slab_area_m2"].as_f64())
                .is_some_and(|a| within(a, area, 0.1, 1.0));
            hits.push(f64::from(ground));
        }
        return mean(&hits);
    }
    let mut hits: Vec<f64> = vec![f64::from(found["placed"] == want["placed"])];
    let want_floors = want["floors"].as_array()?;
    hits.push(f64::from(want_floors.len() == found_floors.len()));
    for (w, f) in want_floors.iter().zip(&found_floors) {
        for (key, fraction, floor) in [
            ("level_m", 0.0, 0.011),
            ("top_m", 0.0, 0.011),
            ("slab_area_m2", 0.01, 0.05),
            ("rooms_area_m2", 0.02, 0.1),
        ] {
            let (Some(a), Some(b)) = (w[key].as_f64(), f[key].as_f64()) else {
                hits.push(0.0);
                continue;
            };
            hits.push(f64::from(within(b, a, fraction, floor)));
        }
    }
    if let Some(levels) = want["levels"].as_object() {
        for (name, level) in levels {
            let hit = found["model"]["levels"][name]
                .as_f64()
                .zip(level.as_f64())
                .is_some_and(|(f, w)| within(f, w, 0.0, 0.011));
            hits.push(f64::from(hit));
        }
    }
    if let Some(roof) = want["roof_area_m2"].as_f64() {
        hits.push(f64::from(
            found["roof"]["area_m2"]
                .as_f64()
                .is_some_and(|a| within(a, roof, 0.01, 0.05)),
        ));
    }
    hits.push(f64::from(found["stone_piers"] == want["stone_piers"]));
    mean(&hits)
}

/// The hosted `building` reconstruction, scored from its RECORDED report
/// (`data/annotations/building/<n>.json`, written by a run someone paid for
/// and reviewed) rather than a live call: the model's massing is graded
/// against what the drawings print — one part per building, its height to
/// 0.3 m, its footprint to ten percent, and whether it was placed.
fn score_building(expected: &Value, recorded: Option<&Value>) -> Option<f64> {
    let want = expected.get("building")?;
    if want.is_null() {
        return None;
    }
    let Some(found) = recorded else {
        return Some(0.0);
    };
    let mut hits: Vec<f64> = vec![f64::from(found["placed"] == want["placed"])];
    let want_parts = want["parts"].as_array()?;
    let found_parts = found["parts"].as_array().cloned().unwrap_or_default();
    hits.push(f64::from(want_parts.len() == found_parts.len()));
    for w in want_parts {
        let best = found_parts
            .iter()
            .filter(|f| f["name"] == w["name"] || found_parts.len() == 1)
            .map(|f| {
                let height = f["height_m"]
                    .as_f64()
                    .zip(w["height_m"].as_f64())
                    .is_some_and(|(a, b)| within(a, b, 0.0, 0.3));
                let footprint = f["footprint_area"]
                    .as_f64()
                    .zip(w["footprint_area"].as_f64())
                    .is_some_and(|(a, b)| within(a, b, 0.1, 1.0));
                (f64::from(height) + f64::from(footprint)) / 2.0
            })
            .fold(0.0, f64::max);
        hits.push(best);
    }
    mean(&hits)
}

fn score_document(
    expected: &Value,
    dossier: &Value,
    model: Option<&Value>,
    building: Option<&Value>,
) -> BTreeMap<&'static str, f64> {
    let mut scores = BTreeMap::new();
    let mut put = |kind: &'static str, score: Option<f64>| {
        if let Some(s) = score {
            scores.insert(kind, (s * 1e4).round() / 1e4);
        }
    };
    put("classification", score_pages(expected, dossier, "class"));
    put("labels", score_pages(expected, dossier, "labels"));
    put("route", score_pages(expected, dossier, "route"));
    put("trust", score_pages(expected, dossier, "trust"));
    put("entities", score_entities(expected, dossier));
    put("groups", score_groups(expected, dossier));
    put("request", score_request(expected, dossier));
    put("georeference", score_georeference(expected, dossier));
    put("facts", score_facts(expected, dossier));
    put("checks", score_checks(expected, dossier));
    put("model", score_model(expected, model));
    put("building", score_building(expected, building));
    scores
}

fn run_dossier(n: u32, out: &Path) -> Value {
    let answers = common::data_dir()
        .join("annotations")
        .join("answers")
        .join(format!("{n}.json"));
    let argv = if answers.is_file() {
        cli::argv::dossier_merged(
            &format!("data/plan/{n}.pdf"),
            answers.to_str().expect("utf-8"),
            out.to_str().expect("utf-8"),
        )
    } else {
        cli::argv::dossier(&format!("data/plan/{n}.pdf"))
    };
    run_argv(&argv).expect_success().json()
}

/// The storey model from the recorded parameters where a reviewed run
/// accepted some (`data/annotations/model/<n>.params.json` — the hosted
/// proposal, checked and kept, so the reconstruction stays deterministic),
/// else from detection with the answers' page labels.
fn run_model(n: u32) -> Option<Value> {
    let annotations = common::data_dir().join("annotations");
    let params = annotations.join("model").join(format!("{n}.params.json"));
    let answers = annotations.join("answers").join(format!("{n}.json"));
    let parcel = annotations.join("parcels").join(format!("{n}.geojson"));
    let mut argv = vec!["model".to_string(), format!("data/plan/{n}.pdf")];
    if params.is_file() {
        argv.push("--params".into());
        argv.push(params.to_str().expect("utf-8").into());
    } else if answers.is_file() {
        argv.push("--answers".into());
        argv.push(answers.to_str().expect("utf-8").into());
    }
    if parcel.is_file() {
        argv.push("--parcel".into());
        argv.push(parcel.to_str().expect("utf-8").into());
    }
    let run = run_argv(&argv);
    run.succeeded().then(|| run.json())
}

/// The recorded `building` report, when a run was kept.
fn recorded_building(n: u32) -> Option<Value> {
    let path = common::data_dir()
        .join("annotations")
        .join("building")
        .join(format!("{n}.json"));
    let kept = std::fs::metadata(&path).is_ok_and(|m| m.len() > 0);
    kept.then(|| read_json(&path))
}

#[test]
fn extractions_score_no_worse_than_the_baseline() {
    let ledgers: Vec<u32> = (1..=14)
        .filter(|n| {
            expected_path(*n).is_file()
                && common::data_dir()
                    .join("plan")
                    .join(format!("{n}.pdf"))
                    .is_file()
        })
        .collect();
    if ledgers.is_empty() && !common::corpus_available("scorecard", &[expected_path(1)]) {
        return;
    }
    let out = tempfile::tempdir().expect("temp dir");

    let mut documents: BTreeMap<String, BTreeMap<&'static str, f64>> = BTreeMap::new();
    for n in &ledgers {
        let expected = read_json(&expected_path(*n));
        let dossier = run_dossier(*n, out.path());
        let model = expected
            .get("model")
            .filter(|m| !m.is_null())
            .and_then(|_| run_model(*n));
        let building = expected
            .get("building")
            .filter(|b| !b.is_null())
            .and_then(|_| recorded_building(*n));
        documents.insert(
            n.to_string(),
            score_document(&expected, &dossier, model.as_ref(), building.as_ref()),
        );
    }

    let mut kinds: BTreeMap<&'static str, f64> = BTreeMap::new();
    for kind in KINDS {
        let scores: Vec<f64> = documents
            .values()
            .filter_map(|d| d.get(kind).copied())
            .collect();
        if let Some(m) = mean(&scores) {
            kinds.insert(kind, (m * 1e4).round() / 1e4);
        }
    }

    let baseline_file = baseline_path();
    let baseline = if baseline_file.is_file() {
        read_json(&baseline_file)
    } else {
        json!({ "kinds": {}, "documents": {} })
    };

    // The table: one row per kind, the baseline beside the new score.
    eprintln!("\nextraction scorecard ({} documents)", documents.len());
    eprintln!("{:<15} {:>9} {:>9}", "kind", "baseline", "now");
    let mut regressions = Vec::new();
    let mut improvements = Vec::new();
    for (kind, score) in &kinds {
        let old = baseline["kinds"][kind].as_f64();
        let mark = match old {
            Some(o) if *score < o - TOLERANCE => "  REGRESSION",
            Some(o) if *score > o + TOLERANCE => "  improved",
            Some(_) => "",
            None => "  new",
        };
        eprintln!(
            "{:<15} {:>9} {:>9.4}{mark}",
            kind,
            old.map(|o| format!("{o:.4}")).unwrap_or_else(|| "-".into()),
            score
        );
    }
    for (document, scores) in &documents {
        for (kind, score) in scores {
            let Some(old) = baseline["documents"][document][kind].as_f64() else {
                continue;
            };
            if *score < old - TOLERANCE {
                regressions.push(format!(
                    "document {document} {kind}: {old:.4} -> {score:.4}"
                ));
            } else if *score > old + TOLERANCE {
                improvements.push(format!(
                    "document {document} {kind}: {old:.4} -> {score:.4}"
                ));
            }
        }
    }
    for line in &improvements {
        eprintln!("improved   {line}");
    }
    for line in &regressions {
        eprintln!("REGRESSION {line}");
    }

    let scorecard = json!({ "kinds": kinds, "documents": documents });
    let latest = common::workspace_root()
        .join("target")
        .join("scorecard-latest.json");
    std::fs::write(
        &latest,
        serde_json::to_string_pretty(&scorecard).expect("serialisable"),
    )
    .expect("write latest scorecard");
    eprintln!("written {}", latest.display());

    if std::env::var_os("PDF_TOOLKIT_SCORECARD_UPDATE").is_some() {
        std::fs::write(
            &baseline_file,
            serde_json::to_string_pretty(&scorecard).expect("serialisable") + "\n",
        )
        .expect("write baseline");
        eprintln!("baseline updated: {}", baseline_file.display());
        return;
    }
    assert!(
        regressions.is_empty(),
        "extraction regressions against {} — if intended, re-run with PDF_TOOLKIT_SCORECARD_UPDATE=1:\n{}",
        baseline_file.display(),
        regressions.join("\n")
    );
}
