//! End-to-end tests for `model`, against the reference dossier of the
//! corpus: `data/plan/4.pdf`, the ArchiCAD sketch set the reconstruction
//! was developed on. Everything here skips when it is absent (see
//! `common::corpus_available`).
//!
//! The library's own golden test (`crates/pdf-model/tests/golden.rs`) pins
//! the geometry against the reference implementation's output; these pin
//! the **contract**: what the command prints, which files it writes, and
//! that the dossier places the model on the map from its own site plan.

mod cli;
mod common;

use std::path::PathBuf;

use cli::{as_str, run_argv};
use serde_json::Value;

fn sample() -> PathBuf {
    common::data_dir().join("plan").join("4.pdf")
}

#[must_use]
fn corpus_available(test: &str) -> bool {
    common::corpus_available(test, &[sample()])
}

fn model(args: &[&str]) -> Value {
    let mut argv = vec!["model".to_string(), as_str(&sample()).to_string()];
    argv.extend(args.iter().map(|a| (*a).to_string()));
    run_argv(&argv).expect_success().json()
}

fn report() -> Value {
    static CACHE: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| model(&[])).clone()
}

#[test]
fn the_parameters_are_detected_from_the_sheets_themselves() {
    if !corpus_available("the_parameters_are_detected_from_the_sheets_themselves") {
        return;
    }
    let report = report();
    let params = &report["params"];
    assert_eq!(params["source"], "detected");
    assert_eq!(params["scale_denominator"], 75.0);
    let floors = params["floors"].as_array().expect("floors");
    assert_eq!(floors.len(), 2, "two storeys");
    assert_eq!(
        (floors[0]["page"].as_u64(), floors[0]["level_m"].as_f64()),
        (Some(13), Some(0.0))
    );
    assert_eq!(
        (floors[1]["page"].as_u64(), floors[1]["level_m"].as_f64()),
        (Some(14), Some(3.6))
    );
    assert_eq!(floors[1]["top_m"], 7.2);
    assert_eq!(params["roof_page"], 15);
    assert_eq!(params["roof_level_m"], 7.2);
    assert_eq!(params["parapet_level_m"], 7.78);
    assert_eq!(params["slab_thickness_m"], 0.5);
    assert_eq!(params["plinth_m"], 0.35);
    assert_eq!(params["axis"]["column_label"], "1");
    assert_eq!(params["axis"]["row_label"], "Ա");
    let notes = report["params_notes"].as_array().expect("notes");
    assert!(
        notes
            .iter()
            .any(|n| n.as_str().unwrap().contains("scale 1:75"))
    );
}

#[test]
fn the_model_is_placed_from_the_site_plan_to_the_centimetre() {
    if !corpus_available("the_model_is_placed_from_the_site_plan_to_the_centimetre") {
        return;
    }
    let report = report();
    assert_eq!(report["placed"], true);
    assert_eq!(report["crs"], "EPSG:4326");
    let geo = &report["georeference"];
    assert_eq!(geo["source"], "sheet_outline");
    assert!(geo["residual_m"].as_f64().unwrap() < 0.02, "{geo}");
    assert!(geo["linework_residual_m"].as_f64().unwrap() < 0.1, "{geo}");
    let bounds = report["bounds"].as_array().expect("bounds");
    let west = bounds[0][0].as_f64().unwrap();
    let south = bounds[0][1].as_f64().unwrap();
    assert!(
        (44.4..44.6).contains(&west) && (40.0..40.3).contains(&south),
        "in Yerevan"
    );
    // Every feature is in degrees, with the extrusion properties a map needs.
    let features = report["geojson"]["features"].as_array().expect("features");
    assert!(features.len() > 50);
    let wall = features
        .iter()
        .find(|f| f["properties"]["role"] == "wall")
        .expect("a wall");
    assert_eq!(wall["properties"]["base"], 0.0);
    assert_eq!(wall["properties"]["height"], 3.6);
    assert!(
        wall["properties"]["color"]
            .as_str()
            .unwrap()
            .starts_with('#')
    );
    let first = &wall["geometry"]["coordinates"][0][0];
    assert!((44.4..44.6).contains(&first[0].as_f64().unwrap()));
    assert_eq!(report["geojson"]["metadata"]["crs"], "EPSG:4326");
}

#[test]
fn the_storeys_and_roof_are_summarised_and_carried_inline() {
    if !corpus_available("the_storeys_and_roof_are_summarised_and_carried_inline") {
        return;
    }
    let report = report();
    let floors = report["floors"].as_array().expect("floors");
    assert_eq!(floors[0]["rooms"], 14);
    assert_eq!(floors[0]["glazing"], 10);
    assert_eq!(floors[1]["rooms"], 20);
    assert_eq!(floors[1]["rails"], 6);
    assert!((floors[0]["slab_area_m2"].as_f64().unwrap() - 87.79).abs() < 0.05);
    assert!((report["roof"]["area_m2"].as_f64().unwrap() - 100.87).abs() < 0.05);
    // The two stone walls either side, cut at the envelope; the five 3 cm
    // strips of wall drawn past it are not piers.
    assert_eq!(report["stone_piers"], 2);
    let model = &report["model"];
    assert_eq!(model["units"], "m");
    assert_eq!(model["levels"]["parapet"], 7.78);
    assert_eq!(model["floors"].as_array().unwrap().len(), 2);
    assert!(
        model["floors"][0]["envelope"]["outer"]
            .as_array()
            .unwrap()
            .len()
            >= 4
    );
    // The parapet the roof plan draws, 37 cm in from the roof's curved edges —
    // not a ring along the edge, and so no warning about one.
    let roof = &model["roof"];
    let parapet = roof["parapet"].as_array().expect("parapet");
    assert_eq!(parapet.len(), 1, "one drawn ring");
    let least = |polygon: &Value, axis: usize| {
        polygon["outer"]
            .as_array()
            .expect("outer ring")
            .iter()
            .map(|p| p[axis].as_f64().expect("coordinate"))
            .fold(f64::INFINITY, f64::min)
    };
    for axis in [0, 1] {
        let inset = least(&parapet[0], axis) - least(&roof["outline"], axis);
        assert!(
            (0.3..0.45).contains(&inset),
            "axis {axis}: the parapet is {inset} m in from the edge"
        );
    }
    assert!(report["warnings"].as_array().is_none_or(|w| {
        w.iter()
            .all(|w| !w.as_str().unwrap_or("").contains("parapet"))
    }));
    let sheets = report["sheets"].as_array().expect("sheets");
    assert!(sheets.iter().all(|s| s["origin_source"] == "axis_bubbles"));
}

#[test]
fn output_writes_the_model_the_mesh_the_geojson_and_the_params() {
    if !corpus_available("output_writes_the_model_the_mesh_the_geojson_and_the_params") {
        return;
    }
    let dir = tempfile::tempdir().expect("temp dir");
    let report = model(&["-o", as_str(dir.path())]);
    for (key, name) in [
        ("model_file", "model/4.model.json"),
        ("glb_file", "model/4.glb"),
        ("geojson_file", "model/4.geojson"),
        ("params_file", "model/4.params.json"),
    ] {
        assert_eq!(report[key], name);
        let path = dir.path().join(name);
        assert!(path.exists(), "{name} written");
        if name.ends_with(".glb") {
            let bytes = std::fs::read(&path).unwrap();
            assert_eq!(&bytes[0..4], b"glTF");
        } else {
            let value: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap())
                .unwrap_or_else(|e| panic!("{name} is not JSON: {e}"));
            assert!(value.is_object());
        }
    }
    // The written parameters re-run the reconstruction unchanged.
    let params = dir.path().join("model/4.params.json");
    let again = model(&["--params", as_str(&params)]);
    assert_eq!(again["params"]["source"], "file");
    assert_eq!(again["floors"], report["floors"]);
    assert!(
        again.get("params_notes").is_none(),
        "a file needs no explaining"
    );
}

#[test]
fn references_place_the_model_where_the_caller_says() {
    if !corpus_available("references_place_the_model_where_the_caller_says") {
        return;
    }
    // Two points in the model's metres and where they are: the origin at
    // (44.5, 40.09), and ten metres east of it along the parallel.
    let report = model(&["-g", "0,0,44.5,40.09", "-g", "10,0,44.5001173,40.09"]);
    assert_eq!(report["placed"], true);
    assert_eq!(report["georeference"]["source"], "references");
    let bounds = report["bounds"].as_array().expect("bounds");
    assert!((bounds[0][0].as_f64().unwrap() - 44.5).abs() < 0.001);
}

#[test]
fn a_single_reference_is_refused() {
    if !corpus_available("a_single_reference_is_refused") {
        return;
    }
    let result = run_argv(&[
        "model".to_string(),
        as_str(&sample()).to_string(),
        "-g".to_string(),
        "0,0,44.5,40.09".to_string(),
    ]);
    let failed = result.expect_failure();
    assert!(
        failed.stderr.contains("--georeference"),
        "stderr names the flag: {}",
        failed.stderr
    );
}
