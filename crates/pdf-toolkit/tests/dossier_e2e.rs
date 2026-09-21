//! End-to-end tests for `dossier`.
//!
//! The centrepiece runs against the real corpus in `data/plan/` with the
//! reviewed vision-pass answers in `data/annotations/answers/`, and requires
//! the output to reproduce `data/annotations/<n>.json` **exactly**. Those
//! files are the reviewed ground truth: every label, entity, group, edge and
//! byte of routing in them was checked by hand, so equality is the strongest
//! statement this command can make about itself. The deterministic-only test
//! pins what must hold *before* any vision pass: placements, decoded QR
//! payloads, template-parsed entities, mis-encoding flags.
//!
//! **The corpus is personal data**, committed as the test data set of a
//! private repository; everything here skips itself when it is absent (a
//! partial checkout), and `PDF_TOOLKIT_REQUIRE_CORPUS=1` turns the skip into
//! a failure — the same contract as `vector_e2e`.

mod cli;
mod common;

use std::path::PathBuf;

use cli::{argv, as_args, run};
use serde_json::Value;

fn corpus_pdf(n: usize) -> PathBuf {
    common::data_dir().join("plan").join(format!("{n}.pdf"))
}

fn annotation(n: usize) -> PathBuf {
    common::data_dir()
        .join("annotations")
        .join(format!("{n}.json"))
}

fn answers(n: usize) -> PathBuf {
    common::data_dir()
        .join("annotations")
        .join("answers")
        .join(format!("{n}.json"))
}

/// See `common::corpus_available`; these tests need the reviewed annotations
/// and answers beside the corpus PDFs.
#[must_use]
fn corpus_available(test: &str) -> bool {
    common::corpus_available(test, &[corpus_pdf(3), annotation(3), answers(3)])
}

/// Expected values that are themselves excerpts of the corpus — a real QR
/// payload, certificate number, parcel code, person — live beside the corpus
/// in `data/annotations/expected.json`, never in code.
fn expected(section: &str) -> Value {
    let path = common::data_dir().join("annotations").join("expected.json");
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} unreadable: {e}", path.display()));
    serde_json::from_str::<Value>(&raw).expect("expected.json parses")[section].clone()
}

fn page(report: &Value, number: u64) -> &Value {
    report["pages"]
        .as_array()
        .expect("pages array")
        .iter()
        .find(|p| p["page_number"] == number)
        .unwrap_or_else(|| panic!("page {number} missing"))
}

/// Every dossier, merged with its reviewed answers, must reproduce the
/// blessed annotation byte-for-byte (as parsed JSON). GeoJSON is written to a
/// temp dir so the `cad.geojson` relative paths are exercised too.
///
/// This is deliberately one test over all fourteen dossiers: the annotations
/// were reviewed as a set, and a regression in any of them is the same
/// failure — the command no longer produces what was reviewed.
#[test]
fn merged_dossier_reproduces_the_reviewed_annotations() {
    if !corpus_available("merged_dossier_reproduces_the_reviewed_annotations") {
        return;
    }
    let out = tempfile::tempdir().expect("temp dir");

    for n in 1..=14 {
        let (pdf, annotation, answers) = (corpus_pdf(n), annotation(n), answers(n));
        assert!(
            pdf.is_file() && annotation.is_file() && answers.is_file(),
            "dossier {n}: corpus file, annotation or answers missing"
        );

        // The binary runs with the workspace root as its working directory,
        // and `source` echoes the path as typed — the blessed annotations
        // were generated with this relative form.
        let argv = argv::dossier_merged(
            &format!("data/plan/{n}.pdf"),
            answers.to_str().expect("utf-8 path"),
            out.path().to_str().expect("utf-8 path"),
        );
        let produced = run(&as_args(&argv)).expect_success().json();

        let mut produced = produced;
        let mut expected: Value =
            serde_json::from_str(&std::fs::read_to_string(&annotation).expect("read annotation"))
                .expect("annotation parses");

        let own_stamp = format!("pdf-toolkit {} dossier", env!("CARGO_PKG_VERSION"));
        assert!(
            produced["generated_by"]
                .as_str()
                .is_some_and(|s| s.starts_with(&own_stamp)),
            "dossier {n}: generated_by must name the running version ({own_stamp})"
        );
        unversion_generated_by(&mut produced);
        unversion_generated_by(&mut expected);

        assert_eq!(
            produced, expected,
            "dossier {n}: output diverged from the reviewed annotation; if the change is \
             intended, regenerate data/annotations/{n}.json and re-review the diff"
        );
    }
}

/// Drops the version from `generated_by` (`pdf-toolkit <version> dossier: …`).
///
/// The stamp in a reviewed annotation records the release it was reviewed
/// against; a version bump changes no reading, so it must not invalidate the
/// review. The running version is asserted separately on the produced side.
fn unversion_generated_by(report: &mut Value) {
    let Some(stamp) = report["generated_by"].as_str() else {
        return;
    };
    let unversioned = match stamp
        .strip_prefix("pdf-toolkit ")
        .and_then(|rest| rest.split_once(' '))
    {
        Some((_version, rest)) => format!("pdf-toolkit {rest}"),
        None => stamp.to_owned(),
    };
    report["generated_by"] = Value::String(unversioned);
}

/// What the deterministic pass alone must get right, pinned on dossier 2 —
/// the one that exercises every mechanism at once: a placed cadastre extract,
/// a decoded QR, CID-garbled CAD sheets, certificate template pages and a
/// subject-parcel match.
#[test]
fn deterministic_dossier_flags_what_it_cannot_read() {
    if !corpus_available("deterministic_dossier_flags_what_it_cannot_read") {
        return;
    }
    let argv = argv::dossier("data/plan/2.pdf");
    let report = run(&as_args(&argv)).expect_success().json();

    // The placement is the vector pipeline's, verbatim.
    let extract = page(&report, 11);
    assert_eq!(extract["georeference"]["placed"], Value::Bool(true));
    assert_eq!(extract["georeference"]["crs"], "EPSG:28408");

    // The e-cadastre reference's QR decodes without any vision pass.
    let want = expected("dossier_2");
    let reference = page(&report, 15);
    assert_eq!(reference["qr_codes"][0]["data"], want["qr_p15"]);
    assert!(
        reference["labels"]
            .as_array()
            .expect("labels")
            .iter()
            .any(|l| l == "qr"),
        "a decoded code must label the page"
    );

    // The certificate template: both pages grouped, chained, owner extracted.
    let group = report["groups"]
        .as_array()
        .expect("groups")
        .iter()
        .find(|g| {
            g["id"].as_str()
                == Some(&format!(
                    "certificate:{}",
                    want["certificate"].as_str().unwrap()
                ))
        })
        .expect("certificate group");
    assert_eq!(group["pages"], serde_json::json!([5, 6]));
    // The certificate names the parcel the (unread, scanned) application
    // cannot name here, so without a form match the kind stays conservative.
    assert!(group["kind"] == "ownership" || group["kind"] == "administrative");
    assert_eq!(page(&report, 5)["related_pages"][0]["page"], 6);
    let entities: Vec<&str> = report["entities"]
        .as_array()
        .expect("entities")
        .iter()
        .filter_map(|e| e["id"].as_str())
        .collect();
    for (kind, key) in [
        ("certificate_number", "certificate"),
        ("cadastral_code", "cadastral_code"),
        ("person", "person"),
    ] {
        let id = format!("{kind}:{}", want[key].as_str().unwrap());
        assert!(entities.contains(&id.as_str()), "{id} missing");
    }

    // The application form's request slot is read off the embedded text:
    // this dossier asks to rebuild a house and put up a fence.
    let request = &report["request"];
    assert_eq!(request["action"], "reconstruction");
    assert_eq!(request["use"], "residential");
    assert_eq!(request["page"], request["use_page"]);
    assert!(
        request["works"]
            .as_array()
            .expect("works")
            .iter()
            .any(|w| w == "fence"),
        "the form names a fence beside the house"
    );
    assert!(
        !request["address"]
            .as_str()
            .expect("address")
            .contains("01-011"),
        "the cadastral code is an entity, not part of the address"
    );

    // Page 25 is a site plan titled in Armenian capitals. The garble test is
    // a LATIN one — it weighs lowercase and Latin vowels, and a page in
    // Armenian, Greek or Cyrillic capitals scores zero on both — so this page
    // used to be called noise and dropped. It reads.
    let armenian = page(&report, 25);
    assert!(
        armenian["extraction"]["text"]
            .as_str()
            .is_some_and(|t| t.contains("ԳԼԽԱՎՈՐ ՀԱՏԱԿԱԳԻԾ")),
        "the sheet names itself and the text layer says so"
    );
    assert_eq!(armenian["extraction"]["encoding_issue"], Value::Null);

    // A genuinely broken CID layer is still called what it is. Dossier 7's
    // page 11 decodes to `ɬʏʙʃ Թɼʗʀ` — letters from no alphabet the sheet is
    // written in, which is a different thing from an alphabet this test
    // cannot score.
    let broken = run(&as_args(&argv::dossier("data/plan/7.pdf")))
        .expect_success()
        .json();
    assert_eq!(
        page(&broken, 11)["extraction"]["encoding_issue"],
        Value::Bool(true)
    );

    // Scans carry their vision-pass work order.
    let scan = page(&report, 4);
    assert_eq!(scan["extraction"]["route"], "vlm");
    assert_eq!(
        scan["extraction"]["vlm"]["reasons"][0],
        "scanned_or_unreadable_text"
    );
}

/// The CI-runnable shape check: a synthetic fixture exercises the command
/// end to end without the corpus.
#[test]
fn dossier_reports_the_contract_shape_on_a_plain_document() {
    let pdfium = common::pdfium();
    let dir = tempfile::tempdir().expect("temp dir");
    let pdf = dir.path().join("plain.pdf");
    common::write_pdf(
        &pdfium,
        &pdf,
        &[
            vec![common::Text::plain(
                50.0,
                700.0,
                12.0,
                "A short letter about nothing in particular, set in plain type.",
            )],
            vec![common::Text::plain(
                50.0,
                700.0,
                12.0,
                "It continues on a second page, still saying very little.",
            )],
        ],
    );
    drop(pdfium);

    let argv = argv::dossier(pdf.to_str().expect("utf-8 path"));
    let report = run(&as_args(&argv)).expect_success().json();

    assert_eq!(report["schema_version"], "0.4.0");
    assert_eq!(report["page_count"], 2);
    assert_eq!(report["stats"]["embedded_text"], 2);
    for n in [1, 2] {
        let p = page(&report, n);
        assert_eq!(p["extraction"]["route"], "embedded_text");
        assert_eq!(p["extraction"]["engine"], "pdfium");
        assert_eq!(p["labels"][0], "text_document");
        assert_eq!(p["content_kind"], "digital");
    }
    assert!(report["graph"]["edges"].is_array());
    assert!(
        report.get("request").is_none(),
        "no application form, no request block — absence, not a default"
    );
}
