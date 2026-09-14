//! The elevation reader against the reference dossier, `data/plan/4.pdf`: its
//! four facades and its section print their levels down one side and their
//! grid axes along the foot. Private corpus data; the test skips without it
//! unless `PDF_TOOLKIT_REQUIRE_CORPUS` is set.

use std::path::Path;

use pdf_classify::sheet::SheetRole;
use pdf_geometry::PdfDocument;
use pdf_model::elevation::{self, ModelAxis, PlanAxes};
use pdf_model::params::{FloorParams, Params};
use pdf_model::sheet::Sheet;

fn document() -> Option<PdfDocument> {
    let pdf = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/plan/4.pdf");
    if pdf.exists() {
        Some(PdfDocument::open(&pdf).expect("the reference dossier opens"))
    } else if std::env::var_os("PDF_TOOLKIT_REQUIRE_CORPUS").is_some() {
        panic!("corpus required but {pdf:?} missing");
    } else {
        eprintln!("skipping: corpus absent");
        None
    }
}

#[test]
fn the_facades_and_the_section_read_their_levels_and_axes() {
    let Some(document) = document() else {
        return;
    };
    let mut params = Params::archicad_template();
    params.floors = vec![FloorParams {
        page: 13,
        name: "+0,00 նիշով հարկի հատակագիծ".into(),
        level_m: 0.0,
        top_m: 3.6,
    }];
    let plan = document.page_content(13).expect("the ground-floor plan");
    let origin = Sheet::read(&plan, &params).expect("the plan reads").origin;
    let axes = PlanAxes::read(&plan, origin, params.mm_per_point() / 1000.0);
    let cm = |v: Option<f64>| v.map(|v| (v * 100.0).round() / 100.0);
    assert_eq!(cm(axes.coordinate(ModelAxis::X, "2")), Some(4.0));
    assert_eq!(cm(axes.coordinate(ModelAxis::X, "3")), Some(11.0));
    assert_eq!(cm(axes.coordinate(ModelAxis::Y, "Բ")), Some(5.9));

    // The sheet titles name the axes read across, left to right.
    for (page, role, axis, rightwards, title) in [
        (16, SheetRole::Elevation, ModelAxis::Y, true, "Ա-Բ"),
        (17, SheetRole::Elevation, ModelAxis::Y, false, "Բ-Ա"),
        (18, SheetRole::Elevation, ModelAxis::X, true, "1-3"),
        (19, SheetRole::Elevation, ModelAxis::X, false, "3-1"),
        (20, SheetRole::Section, ModelAxis::X, true, "1-1"),
    ] {
        let content = document.page_content(page).expect("the sheet");
        let sheet = elevation::read(&content, role, 75.0, &axes)
            .unwrap_or_else(|| panic!("page {page} ({title}) reads"));
        assert_eq!(sheet.across.axis, axis, "page {page} ({title})");
        assert_eq!(
            sheet.across.per_m > 0.0,
            rightwards,
            "page {page} ({title})"
        );
        assert!(
            sheet.levels.marks >= 6,
            "page {page} ({title}): {} marks agree",
            sheet.levels.marks
        );
    }

    // Facade 1-3: the +4,850 mark's triangle touches its line at 474.7, and
    // axis 3's bubble is centred at 838.5.
    let facade = elevation::read(
        &document.page_content(18).expect("facade 1-3"),
        SheetRole::Elevation,
        75.0,
        &axes,
    )
    .expect("facade 1-3 reads");
    assert!(
        (facade.levels.level_at(474.7) - 4.85).abs() < 0.01,
        "{:?}",
        facade.levels
    );
    assert!(
        (facade.across.page_x(11.0) - 838.5).abs() < 1.0,
        "{:?}",
        facade.across
    );
}
