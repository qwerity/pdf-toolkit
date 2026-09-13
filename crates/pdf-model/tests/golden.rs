//! The port reproduces the reference implementation's output on the
//! reference document: `data/plan/4-sketch.pdf` (the 17-sheet sketch set)
//! against `data/annotations/model/4.json`, the Python pipeline's
//! `model.json`. Both are private corpus data and gitignored; the test
//! skips without them unless `PDF_TOOLKIT_REQUIRE_CORPUS` is set.

use std::path::{Path, PathBuf};

use geo::{Area, BooleanOps, BoundingRect};
use pdf_geometry::PdfDocument;
use pdf_model::geom;
use pdf_model::model::{Model, Polygon2D};
use pdf_model::params::{FloorParams, Params};

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn corpus() -> Option<(PathBuf, PathBuf)> {
    let pdf = workspace().join("data/plan/4-sketch.pdf");
    let golden = workspace().join("data/annotations/model/4.json");
    if pdf.exists() && golden.exists() {
        Some((pdf, golden))
    } else if std::env::var_os("PDF_TOOLKIT_REQUIRE_CORPUS").is_some() {
        panic!("corpus required but {pdf:?} / {golden:?} missing");
    } else {
        eprintln!("skipping: corpus absent");
        None
    }
}

fn sketch_params() -> Params {
    let mut params = Params::archicad_template();
    params.floors = vec![
        FloorParams {
            page: 7,
            name: "Ground floor ±0.00".into(),
            level_m: 0.0,
            top_m: 3.6,
        },
        FloorParams {
            page: 8,
            name: "First floor +3.60".into(),
            level_m: 3.6,
            top_m: 7.2,
        },
    ];
    params.roof_page = Some(9);
    params
}

fn area(p: &Polygon2D) -> f64 {
    p.area_m2()
}

/// Symmetric-difference area between a golden polygon and the best-matching
/// port polygon (after `shift`), relative to the golden area.
fn best_mismatch(golden: &Polygon2D, ours: &[Polygon2D], shift: (f64, f64)) -> f64 {
    let g = geom::from_polygon2d(golden);
    ours.iter()
        .map(|o| {
            let shifted = Polygon2D {
                outer: o
                    .outer
                    .iter()
                    .map(|p| [p[0] + shift.0, p[1] + shift.1])
                    .collect(),
                holes: o
                    .holes
                    .iter()
                    .map(|h| h.iter().map(|p| [p[0] + shift.0, p[1] + shift.1]).collect())
                    .collect(),
            };
            let s = geom::from_polygon2d(&shifted);
            g.xor(&s).unsigned_area() / g.unsigned_area().max(1e-9)
        })
        .fold(f64::INFINITY, f64::min)
}

fn bbox_min(p: &Polygon2D) -> (f64, f64) {
    let r = geom::from_polygon2d(p).bounding_rect().unwrap();
    (r.min().x, r.min().y)
}

#[test]
fn the_port_matches_the_reference_model() {
    let Some((pdf, golden_path)) = corpus() else {
        return;
    };
    let golden: Model =
        serde_json::from_str(&std::fs::read_to_string(golden_path).unwrap()).unwrap();
    let document = PdfDocument::open(&pdf).unwrap();
    let ours = pdf_model::reconstruct(&document, &sketch_params())
        .unwrap()
        .model;

    assert_eq!(ours.floors.len(), golden.floors.len());
    // The reference took its origin from text bounding boxes, the port from
    // the bubble circles; the frames differ by a constant shift, measured on
    // the ground-floor slab and applied to everything.
    let (gx, gy) = bbox_min(&golden.floors[0].slab);
    let (ox, oy) = bbox_min(&ours.floors[0].slab);
    let shift = (gx - ox, gy - oy);
    eprintln!("frame shift {shift:?} m");
    assert!(
        shift.0.abs() < 0.05 && shift.1.abs() < 0.05,
        "origins agree to 5 cm: {shift:?}"
    );

    let mut worst: f64 = 0.0;
    for (g, o) in golden.floors.iter().zip(&ours.floors) {
        eprintln!(
            "{}: walls {}/{} glazing {}/{} door gaps {}/{} rooms {}/{} rails {}/{}",
            g.name,
            g.walls.len(),
            o.walls.len(),
            g.glazing.len(),
            o.glazing.len(),
            g.door_gaps.len(),
            o.door_gaps.len(),
            g.rooms.len(),
            o.rooms.len(),
            g.rails.len(),
            o.rails.len()
        );
        assert_eq!(
            (g.z, g.top, g.slab_z, g.glazing_z),
            (o.z, o.top, o.slab_z, o.glazing_z)
        );
        assert_eq!(g.rooms.len(), o.rooms.len(), "{}: room count", g.name);
        let mut ga: Vec<f64> = g.rooms.iter().map(|r| r.area).collect();
        let mut oa: Vec<f64> = o.rooms.iter().map(|r| r.area).collect();
        ga.sort_by(f64::total_cmp);
        oa.sort_by(f64::total_cmp);
        for (a, b) in ga.iter().zip(&oa) {
            assert!((a - b).abs() <= 0.011, "{}: room area {a} vs {b}", g.name);
        }
        assert!(
            (area(&g.slab) - area(&o.slab)).abs() < 0.02,
            "{}: slab area",
            g.name
        );
        let slab_mismatch = best_mismatch(&g.slab, std::slice::from_ref(&o.slab), shift);
        worst = worst.max(slab_mismatch);
        for (label, gs, os) in [
            ("walls", &g.walls, &o.walls),
            ("glazing", &g.glazing, &o.glazing),
            ("door_gaps", &g.door_gaps, &o.door_gaps),
        ] {
            assert_eq!(gs.len(), os.len(), "{}: {label} count", g.name);
            let total_g: f64 = gs.iter().map(area).sum();
            let total_o: f64 = os.iter().map(area).sum();
            assert!(
                (total_g - total_o).abs() < 0.02,
                "{}: {label} area {total_g} vs {total_o}",
                g.name
            );
            for gp in gs {
                let m = best_mismatch(gp, os, shift);
                worst = worst.max(m);
                assert!(
                    m < 0.02,
                    "{}: a {label} polygon differs by {:.1}% of its area",
                    g.name,
                    m * 100.0
                );
            }
        }
        assert_eq!(g.rails.len(), o.rails.len(), "{}: rails", g.name);
        let rail_g: f64 = g
            .rails
            .iter()
            .map(|r| geom::length(&geom::ring_in(r)))
            .sum();
        let rail_o: f64 = o
            .rails
            .iter()
            .map(|r| geom::length(&geom::ring_in(r)))
            .sum();
        assert!(
            (rail_g - rail_o).abs() < 0.05,
            "{}: rail length {rail_g} vs {rail_o}",
            g.name
        );
    }
    let (gr, or) = (golden.roof.as_ref().unwrap(), ours.roof.as_ref().unwrap());
    eprintln!(
        "roof: outline {}/{} points, area {:.3}/{:.3}",
        gr.outline.outer.len(),
        or.outline.outer.len(),
        area(&gr.outline),
        area(&or.outline)
    );
    assert!(
        (area(&gr.outline) - area(&or.outline)).abs() < 0.05,
        "roof area"
    );
    let roof_mismatch = best_mismatch(&gr.outline, std::slice::from_ref(&or.outline), shift);
    worst = worst.max(roof_mismatch);
    assert!(
        roof_mismatch < 0.01,
        "roof outline differs by {:.2}%",
        roof_mismatch * 100.0
    );
    assert_eq!((gr.slab_z, gr.parapet_z), (or.slab_z, or.parapet_z));
    let parapet_g: f64 = gr.parapet.iter().map(area).sum();
    let parapet_o: f64 = or.parapet.iter().map(area).sum();
    assert!(
        (parapet_g - parapet_o).abs() < 0.05,
        "parapet area {parapet_g} vs {parapet_o}"
    );
    assert_eq!(golden.stone.len(), ours.stone.len(), "stone piers");
    for gp in &golden.stone {
        let m = best_mismatch(gp, &ours.stone, shift);
        worst = worst.max(m);
        assert!(m < 0.02, "a stone pier differs by {:.1}%", m * 100.0);
    }
    eprintln!("worst polygon mismatch {:.3}% of area", worst * 100.0);
}
