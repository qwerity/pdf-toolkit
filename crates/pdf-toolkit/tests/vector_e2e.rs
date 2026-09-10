//! End-to-end tests for `vector`, against the real dossier corpus.
//!
//! These run the binary over `test_assets/plan/` — Armenian building-permit
//! dossiers — because the thing being tested is not that the code runs but that
//! it gets the right answer on real CAD exports. A synthetic fixture cannot
//! exercise a Gauss-Krüger datum shift or a coordinate table that a CAD
//! exporter turned into glyph outlines.
//!
//! **The dossiers are private data.** They are not committed, published or
//! fetched, and `test_assets/` is gitignored in full. Everything here skips
//! itself when they are absent — see [`corpus_available`].
//!
//! The centrepiece is [`known_placements_reproduce_to_the_millimetre`], which
//! pins every page the corpus can place and the residual it places to. A
//! residual is the one number that says a placement is *correct* rather than
//! merely produced: the transform is fitted to the table's own coordinates, so
//! a misread digit moves the residual even when the fit "succeeds".

mod cli;
mod common;

use std::collections::HashMap;
use std::path::PathBuf;

use cli::{as_str, run, run_argv};
use serde_json::Value;

/// A dossier from the corpus.
fn sample(name: &str) -> PathBuf {
    corpus_dir().join(name)
}

fn corpus_dir() -> PathBuf {
    common::workspace_root().join("test_assets").join("plan")
}

/// Whether the corpus is present, and what to do when it is not.
///
/// The dossiers are real building-permit submissions — private data. They are
/// never committed, published or fetched; they exist only on machines that
/// already hold them, and `test_assets/` is gitignored in full.
///
/// So a clone without them still gets a passing `cargo test`, with a loud line
/// naming what was skipped. On a machine that *does* hold the corpus, a skip
/// would silently drop the only tests that check the georeferencing is right,
/// so `PDF_TOOLKIT_REQUIRE_CORPUS=1` turns the skip into a failure there.
///
/// Returns false when the caller should return without asserting.
#[must_use]
fn corpus_available(test: &str) -> bool {
    if sample("3.pdf").is_file() {
        return true;
    }
    assert!(
        std::env::var_os("PDF_TOOLKIT_REQUIRE_CORPUS").is_none(),
        "{test}: PDF_TOOLKIT_REQUIRE_CORPUS is set but the dossier corpus is \
         missing from {}",
        corpus_dir().display()
    );
    eprintln!(
        "SKIP {test}: dossier corpus not present in {}. \
         It is private data and is not distributed; copy the dossiers there to \
         enable these tests.",
        corpus_dir().display()
    );
    false
}

fn vector(args: &[&str]) -> Value {
    let mut argv = vec!["vector".to_string()];
    argv.extend(args.iter().map(|a| (*a).to_string()));
    run_argv(&argv).expect_success().json()
}

/// A whole dossier, run once per test binary and shared.
///
/// Whole-document rather than per-page, and that is not only for speed: a page
/// can be placed *because of its siblings*. A fit good to centimetres fixes the
/// document's coordinate system, and the pages that failed are then retried
/// against it with a looser tolerance. `10.pdf` p12 places only this way — ask
/// for that page alone and it correctly reports no consistent fit.
fn document(name: &str) -> Value {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<HashMap<String, Value>>> =
        std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()));

    if let Some(hit) = cache.lock().expect("cache").get(name) {
        return hit.clone();
    }
    let value = vector(&[as_str(&sample(name))]);
    cache
        .lock()
        .expect("cache")
        .insert(name.to_string(), value.clone());
    value
}

/// One page of one document, as produced by a whole-document run.
fn page(name: &str, number: u32) -> Value {
    document(name)["pages"]
        .as_array()
        .expect("pages array")
        .iter()
        .find(|p| p["page_number"] == number)
        .unwrap_or_else(|| panic!("{name} has no page {number}"))
        .clone()
}

fn f64_at(value: &Value, key: &str) -> f64 {
    value[key]
        .as_f64()
        .unwrap_or_else(|| panic!("`{key}` missing or not a number in {value}"))
}

// ---------------------------------------------------------------------------
// the corpus result, which is what says the geometry is right
// ---------------------------------------------------------------------------

/// Every page the corpus places from its own printed table, and to what.
///
/// `(document, page, control points, residual in metres, fitted scale)`.
/// Residuals are compared with a tolerance far tighter than the difference
/// between a correct fit and a wrong one, so this catches a regression in the
/// projection, the table reader, or the RANSAC consensus without being brittle
/// about the last digit.
const PLACEMENTS: &[(&str, u32, u64, f64, f64)] = &[
    ("2.pdf", 11, 10, 0.0041, 500.0),
    ("3.pdf", 9, 5, 0.0030, 500.0),
    ("3.pdf", 18, 5, 0.0082, 499.0),
    ("4.pdf", 3, 9, 0.0031, 500.0),
    ("5.pdf", 9, 5, 0.0041, 714.0),
    ("9.pdf", 6, 7, 0.0046, 500.0),
    ("10.pdf", 4, 6, 0.0038, 500.0),
    // Placed only because a sibling page fitted to centimetres and fixed the
    // document's CRS, which licenses a looser tolerance here. Without that
    // second pass this page does not place at all.
    ("10.pdf", 12, 4, 0.3675, 645.0),
    ("13.pdf", 4, 8, 0.0031, 500.0),
];

/// Every dossier in the corpus, in numeric order.
///
/// Read from the directory rather than listed, so adding a dossier does not
/// mean editing a list here. What a new dossier *must* still do is not silently
/// change the ground truth: if it places a page, `the_corpus_places_exactly_the_
/// recorded_pages` fails until that placement is written into [`PLACEMENTS`].
fn corpus_documents() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(corpus_dir())
        .expect("corpus directory")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".pdf"))
        .collect();
    names.sort_by_key(|name| {
        name.trim_end_matches(".pdf")
            .parse::<u32>()
            .unwrap_or(u32::MAX)
    });
    names
}

#[test]
fn known_placements_reproduce_to_the_millimetre() {
    if !corpus_available("known_placements_reproduce_to_the_millimetre") {
        return;
    }

    for &(document, number, points, rmse, scale) in PLACEMENTS {
        let page = page(document, number);
        let geo = &page["georeference"];

        assert_eq!(
            geo["placed"], true,
            "{document} p{number} must place; got {geo}"
        );
        assert_eq!(geo["source"], "text_layer", "{document} p{number}");
        assert_eq!(
            geo["control_points_used"], points,
            "{document} p{number} control points"
        );

        let actual = f64_at(geo, "rmse_m");
        assert!(
            (actual - rmse).abs() < 0.001,
            "{document} p{number} residual drifted: expected {rmse} m, got {actual} m"
        );

        let fitted = f64_at(geo, "fitted_scale_denominator");
        assert!(
            (fitted - scale).abs() <= 1.0,
            "{document} p{number} fitted scale drifted: expected 1:{scale}, got 1:{fitted}"
        );
    }
}

/// The fitted scale is recovered from the drawing, independently of the
/// coordinates the transform was fitted to — so its agreement with the round
/// number a draughtsman would have plotted at is a second, independent check.
#[test]
fn fitted_scales_land_on_plausible_drawing_scales() {
    if !corpus_available("fitted_scales_land_on_plausible_drawing_scales") {
        return;
    }

    for &(document, number, _, _, _) in PLACEMENTS {
        let geo = page(document, number)["georeference"].clone();
        let fitted = f64_at(&geo, "fitted_scale_denominator");
        assert!(
            (20.0..=25_000.0).contains(&fitted),
            "{document} p{number}: 1:{fitted} is not a drawing scale"
        );
    }
}

#[test]
fn placed_pages_land_in_yerevan() {
    if !corpus_available("placed_pages_land_in_yerevan") {
        return;
    }

    // The corpus is in Yerevan. A datum shift applied wrongly
    // moves a result by tens of metres to hundreds of kilometres, and this is
    // the check that notices the large end of that.
    for &(document, number, ..) in PLACEMENTS {
        let geo = page(document, number)["georeference"].clone();
        let center = geo["center"].as_array().expect("center");
        let (lon, lat) = (
            center[0].as_f64().expect("lon"),
            center[1].as_f64().expect("lat"),
        );
        assert!(
            (44.0..45.0).contains(&lon) && (40.0..40.5).contains(&lat),
            "{document} p{number} placed at {lon},{lat}, outside Yerevan"
        );
    }
}

// ---------------------------------------------------------------------------
// the shape of the result
// ---------------------------------------------------------------------------

#[test]
fn a_placed_page_carries_its_geojson_inline_in_wgs84() {
    if !corpus_available("a_placed_page_carries_its_geojson_inline_in_wgs84") {
        return;
    }

    let page = page("3.pdf", 9);

    assert_eq!(page["crs"], "EPSG:4326");
    let collection = &page["geojson"];
    assert_eq!(collection["type"], "FeatureCollection");

    let features = collection["features"].as_array().expect("features");
    assert!(!features.is_empty(), "a survey sheet must export geometry");
    assert_eq!(
        features.len() as u64,
        page["summary"]["features"].as_u64().expect("summary count"),
        "the summary must count what the collection actually holds"
    );

    // Every coordinate is degrees, not the projected easting/northing the fit
    // was made in — a confusion that produces a plausible-looking file placed
    // thousands of kilometres away.
    let pairs = coordinate_pairs(&features[0]["geometry"]["coordinates"]);
    assert!(!pairs.is_empty(), "a feature with no coordinates");
    for (lon, lat) in pairs {
        assert!(
            (44.0..45.0).contains(&lon) && (40.0..40.5).contains(&lat),
            "coordinates are not WGS 84 degrees: {lon},{lat}"
        );
    }
}

#[test]
fn every_page_reports_a_classification_beside_its_geometry() {
    if !corpus_available("every_page_reports_a_classification_beside_its_geometry") {
        return;
    }

    let classification = page("3.pdf", 9)["classification"].clone();
    let classification = &classification;

    assert_eq!(classification["class"], "topographic_survey");
    assert_eq!(classification["confidence"], "certain");
    assert_eq!(classification["is_drawing"], true);
    assert_eq!(classification["is_mappable"], true);
    assert!(
        !classification["reason"]
            .as_str()
            .expect("reason")
            .is_empty(),
        "a verdict must say why"
    );

    // The measurements the verdict was made from, so a caller that disagrees
    // can see what it was based on.
    let features = &classification["features"];
    assert!(features["path_count"].as_u64().expect("path_count") > 100);
    assert_eq!(features["has_control_table"], true);
    assert!(features["control_rows"].as_u64().expect("control_rows") >= 4);
}

#[test]
fn a_document_reports_its_own_page_count_and_source() {
    if !corpus_available("a_document_reports_its_own_page_count_and_source") {
        return;
    }

    let input = sample("3.pdf");
    let out = document("3.pdf");

    assert_eq!(out["document"], "3");
    assert_eq!(out["source"], as_str(&input));
    assert_eq!(
        out["page_count"], 34,
        "page_count is the document's, not the selection's"
    );
}

#[test]
fn a_page_selection_is_honoured() {
    if !corpus_available("a_page_selection_is_honoured") {
        return;
    }

    let out = vector(&[as_str(&sample("3.pdf")), "--pages", "9,18"]);
    let pages = out["pages"].as_array().expect("pages");

    assert_eq!(pages.len(), 2);
    assert_eq!(pages[0]["page_number"], 9);
    assert_eq!(pages[1]["page_number"], 18);
}

// ---------------------------------------------------------------------------
// pages that cannot be placed
// ---------------------------------------------------------------------------

/// A page with no coordinate table still exports its geometry, in a frame that
/// says so. The shapes are right; only the position on Earth is missing.
#[test]
fn an_unplaceable_page_still_exports_geometry_and_says_why() {
    if !corpus_available("an_unplaceable_page_still_exports_geometry_and_says_why") {
        return;
    }

    let page = page("7.pdf", 10);
    let geo = &page["georeference"];

    assert_eq!(geo["placed"], false);
    assert_eq!(geo["failure"]["reason"], "no_control_table");
    assert!(
        geo.get("rmse_m").is_none() && geo.get("center").is_none(),
        "an unplaced page must not report a residual or a position"
    );

    assert_eq!(
        page["crs"], "local_metres",
        "the sheet prints a scale, so its own frame is metric"
    );
    assert!(
        page["summary"]["features"].as_u64().expect("features") > 0,
        "the drawing is still there even though it is not placed"
    );
}

#[test]
fn a_page_whose_scale_is_unknown_falls_back_to_page_units() {
    if !corpus_available("a_page_whose_scale_is_unknown_falls_back_to_page_units") {
        return;
    }

    let page = page("7.pdf", 1);

    assert_eq!(page["georeference"]["placed"], false);
    assert_eq!(page["crs"], "page_units");
}

// ---------------------------------------------------------------------------
// placing a drawing from supplied references
// ---------------------------------------------------------------------------

/// Two references against the local frame of `7.pdf` p10, whose extent is
/// roughly -566..552 by -400..400 metres. They are not survey truth — they are
/// a known transform — so what is asserted is that the tool applies them
/// exactly, not that the result is a real parcel.
const REF_A: &str = "-565.87,-399.92,44.5100000,40.1500000";
const REF_B: &str = "552.41,399.92,44.5231000,40.5620000";

#[test]
fn supplied_references_place_a_drawing_that_has_no_table() {
    if !corpus_available("supplied_references_place_a_drawing_that_has_no_table") {
        return;
    }

    let input = sample("7.pdf");
    let out = vector(&[
        as_str(&input),
        "--pages",
        "10",
        "--georeference",
        REF_A,
        "--georeference",
        REF_B,
    ]);
    let page = &out["pages"][0];
    let geo = &page["georeference"];

    assert_eq!(geo["placed"], true);
    assert_eq!(geo["source"], "references");
    assert_eq!(geo["control_points_used"], 2);
    assert_eq!(page["crs"], "EPSG:4326");

    // Two references determine the similarity exactly, so it must fit them.
    assert!(
        f64_at(geo, "rmse_m") < 1e-6,
        "two references define the transform; residual must vanish, got {}",
        geo["rmse_m"]
    );
    assert!(
        geo["rotation_degrees"].is_number(),
        "a placement from references recovers a rotation"
    );

    // The geometry moved with it, rather than the report alone changing.
    let features = page["geojson"]["features"].as_array().expect("features");
    assert!(!features.is_empty());
    let coordinates: Vec<(f64, f64)> = features
        .iter()
        .flat_map(|f| coordinate_pairs(&f["geometry"]["coordinates"]))
        .collect();
    assert!(!coordinates.is_empty(), "no coordinate pairs to check");
    for (lon, lat) in coordinates {
        assert!(
            (44.2..44.8).contains(&lon) && (40.1..40.6).contains(&lat),
            "placed geometry escaped the references' neighbourhood: {lon},{lat}"
        );
    }
}

/// Every `[x, y]` in a GeoJSON coordinate value, at whatever nesting depth the
/// geometry type puts them.
fn coordinate_pairs(value: &Value) -> Vec<(f64, f64)> {
    let Some(items) = value.as_array() else {
        return Vec::new();
    };
    // A position is two-or-more numbers; anything else is a level of nesting.
    if let (Some(x), Some(y)) = (
        items.first().and_then(Value::as_f64),
        items.get(1).and_then(Value::as_f64),
    ) {
        return vec![(x, y)];
    }
    items.iter().flat_map(coordinate_pairs).collect()
}

/// A page that can place itself is not overridden by supplied references: the
/// sheet's own table is checkable against the drawing, and a supplied pair is
/// not.
#[test]
fn a_sheets_own_table_wins_over_supplied_references() {
    if !corpus_available("a_sheets_own_table_wins_over_supplied_references") {
        return;
    }

    let out = vector(&[
        as_str(&sample("3.pdf")),
        "--pages",
        "9",
        "--georeference",
        REF_A,
        "--georeference",
        REF_B,
    ]);
    let geo = &out["pages"][0]["georeference"];

    assert_eq!(geo["source"], "text_layer");
    assert!((f64_at(geo, "rmse_m") - 0.0030).abs() < 0.001);
}

/// One reference fixes position but leaves rotation and scale free, so a
/// placement built from it would be confidently wrong.
#[test]
fn a_single_reference_is_refused() {
    if !corpus_available("a_single_reference_is_refused") {
        return;
    }

    let input = sample("7.pdf");
    let run = run(&[
        "vector",
        as_str(&input),
        "--pages",
        "10",
        "--georeference",
        REF_A,
    ])
    .expect_failure();

    assert!(
        run.stderr.contains("at least two"),
        "the refusal must say what is missing, got {:?}",
        run.stderr
    );
    assert!(run.stdout.trim().is_empty(), "a refusal prints no JSON");
}

#[test]
fn a_malformed_reference_is_refused() {
    if !corpus_available("a_malformed_reference_is_refused") {
        return;
    }

    let input = sample("7.pdf");
    for bad in [
        "1,2,3",          // three values
        "1,2,3,4,5",      // five
        "a,b,c,d",        // not numbers
        "0,0,400.0,40.0", // longitude off the world
        "0,0,44.5,120.0", // latitude off the world
    ] {
        let run = run(&["vector", as_str(&input), "--georeference", bad]).expect_failure();
        assert!(
            run.stdout.trim().is_empty(),
            "`{bad}` must not produce a document"
        );
    }
}

// ---------------------------------------------------------------------------
// files on disk
// ---------------------------------------------------------------------------

#[test]
fn output_writes_the_same_geojson_it_reported() {
    if !corpus_available("output_writes_the_same_geojson_it_reported") {
        return;
    }

    let dir = common::workspace_root()
        .join("target")
        .join("tmp")
        .join("vector_e2e");
    let _ = std::fs::remove_dir_all(&dir);

    let out = vector(&[
        as_str(&sample("3.pdf")),
        "--pages",
        "9",
        "--output",
        as_str(&dir),
    ]);

    let written = dir.join("3_p09.geojson");
    assert!(written.is_file(), "expected {}", written.display());

    let from_disk: Value =
        serde_json::from_str(&std::fs::read_to_string(&written).expect("read geojson"))
            .expect("written file must be JSON");
    assert_eq!(
        from_disk, out["pages"][0]["geojson"],
        "the file and the stream must not be able to disagree"
    );
}

// ---------------------------------------------------------------------------
// the corpus as a whole
// ---------------------------------------------------------------------------

/// Runs every dossier and checks the invariants that must hold for every page,
/// whatever it contains.
#[test]
fn every_page_of_every_dossier_reports_a_usable_result() {
    if !corpus_available("every_page_of_every_dossier_reports_a_usable_result") {
        return;
    }

    let documents = corpus_documents();
    assert!(
        documents.len() >= 10,
        "the corpus looks truncated: {} dossier(s) in {}",
        documents.len(),
        corpus_dir().display()
    );
    let mut total_pages = 0usize;

    for name in &documents {
        let out = document(name);
        let pages = out["pages"].as_array().expect("pages");
        assert!(!pages.is_empty(), "{name} produced no pages");

        for page in pages {
            total_pages += 1;

            // Every page says what it is, what frame its coordinates are in,
            // and either where it is or why it is not anywhere.
            assert!(page["classification"]["class"].is_string(), "{name}");
            assert_eq!(page["geojson"]["type"], "FeatureCollection", "{name}");
            let crs = page["crs"].as_str().expect("crs");
            assert!(
                matches!(crs, "EPSG:4326" | "local_metres" | "page_units"),
                "{name} p{}: unexpected crs {crs}",
                page["page_number"]
            );

            let geo = &page["georeference"];
            if geo["placed"] == Value::Bool(true) {
                assert_eq!(crs, "EPSG:4326", "{name}: a placed page must be in degrees");
                assert!(
                    geo["rmse_m"].is_number(),
                    "{name}: placed without a residual"
                );
            } else {
                assert!(
                    geo["failure"]["reason"].is_string(),
                    "{name}: unplaced without a reason"
                );
                assert_ne!(crs, "EPSG:4326", "{name}: unplaced but claiming degrees");
            }
        }
    }

    assert!(
        total_pages > 250,
        "the corpus looks truncated: {total_pages} pages across {} dossiers",
        documents.len()
    );
}

/// The corpus places exactly the pages [`PLACEMENTS`] records — no more, no
/// fewer.
///
/// The "no more" half is the one that matters. A page that starts placing
/// because a threshold moved is a change in what this tool claims about the
/// world, and it should be looked at and written down rather than absorbed
/// silently. Adding a dossier that places a page fails here on purpose.
#[test]
fn the_corpus_places_exactly_the_recorded_pages() {
    if !corpus_available("the_corpus_places_exactly_the_recorded_pages") {
        return;
    }

    let mut actual: Vec<(String, u64)> = Vec::new();
    for name in corpus_documents() {
        for page in document(&name)["pages"].as_array().expect("pages") {
            if page["georeference"]["placed"] == Value::Bool(true) {
                actual.push((
                    name.clone(),
                    page["page_number"].as_u64().expect("page number"),
                ));
            }
        }
    }
    actual.sort();

    let mut expected: Vec<(String, u64)> = PLACEMENTS
        .iter()
        .map(|(document, page, ..)| ((*document).to_string(), u64::from(*page)))
        .collect();
    expected.sort();

    let unexpected: Vec<_> = actual.iter().filter(|p| !expected.contains(p)).collect();
    let missing: Vec<_> = expected.iter().filter(|p| !actual.contains(p)).collect();

    assert!(
        unexpected.is_empty() && missing.is_empty(),
        "the corpus no longer places what PLACEMENTS records.\n  \
         newly placed (add to PLACEMENTS with its residual): {unexpected:?}\n  \
         no longer placed (a regression): {missing:?}"
    );
}
