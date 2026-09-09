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
