//! What the `vector` command reports.
//!
//! These are the shapes a caller parses, so they are deliberately flat and
//! self-describing: every page carries its classification, its placement (or
//! the reason it has none), the CRS its coordinates are in, and the GeoJSON
//! itself. A caller never has to correlate two files to know what it is looking
//! at.

use geojson::{FeatureCollection, GeometryValue, Position};
use serde::Serialize;

use pdf_classify::{Confidence, PageClass, PageFeatures, PageVerdict};
use pdf_geo::crs::{Projected, ProjectedCrs};
use pdf_geo::geojson::ExportSummary;
use pdf_geo::pipeline::{GeoreferenceFailure, GeoreferenceResult};
use pdf_geometry::model::{BBox, Point};

pub use pdf_geo::local::Placement;

use crate::{PlacementSource, round};

/// Everything recovered from one document.
#[derive(Debug, Serialize)]
pub struct DocumentVector {
    /// Stem of the source file, used as the feature id prefix.
    pub document: String,
    pub source: String,
    /// Pages in the document, not in the selection.
    pub page_count: usize,
    pub pages: Vec<PageVector>,
    /// Pages that could not be read at all. Empty in the ordinary case.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

impl DocumentVector {
    /// Pages that carry coordinates on the ground.
    pub fn placed(&self) -> usize {
        self.pages
            .iter()
            .filter(|p| p.georeference.as_ref().is_some_and(|g| g.placed))
            .count()
    }
}

/// One page: what it is, where it is, and its geometry.
#[derive(Debug, Serialize)]
pub struct PageVector {
    pub page_number: usize,
    pub classification: Classification,
    pub georeference: Option<GeoreferenceReport>,
    /// The frame `geojson` is written in. `EPSG:4326` once placed; otherwise
    /// `local_metres` or `page_units`, which say the shapes are right but the
    /// position is not known.
    pub crs: String,
    pub summary: ExportSummary,
    /// The FeatureCollection itself, inline. Callers that only want the
    /// classification can ignore it; callers that want a map do not need a
    /// second file.
    pub geojson: FeatureCollection,
}

/// What kind of page this is, and how sure the rules are.
#[derive(Debug, Serialize)]
pub struct Classification {
    pub class: PageClass,
    pub confidence: Confidence,
    /// Why the rules decided that, in one line.
    pub reason: String,
    /// Whether the page carries a technical drawing of any kind.
    pub is_drawing: bool,
    /// Whether it is the kind of drawing worth trying to place on a map.
    pub is_mappable: bool,
    pub features: Features,
}

/// The structural measurements the classification was made from.
///
/// Reported because a caller disagreeing with a verdict needs to see what it
/// was based on, and because these are useful signals in their own right.
#[derive(Debug, Serialize)]
pub struct Features {
    pub path_count: usize,
    pub text_count: usize,
    pub image_count: usize,
    pub image_coverage: f64,
    pub vector_coverage: f64,
    pub line_density: f64,
    pub distinct_colors: usize,
    pub layer_count: usize,
    pub cadastral_codes: usize,
    pub declared_scales: Vec<f64>,
    pub has_control_table: bool,
    pub control_rows: usize,
}

/// Where a page sits on the ground, or why it does not.
#[derive(Debug, Serialize)]
pub struct GeoreferenceReport {
    pub placed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<PlacementSource>,
    /// Why placement failed. Present only when `placed` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<GeoreferenceFailure>,
    /// Coordinate reference system the fit was made in, e.g. `EPSG:28408`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crs: Option<String>,
    /// RMS residual over the control points, in metres. The number that says
    /// whether the placement is right.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rmse_m: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_residual_m: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub control_points_used: Option<usize>,
    /// Sheet scale the fit implies, recovered independently of the coordinates
    /// it was fitted from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fitted_scale_denominator: Option<f64>,
    /// Sheet scale actually printed on the page, where one was readable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declared_scale_denominator: Option<f64>,
    /// Rotation from the drawing's frame to true north, degrees CCW. Only
    /// meaningful for a placement made from references.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rotation_degrees: Option<f64>,
    /// Centre of the placed geometry, as `[longitude, latitude]`.
    ///
    /// Absent when the fit's own CRS could not be converted to WGS 84, which
    /// is the honest answer rather than reporting projected easting/northing
    /// in a field documented as degrees.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub center: Option<[f64; 2]>,
}

impl GeoreferenceReport {
    pub fn placed(source: PlacementSource, result: &GeoreferenceResult) -> Self {
        let g = &result.georeference;
        Self {
            placed: true,
            source: Some(source),
            failure: None,
            // The fit is made in the sheet's own projected CRS; the exported
            // coordinates are WGS 84. Reporting the source CRS is what lets a
            // caller check the datum assumption the placement rests on.
            crs: g.crs.epsg().map(|code| format!("EPSG:{code}")),
            rmse_m: Some(round(g.rmse, 4)),
            max_residual_m: Some(round(g.max_residual, 4)),
            control_points_used: Some(g.control_points_used),
            fitted_scale_denominator: Some(round(g.fitted_scale_denominator, 2)),
            declared_scale_denominator: g.declared_scale_denominator,
            rotation_degrees: None,
            // control_bbox is in the sheet's projected CRS -- the frame the fit
            // was made in -- so it has to come back through the projection to
            // be the degrees this field promises.
            center: centre_wgs84(&g.crs, &g.control_bbox),
        }
    }

    pub fn from_references(placement: &Placement, bounds: &BBox) -> Self {
        Self {
            placed: true,
            source: Some(PlacementSource::References),
            failure: None,
            crs: Some("EPSG:4326".into()),
            rmse_m: Some(round(placement.rmse_m, 4)),
            max_residual_m: Some(round(placement.max_residual_m, 4)),
            control_points_used: Some(placement.references),
            fitted_scale_denominator: None,
            declared_scale_denominator: None,
            rotation_degrees: Some(round(placement.rotation_degrees, 3)),
            center: Some(crate::centre(bounds)),
        }
    }

    pub fn unplaced(failure: GeoreferenceFailure) -> Self {
        Self {
            placed: false,
            source: None,
            failure: Some(failure),
            crs: None,
            rmse_m: None,
            max_residual_m: None,
            control_points_used: None,
            fitted_scale_denominator: None,
            declared_scale_denominator: None,
            rotation_degrees: None,
            center: None,
        }
    }
}

/// Converts a bounding box in a projected CRS to its centre in WGS 84.
fn centre_wgs84(crs: &ProjectedCrs, bounds: &BBox) -> Option<[f64; 2]> {
    let c = bounds.center();
    let lonlat = crs.to_wgs84(Projected {
        easting: c.x,
        northing: c.y,
    })?;
    Some([round(lonlat.lon, 7), round(lonlat.lat, 7)])
}

pub(crate) fn classification(verdict: &PageVerdict, features: &PageFeatures) -> Classification {
    Classification {
        class: verdict.class,
        confidence: verdict.confidence,
        reason: verdict.reason.clone(),
        is_drawing: verdict.class.is_drawing(),
        is_mappable: verdict.class.is_mappable(),
        features: Features {
            path_count: features.path_count,
            text_count: features.text_count,
            image_count: features.image_count,
            image_coverage: round(features.image_coverage, 6),
            vector_coverage: round(features.vector_coverage, 6),
            line_density: round(features.line_density, 4),
            distinct_colors: features.distinct_colors,
            layer_count: features.layer_count,
            cadastral_codes: features.cadastral_codes,
            declared_scales: features.declared_scales.clone(),
            has_control_table: features.has_control_table,
            control_rows: features.control_rows,
        },
    }
}

/// Rewrites a geometry's coordinates from the local frame onto the ground.
pub(crate) fn reproject(value: &mut GeometryValue, placement: &Placement, bounds: &mut BBox) {
    let mut convert = |p: &mut Position| {
        let s = p.as_slice();
        let placed = placement.to_wgs84(Point::new(s[0], s[1]));
        *p = Position::from([round(placed.lon, 7), round(placed.lat, 7)]);
        bounds.extend(Point::new(placed.lon, placed.lat));
    };
    match value {
        GeometryValue::Point { coordinates } => convert(coordinates),
        GeometryValue::LineString { coordinates } | GeometryValue::MultiPoint { coordinates } => {
            coordinates.iter_mut().for_each(&mut convert)
        }
        GeometryValue::Polygon { coordinates } | GeometryValue::MultiLineString { coordinates } => {
            coordinates.iter_mut().flatten().for_each(&mut convert)
        }
        GeometryValue::MultiPolygon { coordinates } => coordinates
            .iter_mut()
            .flatten()
            .flatten()
            .for_each(&mut convert),
        GeometryValue::GeometryCollection { geometries } => geometries
            .iter_mut()
            .for_each(|g| reproject(&mut g.value, placement, bounds)),
    }
}
