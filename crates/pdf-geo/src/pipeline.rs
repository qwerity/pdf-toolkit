//! Georeferencing of a single page: control table → correspondences → fit.

use serde::{Deserialize, Serialize};

use super::control::{
    ControlTable, candidate_correspondences, declared_scale_denominators, shape_correspondences,
    table_candidates_from_text_layer,
};
use super::fit::{Fit, FitConstraints, ransac_similarity};
use super::geojson::Georeference;
use pdf_geometry::model::{BBox, PageContent, Point};

/// Why a page could not be placed on the map.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "reason")]
pub enum GeoreferenceFailure {
    /// No turning-point table was found in the text layer.
    NoControlTable,
    /// A table was found but its coordinates are not in a recognised CRS.
    UnknownCrs,
    /// Table rows exist but no drawing marker could be matched to them.
    NoMarkerMatches { candidates: usize },
    /// Markers were found but no consistent transform explains them.
    NoConsistentFit { candidates: usize, tolerance_m: f64 },
}

/// Outcome of georeferencing one page.
#[derive(Debug, Clone)]
pub struct GeoreferenceResult {
    pub georeference: Georeference,
    pub table: ControlTable,
    pub fit: Fit,
    /// True when the table was read on another sheet of the same document
    /// and this page was placed by matching its drawn parcel to it.
    pub borrowed_table: bool,
}

/// Attempts to place a page in world coordinates using every supplied control
/// table, keeping whichever produces the best-supported fit.
///
/// Passing more than one table is how column-order ambiguity is resolved: the
/// wrong assignment produces no consistent similarity transform and loses.
pub fn georeference_page(
    page: &PageContent,
    tables: &[ControlTable],
    constraints: FitConstraints,
) -> Result<GeoreferenceResult, GeoreferenceFailure> {
    if tables.is_empty() {
        return Err(GeoreferenceFailure::NoControlTable);
    }
    let declared = declared_scale_denominators(page);
    let constraints = narrow_to_declared_scale(constraints, &declared);
    let mut total_candidates = 0usize;
    let mut best: Option<GeoreferenceResult> = None;
    let mut any_usable = false;

    for table in tables {
        if !table.is_usable() {
            continue;
        }
        any_usable = true;
        // Two independent ways of anchoring the sheet: the printed vertex
        // numbers when they survived as text, and the parcel outline matched by
        // shape when they did not. Both feed the same consensus step.
        let mut candidates = shape_correspondences(page, table);
        candidates.extend(candidate_correspondences(page, table));
        total_candidates += candidates.len();
        let Some(fit) = ransac_similarity(&candidates, constraints) else {
            continue;
        };

        let mut control_bbox = BBox::EMPTY;
        for &i in &fit.inliers {
            control_bbox.extend(candidates[i].world);
        }
        let fitted = fit.transform.drawing_scale_denominator();
        let georeference = Georeference {
            transform: fit.transform,
            crs: table.crs,
            rmse: fit.rmse,
            max_residual: fit.max_residual,
            control_points_used: fit.inliers.len(),
            fitted_scale_denominator: fitted,
            // Sheets often print several scales; keep the one nearest the fit.
            declared_scale_denominator: declared.iter().copied().min_by(|a, b| {
                (a - fitted)
                    .abs()
                    .partial_cmp(&(b - fitted).abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
            control_bbox,
            dimension_check: super::dimensions::check(table),
        };
        let result = GeoreferenceResult {
            georeference,
            table: table.clone(),
            fit: fit.clone(),
            borrowed_table: false,
        };
        let better = match &best {
            None => true,
            Some(b) => {
                (result.fit.inliers.len(), -result.fit.rmse) > (b.fit.inliers.len(), -b.fit.rmse)
            }
        };
        if better {
            best = Some(result);
        }
    }

    match best {
        Some(result) => Ok(result),
        None if !any_usable => Err(GeoreferenceFailure::UnknownCrs),
        None if total_candidates == 0 => {
            Err(GeoreferenceFailure::NoMarkerMatches { candidates: 0 })
        }
        None => Err(GeoreferenceFailure::NoConsistentFit {
            candidates: total_candidates,
            tolerance_m: constraints.tolerance,
        }),
    }
}

/// Fallback inlier tolerance, in metres, used for a second attempt once the
/// document's coordinate system is already known. Vertex numbers printed beside
/// a corner sit a little off it, and boundaries drawn as filled slivers put
/// their corners half a line-width away, so a looser second pass recovers
/// sheets the first one misses. Every other guard — declared scale, minimum
/// inliers, non-collinearity — still applies.
pub const RELAXED_TOLERANCE_M: f64 = 1.2;

/// Residual, in metres, below which a fit is treated as certain enough to fix
/// the whole document's coordinate system. Drawings are plotted from the same
/// survey coordinates the table lists, so a correct fit agrees to centimetres.
pub const CONFIDENT_RMSE_M: f64 = 0.05;

/// Georeferences a page using only what the PDF itself provides.
pub fn georeference_from_text_layer(
    page: &PageContent,
    constraints: FitConstraints,
) -> Result<GeoreferenceResult, GeoreferenceFailure> {
    georeference_page(page, &table_candidates_from_text_layer(page), constraints)
}

/// Forces every page of a document into one coordinate system.
///
/// Two orderings of a coordinate table can both yield a syntactically valid
/// Gauss-Krüger zone, and on a sheet where the fit is weak the wrong one
/// sometimes scores best. Pages of one dossier necessarily share a coordinate
/// system, so once any page has been placed to centimetre accuracy that CRS is
/// imposed on the rest — which is what stops a marginal page from being placed
/// a thousand kilometres away. Pages that then fail are reported as failures
/// rather than left with their inconsistent fit.
///
/// `tables` supplies the candidate control tables per page, so a caller that
/// recovered one with a vision model gets the same treatment as one read from
/// the text layer.
pub fn enforce_document_crs(
    pages: &[PageContent],
    tables: &[Vec<ControlTable>],
    results: &mut [Result<GeoreferenceResult, GeoreferenceFailure>],
    constraints: FitConstraints,
) {
    let Some(document_crs) = results
        .iter()
        .filter_map(|r| r.as_ref().ok())
        .filter(|r| r.fit.rmse <= CONFIDENT_RMSE_M)
        .max_by_key(|r| r.fit.inliers.len())
        .map(|r| r.table.crs)
    else {
        return;
    };

    let relaxed = FitConstraints {
        tolerance: RELAXED_TOLERANCE_M,
        ..constraints
    };
    for (index, result) in results.iter_mut().enumerate() {
        let disagrees = result.as_ref().is_ok_and(|r| r.table.crs != document_crs);
        if result.is_ok() && !disagrees {
            continue;
        }
        let consistent: Vec<ControlTable> = tables[index]
            .iter()
            .filter(|t| t.crs == document_crs)
            .cloned()
            .collect();
        if consistent.is_empty() {
            if disagrees {
                *result = Err(GeoreferenceFailure::UnknownCrs);
            }
            continue;
        }
        match georeference_page(&pages[index], &consistent, constraints)
            .or_else(|_| georeference_page(&pages[index], &consistent, relaxed))
        {
            Ok(better) => *result = Ok(better),
            Err(e) => {
                if disagrees {
                    *result = Err(e);
                }
            }
        }
    }
}

/// Lends the document's placed coordinate tables to the sheets that have
/// none of their own.
///
/// The survey sheet prints the parcel's coordinates once; the general plan,
/// the cadastral extract and the situation map draw the *same parcel* with
/// no table. Matching a sheet's drawn outline to a confidently placed
/// table's world polygon is [`shape_correspondences`]' existing job, so a
/// sibling that really shows the parcel fits it under every existing guard
/// — RANSAC consensus, scale bounds, spread ratio — and a sheet that does
/// not show the parcel simply finds no consistent transform and stays
/// local. Geometry decides, never proximity.
///
/// The borrowed table is stripped of what the donor sheet printed but this
/// one does not: its page-space `bbox` (meaningless on another sheet) and
/// its `printed_distances` (so `printed_dimensions` does not claim a check
/// this sheet never carried).
pub fn propagate_document_tables(
    pages: &[PageContent],
    results: &mut [Result<GeoreferenceResult, GeoreferenceFailure>],
    constraints: FitConstraints,
) {
    let mut donors: Vec<ControlTable> = Vec::new();
    for result in results.iter().filter_map(|r| r.as_ref().ok()) {
        if result.fit.rmse > CONFIDENT_RMSE_M {
            continue;
        }
        let mut table = result.table.clone();
        table.bbox = None;
        table.printed_distances = vec![None; table.rows.len()];
        // Dedup by the coordinates themselves: two parcels of one dossier
        // can share a corner count, and dropping the second would leave its
        // sibling sheets matched only against the wrong polygon.
        let duplicate = donors.iter().any(|d| {
            d.crs == table.crs
                && d.rows.len() == table.rows.len()
                && d.rows.iter().zip(&table.rows).all(|(a, b)| {
                    (a.projected.easting - b.projected.easting).abs() < 0.01
                        && (a.projected.northing - b.projected.northing).abs() < 0.01
                })
        });
        if !duplicate {
            donors.push(table);
        }
    }
    if donors.is_empty() {
        return;
    }

    let relaxed = FitConstraints {
        tolerance: RELAXED_TOLERANCE_M,
        ..constraints
    };
    for (index, result) in results.iter_mut().enumerate() {
        if result.is_ok() {
            continue;
        }
        if let Ok(mut placed) = georeference_page(&pages[index], &donors, constraints)
            .or_else(|_| georeference_page(&pages[index], &donors, relaxed))
            && borrowed_fit_is_credible(&placed)
        {
            placed.borrowed_table = true;
            *result = Ok(placed);
        }
    }
}

/// The extra evidence a borrowed-table placement must show.
///
/// Four points nearly-affinely match a great many rectangles — window
/// frames, title blocks — so a four-inlier subset of a larger parcel at
/// decimetre residual is exactly the false positive this pass must not
/// produce. Five or more correspondences over an irregular outline are
/// exponentially unlikely by chance; a *complete* match of a small parcel
/// is accepted only at the residual a correct plot actually shows.
fn borrowed_fit_is_credible(result: &GeoreferenceResult) -> bool {
    let inliers = result.fit.inliers.len();
    let polygon = crate::control::world_polygon(&result.table);
    // A complete match of a small parcel is accepted only when the parcel
    // could not be mistaken for sheet furniture: a similarity transform
    // maps ANY aspect-matching rectangle onto a rectangular quad with zero
    // residual, so a rectangular four-corner parcel proves nothing.
    inliers >= 5
        || (inliers >= polygon.len()
            && result.fit.rmse <= CONFIDENT_RMSE_M
            && !ring_is_rectangular(&polygon))
}

/// Whether every corner of a quad sits within a few degrees of a right
/// angle — the shape of every title block, frame cell and room outline.
fn ring_is_rectangular(polygon: &[Point]) -> bool {
    if polygon.len() != 4 {
        return false;
    }
    (0..4).all(|i| {
        let previous = polygon[(i + 3) % 4];
        let corner = polygon[i];
        let next = polygon[(i + 1) % 4];
        let a = (previous.x - corner.x, previous.y - corner.y);
        let b = (next.x - corner.x, next.y - corner.y);
        let dot = a.0 * b.0 + a.1 * b.1;
        let norms = (a.0.hypot(a.1) * b.0.hypot(b.1)).max(1e-12);
        // cos(84°) ≈ 0.105: corners within ±6° of square count as square.
        (dot / norms).abs() < 0.105
    })
}

/// Georeferences every page of a document from its own text layer.
pub fn georeference_document(
    pages: &[PageContent],
    constraints: FitConstraints,
) -> Vec<Result<GeoreferenceResult, GeoreferenceFailure>> {
    let tables: Vec<Vec<ControlTable>> =
        pages.iter().map(table_candidates_from_text_layer).collect();
    let mut results: Vec<Result<GeoreferenceResult, GeoreferenceFailure>> = pages
        .iter()
        .zip(&tables)
        .map(|(page, t)| georeference_page(page, t, constraints))
        .collect();
    enforce_document_crs(pages, &tables, &mut results, constraints);
    propagate_document_tables(pages, &mut results, constraints);
    results
}

/// The factor by which a page may plausibly have been rescaled after the sheet
/// was plotted. PDF post-processing tools routinely resize pages, so the fitted
/// scale need not equal the printed one — but it cannot differ wildly.
const MAX_PAGE_RESCALE: f64 = 3.0;

/// Restricts the acceptable scale range to what the sheet's own printed scale
/// allows, which is the single strongest guard against a plausible-looking fit
/// to the wrong control points.
fn narrow_to_declared_scale(constraints: FitConstraints, declared: &[f64]) -> FitConstraints {
    let (Some(min), Some(max)) = (
        declared.iter().copied().reduce(f64::min),
        declared.iter().copied().reduce(f64::max),
    ) else {
        return constraints;
    };
    FitConstraints {
        min_scale_denominator: constraints
            .min_scale_denominator
            .max(min / MAX_PAGE_RESCALE),
        max_scale_denominator: constraints
            .max_scale_denominator
            .min(max * MAX_PAGE_RESCALE),
        ..constraints
    }
}

/// Ratio between the fitted and printed sheet scale, when both are known. A
/// value far from 1.0 means the fit is wrong even if its residuals look small.
pub fn scale_agreement(georeference: &Georeference) -> Option<f64> {
    let declared = georeference.declared_scale_denominator?;
    if declared <= 0.0 {
        return None;
    }
    Some(georeference.fitted_scale_denominator / declared)
}

/// Centre of the georeferenced drawing in WGS 84, handy for framing a map.
pub fn center_lon_lat(georeference: &Georeference) -> Option<(f64, f64)> {
    if georeference.control_bbox.is_empty() {
        return None;
    }
    let c = georeference.control_bbox.center();
    let ll = georeference.crs.to_wgs84(super::crs::Projected {
        northing: c.y,
        easting: c.x,
    })?;
    Some((ll.lon, ll.lat))
}

/// Page-space extent covered by the drawing's control points, used to crop a
/// page image down to the survey area before showing it to a model.
pub fn control_page_bbox(page: &PageContent, table: &ControlTable) -> BBox {
    let mut bbox = BBox::EMPTY;
    for c in candidate_correspondences(page, table) {
        bbox.extend(Point::new(c.page.x, c.page.y));
    }
    bbox
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::{ControlRow, TableSource};
    use crate::crs::Projected;
    use pdf_geometry::model::{PaintMode, Rgb, SubPath, VectorPath};

    /// An irregular six-corner parcel in Gauss-Krüger zone 8, metres.
    fn parcel_world() -> Vec<(f64, f64)> {
        vec![
            (8_458_288.80, 4_440_484.10),
            (8_458_308.86, 4_440_494.82),
            (8_458_321.20, 4_440_486.30),
            (8_458_316.40, 4_440_470.75),
            (8_458_299.10, 4_440_463.20),
            (8_458_285.55, 4_440_472.40),
        ]
    }

    fn table() -> ControlTable {
        let rows = parcel_world()
            .iter()
            .enumerate()
            .map(|(i, &(e, n))| ControlRow {
                label: (i + 1).to_string(),
                projected: Projected {
                    northing: n,
                    easting: e,
                },
            })
            .collect();
        ControlTable::from_rows(rows, TableSource::TextLayer, None)
    }

    /// A page whose only content is the parcel outline, drawn under the
    /// given similarity (world → page units).
    fn sheet_with_parcel(scale: f64, angle_deg: f64, tx: f64, ty: f64) -> PageContent {
        let angle = angle_deg.to_radians();
        let (cos, sin) = (angle.cos(), angle.sin());
        let (e0, n0) = parcel_world()[0];
        let mut points: Vec<Point> = parcel_world()
            .iter()
            .map(|&(e, n)| {
                let (x, y) = (e - e0, n - n0);
                Point::new(
                    (x * cos - y * sin) * scale + tx,
                    (x * sin + y * cos) * scale + ty,
                )
            })
            .collect();
        points.push(points[0]);
        PageContent {
            page_number: 1,
            media_box: BBox {
                min_x: 0.0,
                min_y: 0.0,
                max_x: 842.0,
                max_y: 595.0,
            },
            paths: vec![VectorPath {
                subpaths: vec![SubPath {
                    points,
                    closed: true,
                }],
                mode: PaintMode::Stroke,
                stroke: Rgb::BLACK,
                fill: Rgb::BLACK,
                width: 0.5,
                layer: None,
            }],
            texts: Vec::new(),
            image_count: 0,
            image_coverage: 0.0,
        }
    }

    #[test]
    fn a_placed_table_propagates_to_a_tableless_sheet_showing_the_parcel() {
        // Donor at 1:500, sibling redrawn at 1:1000, rotated, elsewhere on
        // its sheet; a third sheet shows unrelated geometry.
        let donor = sheet_with_parcel(1.0 / 0.1764, 0.0, 200.0, 150.0);
        let sibling = sheet_with_parcel(1.0 / 0.3528, 30.0, 420.0, 300.0);
        // Same vertex count, different shape: an L-shaped outline. Shape,
        // not translation, is what must fail the match.
        let unrelated = {
            let mut page = sheet_with_parcel(1.0, 0.0, 0.0, 0.0);
            let l_shape = [
                (100.0, 100.0),
                (300.0, 100.0),
                (300.0, 200.0),
                (200.0, 200.0),
                (200.0, 320.0),
                (100.0, 320.0),
                (100.0, 100.0),
            ];
            page.paths[0].subpaths[0].points =
                l_shape.iter().map(|&(x, y)| Point::new(x, y)).collect();
            page
        };
        let constraints = FitConstraints::default();

        let donor_fit =
            georeference_page(&donor, &[table()], constraints).expect("the donor places");
        assert!(
            donor_fit.fit.rmse <= CONFIDENT_RMSE_M,
            "rmse {}",
            donor_fit.fit.rmse
        );
        assert!(!donor_fit.borrowed_table);

        let pages = [donor, sibling, unrelated];
        let mut results = vec![
            Ok(donor_fit),
            Err(GeoreferenceFailure::NoControlTable),
            Err(GeoreferenceFailure::NoControlTable),
        ];
        propagate_document_tables(&pages, &mut results, constraints);

        let placed = results[1].as_ref().expect("the sibling places");
        assert!(
            placed.borrowed_table,
            "the source must say the table was lent"
        );
        assert!(
            placed.fit.rmse < 0.5,
            "the parcel matches to sub-metre: {}",
            placed.fit.rmse
        );
        assert!(
            (placed.georeference.fitted_scale_denominator - 1000.0).abs() < 50.0,
            "the sibling's own scale is recovered: {}",
            placed.georeference.fitted_scale_denominator
        );
        assert!(
            placed.georeference.dimension_check.is_none(),
            "a lent table must not claim the donor's printed side lengths"
        );
        assert!(
            results[2].is_err(),
            "a sheet without the parcel stays honestly unplaced"
        );
    }
}
