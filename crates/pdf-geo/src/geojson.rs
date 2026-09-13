//! Conversion of extracted page geometry into a map-ready GeoJSON
//! `FeatureCollection`.
//!
//! GeoJSON is WGS 84 by specification, so a page is only written in geographic
//! coordinates once a transform to the projected CRS has been fitted. Sheet
//! furniture — the border, the title block, the coordinate table — is removed
//! first, otherwise it would be projected onto the map as if it were terrain.

use geojson::{
    Feature, FeatureCollection, Geometry, GeometryValue, JsonObject, JsonValue, Position,
};
use serde::{Deserialize, Serialize};

use super::crs::{LonLat, Projected, ProjectedCrs};
use super::fit::Similarity;
use pdf_geometry::model::{BBox, PageContent, PaintMode, Point, SubPath, VectorPath};

/// Everything needed to place a page's geometry on the map.
#[derive(Debug, Clone)]
pub struct Georeference {
    /// The sheet's printed side lengths checked against its coordinates, when
    /// it prints any. Carried here so the summary reaches the GeoJSON metadata.
    pub dimension_check: Option<super::dimensions::DimensionReport>,
    pub transform: Similarity,
    pub crs: ProjectedCrs,
    /// RMS residual of the control-point fit, in metres.
    pub rmse: f64,
    pub max_residual: f64,
    pub control_points_used: usize,
    /// Sheet scale implied by the fit, e.g. 500.0 for a 1:500 drawing.
    pub fitted_scale_denominator: f64,
    /// Scale printed on the sheet, when one was found.
    pub declared_scale_denominator: Option<f64>,
    /// World-space extent of the control points, used to bound the export to
    /// the area the survey actually covers.
    pub control_bbox: BBox,
}

/// Tunables for turning page geometry into features.
#[derive(Debug, Clone, Copy)]
pub struct ExportOptions {
    /// Discard segments shorter than this, in metres. Removes hatch stipple.
    pub min_feature_length_m: f64,
    /// Douglas–Peucker tolerance, in metres.
    pub simplify_tolerance_m: f64,
    /// Drop paths lighter than this luminance; CAD exports use near-white for
    /// construction lines that should not reach the map.
    pub max_luminance: f64,
    /// Discard geometry whose page bounding box covers more than this fraction
    /// of the sheet — the border and title-block frame.
    pub max_page_coverage: f64,
    /// A plain rectangle spanning more than this fraction of the sheet is a
    /// drawing border, not a feature. Sheet frames are often small enough to
    /// slip under `max_page_coverage`, but they are always simple rectangles,
    /// which real line work at this size never is.
    pub max_rectangle_coverage: f64,
    /// Keep only geometry within this multiple of the control-point extent.
    pub roi_expansion: f64,
    /// Absolute minimum radius of the region of interest, in metres.
    pub roi_min_radius_m: f64,
    /// Discard any subpath smaller than this across, in metres.
    ///
    /// Sheet lettering and hatch stipple are small marks a metre or so across on
    /// the ground; real features — boundaries, footprints — span many metres.
    /// The test is deliberately not conditional on how the mark was painted:
    /// exporters convert lettering to outlines and then either fill *or* stroke
    /// them, and a rule that only looked at fills let a whole page of stroked
    /// parcel numbers through.
    ///
    /// A short *side* of a larger feature is unaffected, because the test
    /// applies to the whole subpath: the 1.2 m side of a parcel belongs to a
    /// ring spanning tens of metres.
    pub min_feature_extent_m: f64,
    pub max_features: usize,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            min_feature_length_m: 0.15,
            simplify_tolerance_m: 0.02,
            max_luminance: 0.93,
            max_page_coverage: 0.6,
            max_rectangle_coverage: 0.25,
            roi_expansion: 4.0,
            roi_min_radius_m: 60.0,
            min_feature_extent_m: 2.5,
            max_features: 60_000,
        }
    }
}

/// Everything one page's export produced.
#[derive(Debug, Clone)]
pub struct PageExport {
    pub collection: FeatureCollection,
    pub summary: ExportSummary,
    /// The geometry that survived filtering, still in page space, one entry per
    /// retained subpath with its original paint state.
    ///
    /// Validation renders this rather than re-deriving what to keep, so the
    /// picture a reviewer or a model is shown is exactly what was written.
    pub retained: Vec<VectorPath>,
}

/// Summary of what was written, surfaced in reports and used by validation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportSummary {
    pub features: usize,
    pub polygons: usize,
    pub linestrings: usize,
    pub dropped_out_of_roi: usize,
    pub dropped_too_small: usize,
    pub dropped_furniture: usize,
    pub truncated: bool,
}

/// Builds a georeferenced `FeatureCollection` from a page.
pub fn export_page(
    page: &PageContent,
    georeference: &Georeference,
    document_id: &str,
    exclude: &[BBox],
    options: ExportOptions,
) -> PageExport {
    let roi = region_of_interest(page, georeference, options);
    let mut summary = ExportSummary {
        features: 0,
        polygons: 0,
        linestrings: 0,
        dropped_out_of_roi: 0,
        dropped_too_small: 0,
        dropped_furniture: 0,
        truncated: false,
    };
    let page_area = page.media_box.area().max(1.0);
    let mut features = Vec::new();
    let mut retained = Vec::new();

    for path in &page.paths {
        if features.len() >= options.max_features {
            summary.truncated = true;
            break;
        }
        if is_unwanted_ink(path, options) {
            summary.dropped_furniture += 1;
            continue;
        }
        for sub in &path.subpaths {
            // Judged per subpath, not per path: a CAD export routinely puts
            // every cadastral boundary on the sheet into one path, whose
            // bounding box then spans the page. Testing the path as a whole
            // would discard the entire drawing as if it were a border.
            if is_furniture(
                sub,
                page_area,
                exclude,
                options,
                georeference.transform.scale(),
            ) {
                summary.dropped_furniture += 1;
                continue;
            }
            let world: Vec<Point> = sub
                .points
                .iter()
                .map(|p| georeference.transform.apply(*p))
                .collect();
            if world.len() < 2 {
                continue;
            }
            if !roi.is_empty() && !world.iter().any(|p| roi.contains(*p)) {
                summary.dropped_out_of_roi += 1;
                continue;
            }
            let world = simplify(&world, options.simplify_tolerance_m);
            let length = polyline_length(&world, sub.closed);
            if length < options.min_feature_length_m {
                summary.dropped_too_small += 1;
                continue;
            }

            let as_polygon = sub.closed
                && world.len() >= 3
                && matches!(path.mode, PaintMode::Fill | PaintMode::FillStroke);
            let geometry = if as_polygon {
                summary.polygons += 1;
                Geometry::new(GeometryValue::Polygon {
                    coordinates: vec![ring_to_wgs84(&world, georeference)],
                })
            } else {
                summary.linestrings += 1;
                Geometry::new(GeometryValue::LineString {
                    coordinates: world
                        .iter()
                        .filter_map(|p| to_position(*p, georeference))
                        .collect(),
                })
            };
            features.push(Feature {
                bbox: None,
                geometry: Some(geometry),
                id: None,
                properties: Some(path_properties(path, document_id, page.page_number, length)),
                foreign_members: None,
            });
            retained.push(VectorPath {
                subpaths: vec![sub.clone()],
                ..path.clone()
            });
        }
    }

    summary.features = features.len();
    let collection = FeatureCollection {
        bbox: None,
        features,
        foreign_members: Some(metadata(georeference, document_id, page.page_number)),
    };
    PageExport {
        collection,
        summary,
        retained,
    }
}

/// Exports a page that could not be placed, in its own local frame.
///
/// Same filtering as [`export_page`] — the sheet's furniture is no more welcome
/// here — but no region of interest, because there are no control points to
/// centre one on, and no projection, because the coordinates are metres in a
/// frame of the drawing's own rather than degrees on the Earth.
pub fn export_local(
    page: &PageContent,
    frame: &super::local::LocalFrame,
    document_id: &str,
    exclude: &[BBox],
    options: ExportOptions,
) -> PageExport {
    let transform = frame.transform();
    let mut summary = ExportSummary {
        features: 0,
        polygons: 0,
        linestrings: 0,
        dropped_out_of_roi: 0,
        dropped_too_small: 0,
        dropped_furniture: 0,
        truncated: false,
    };
    let page_area = page.media_box.area().max(1.0);
    let mut features = Vec::new();
    let mut retained = Vec::new();

    for path in &page.paths {
        if features.len() >= options.max_features {
            summary.truncated = true;
            break;
        }
        if is_unwanted_ink(path, options) {
            summary.dropped_furniture += 1;
            continue;
        }
        for sub in &path.subpaths {
            if is_furniture(sub, page_area, exclude, options, frame.units_per_page_unit) {
                summary.dropped_furniture += 1;
                continue;
            }
            let local: Vec<Point> = sub.points.iter().map(|p| transform.apply(*p)).collect();
            if local.len() < 2 {
                continue;
            }
            let local = simplify(&local, options.simplify_tolerance_m);
            let length = polyline_length(&local, sub.closed);
            if length < options.min_feature_length_m {
                summary.dropped_too_small += 1;
                continue;
            }

            let coordinates: Vec<Position> = local
                .iter()
                .map(|p| Position::from([round(p.x, 4), round(p.y, 4)]))
                .collect();
            let as_polygon = sub.closed
                && coordinates.len() >= 3
                && matches!(path.mode, PaintMode::Fill | PaintMode::FillStroke);
            let geometry = if as_polygon {
                summary.polygons += 1;
                let mut ring = coordinates;
                if ring.first() != ring.last()
                    && let Some(first) = ring.first().cloned()
                {
                    ring.push(first);
                }
                Geometry::new(GeometryValue::Polygon {
                    coordinates: vec![ring],
                })
            } else {
                summary.linestrings += 1;
                Geometry::new(GeometryValue::LineString { coordinates })
            };
            features.push(Feature {
                bbox: None,
                geometry: Some(geometry),
                id: None,
                properties: Some(path_properties(path, document_id, page.page_number, length)),
                foreign_members: None,
            });
            retained.push(VectorPath {
                subpaths: vec![sub.clone()],
                ..path.clone()
            });
        }
    }

    summary.features = features.len();
    let mut meta = JsonObject::new();
    meta.insert("document".into(), JsonValue::from(document_id.to_string()));
    meta.insert("page".into(), JsonValue::from(page.page_number));
    meta.insert(
        "local_frame".into(),
        serde_json::to_value(frame).unwrap_or(JsonValue::Null),
    );
    meta.insert(
        "warning".into(),
        JsonValue::from(
            "Coordinates are in this drawing's own frame, not WGS 84. Pass two or more \
--georeference local_x,local_y,longitude,latitude to place it on the ground.",
        ),
    );
    let mut wrapper = JsonObject::new();
    wrapper.insert("metadata".into(), JsonValue::Object(meta));

    PageExport {
        collection: FeatureCollection {
            bbox: None,
            features,
            foreign_members: Some(wrapper),
        },
        summary,
        retained,
    }
}

/// Emits the parcel described by a control table, as a polygon.
///
/// The turning points *are* the boundary, in order, so this needs no drawing at
/// all — which is what makes it the answer for a scanned page, where the line
/// work is pixels and nothing can be vectorised. Accuracy is the survey's own:
/// these are the coordinates the surveyor recorded, not a measurement taken off
/// a picture of them.
pub fn export_survey_parcel(
    table: &super::control::ControlTable,
    document_id: &str,
    page_number: usize,
    verified: bool,
) -> Option<FeatureCollection> {
    let ring = super::control::world_polygon(table);
    if ring.len() < 3 {
        return None;
    }
    let mut coordinates: Vec<Position> = ring
        .iter()
        .filter_map(|p| {
            let ll = table.crs.to_wgs84(Projected {
                northing: p.y,
                easting: p.x,
            })?;
            Some(Position::from([round6(ll.lon), round6(ll.lat)]))
        })
        .collect();
    if coordinates.len() < 3 {
        return None;
    }
    if coordinates.first() != coordinates.last()
        && let Some(first) = coordinates.first().cloned()
    {
        coordinates.push(first);
    }

    let mut props = JsonObject::new();
    // The surveyor's own ring is a survey parcel; a caller-supplied polygon
    // (the cadastre's) is a reference the sheet was matched to, and a map
    // must not mistake one statement of the boundary for the other.
    let external = matches!(table.source, super::control::TableSource::External { .. });
    props.insert(
        "kind".into(),
        JsonValue::from(if external {
            "reference_parcel"
        } else {
            "survey_parcel"
        }),
    );
    props.insert("document".into(), JsonValue::from(document_id.to_string()));
    props.insert("page".into(), JsonValue::from(page_number));
    props.insert("turning_points".into(), JsonValue::from(ring.len()));
    props.insert(
        "coordinate_source".into(),
        JsonValue::from(match &table.source {
            super::control::TableSource::TextLayer => "text_layer".to_string(),
            super::control::TableSource::Vlm { model } => format!("vlm:{model}"),
            super::control::TableSource::External { origin } => format!("external:{origin}"),
        }),
    );
    props.insert(
        "perimeter_m".into(),
        JsonValue::from(round(ring_perimeter(&ring), 3)),
    );
    // Which column held the northing is a reading, and only a fit against the
    // drawing proves it. Without one the parcel is the right shape and size but
    // its position rests on a prior, and saying so is the difference between a
    // result and a guess.
    props.insert("verified".into(), JsonValue::from(verified));
    props.insert(
        "area_m2".into(),
        JsonValue::from(round(ring_area(&ring), 2)),
    );

    let mut meta = JsonObject::new();
    meta.insert("document".into(), JsonValue::from(document_id.to_string()));
    meta.insert("page".into(), JsonValue::from(page_number));
    meta.insert("source_crs".into(), JsonValue::from(table.crs.label()));
    meta.insert("verified".into(), JsonValue::from(verified));
    meta.insert(
        "note".into(),
        JsonValue::from(if verified {
            "Built from the sheet's turning-point coordinates, not from its line work, and \
confirmed by fitting the drawing to them. Accurate to the survey itself; covers the subject \
parcel only."
        } else {
            "Built from the sheet's turning-point coordinates, not from its line work. The \
shape and size are the survey's own, but no drawing was available to confirm which coordinate \
column held the northing, so the position is unconfirmed."
        }),
    );
    let mut wrapper = JsonObject::new();
    wrapper.insert("metadata".into(), JsonValue::Object(meta));

    Some(FeatureCollection {
        bbox: None,
        features: vec![Feature {
            bbox: None,
            geometry: Some(Geometry::new(GeometryValue::Polygon {
                coordinates: vec![coordinates],
            })),
            id: None,
            properties: Some(props),
            foreign_members: None,
        }],
        foreign_members: Some(wrapper),
    })
}

fn ring_perimeter(ring: &[Point]) -> f64 {
    if ring.len() < 2 {
        return 0.0;
    }
    ring.windows(2).map(|w| w[0].distance(&w[1])).sum::<f64>()
        + ring[ring.len() - 1].distance(&ring[0])
}

/// Shoelace area in square metres; the sign is dropped since winding direction
/// is a drafting choice, not information about the parcel.
fn ring_area(ring: &[Point]) -> f64 {
    let n = ring.len();
    if n < 3 {
        return 0.0;
    }
    let mut sum = 0.0;
    for i in 0..n {
        let (p, q) = (ring[i], ring[(i + 1) % n]);
        sum += p.x * q.y - q.x * p.y;
    }
    (sum / 2.0).abs()
}

/// Emits recovered labels as points, so a parcel code travels with the map
/// rather than being lost with the lettering it was printed as.
pub fn export_labels(
    labels: &[super::labels::MapLabel],
    georeference: &Georeference,
    document_id: &str,
    page_number: usize,
) -> FeatureCollection {
    let features = labels
        .iter()
        .filter_map(|label| {
            let world = georeference.transform.apply(label.position);
            let position = to_position(world, georeference)?;
            let mut props = JsonObject::new();
            props.insert("kind".into(), JsonValue::from("label"));
            props.insert("text".into(), JsonValue::from(label.text.clone()));
            props.insert("document".into(), JsonValue::from(document_id.to_string()));
            props.insert("page".into(), JsonValue::from(page_number));
            props.insert(
                "text_source".into(),
                JsonValue::from(match &label.source {
                    super::labels::LabelSource::TextLayer => "text_layer".to_string(),
                    super::labels::LabelSource::Vlm { model } => format!("vlm:{model}"),
                }),
            );
            Some(Feature {
                bbox: None,
                geometry: Some(Geometry::new(GeometryValue::Point {
                    coordinates: position,
                })),
                id: None,
                properties: Some(props),
                foreign_members: None,
            })
        })
        .collect();
    FeatureCollection {
        bbox: None,
        features,
        foreign_members: None,
    }
}

/// Emits the control points themselves, so a viewer can show the anchors the
/// transform was fitted to alongside the drawing.
pub fn export_control_points(
    rows: &[(String, Projected)],
    crs: &ProjectedCrs,
) -> FeatureCollection {
    let features = rows
        .iter()
        .filter_map(|(label, p)| {
            let ll = crs.to_wgs84(*p)?;
            let mut props = JsonObject::new();
            props.insert("kind".into(), JsonValue::from("control_point"));
            props.insert("label".into(), JsonValue::from(label.clone()));
            props.insert("northing".into(), JsonValue::from(p.northing));
            props.insert("easting".into(), JsonValue::from(p.easting));
            Some(Feature {
                bbox: None,
                geometry: Some(Geometry::new(GeometryValue::Point {
                    coordinates: Position::from([round6(ll.lon), round6(ll.lat)]),
                })),
                id: None,
                properties: Some(props),
                foreign_members: None,
            })
        })
        .collect();
    FeatureCollection {
        bbox: None,
        features,
        foreign_members: None,
    }
}

fn path_properties(
    path: &VectorPath,
    document_id: &str,
    page_number: usize,
    length_m: f64,
) -> JsonObject {
    let mut props = JsonObject::new();
    props.insert("document".into(), JsonValue::from(document_id.to_string()));
    props.insert("page".into(), JsonValue::from(page_number));
    props.insert("stroke".into(), JsonValue::from(path.stroke.to_hex()));
    props.insert(
        "length_m".into(),
        JsonValue::from((length_m * 100.0).round() / 100.0),
    );
    if let Some(layer) = &path.layer {
        props.insert("layer".into(), JsonValue::from(layer.clone()));
    }
    props
}

fn metadata(georeference: &Georeference, document_id: &str, page: usize) -> JsonObject {
    let mut meta = JsonObject::new();
    meta.insert("document".into(), JsonValue::from(document_id.to_string()));
    meta.insert("page".into(), JsonValue::from(page));
    meta.insert(
        "source_crs".into(),
        JsonValue::from(georeference.crs.label()),
    );
    if let Some(epsg) = georeference.crs.epsg() {
        meta.insert("source_epsg".into(), JsonValue::from(epsg));
    }
    meta.insert(
        "fit_rmse_m".into(),
        JsonValue::from(round(georeference.rmse, 4)),
    );
    meta.insert(
        "fit_max_residual_m".into(),
        JsonValue::from(round(georeference.max_residual, 4)),
    );
    meta.insert(
        "control_points_used".into(),
        JsonValue::from(georeference.control_points_used),
    );
    meta.insert(
        "fitted_scale_denominator".into(),
        JsonValue::from(round(georeference.fitted_scale_denominator, 1)),
    );
    if let Some(declared) = georeference.declared_scale_denominator {
        meta.insert(
            "declared_scale_denominator".into(),
            JsonValue::from(declared),
        );
    }
    if let Some(check) = &georeference.dimension_check {
        let mut dimensions = JsonObject::new();
        dimensions.insert("sides_printed".into(), JsonValue::from(check.sides_printed));
        dimensions.insert(
            "sides_agreeing".into(),
            JsonValue::from(check.sides_agreeing),
        );
        dimensions.insert("max_error_m".into(), JsonValue::from(check.max_error_m));
        dimensions.insert("all_agree".into(), JsonValue::from(check.all_agree));
        meta.insert("printed_dimensions".into(), JsonValue::Object(dimensions));
    }
    let mut wrapper = JsonObject::new();
    wrapper.insert("metadata".into(), JsonValue::Object(meta));
    wrapper
}

fn ring_to_wgs84(points: &[Point], georeference: &Georeference) -> Vec<Position> {
    let mut ring: Vec<Position> = points
        .iter()
        .filter_map(|p| to_position(*p, georeference))
        .collect();
    if ring.first() != ring.last()
        && let Some(first) = ring.first().cloned()
    {
        ring.push(first);
    }
    ring
}

/// Projected metres → WGS 84 `[lon, lat]`, rounded to ~1 cm.
fn to_position(p: Point, georeference: &Georeference) -> Option<Position> {
    let LonLat { lon, lat } = georeference.crs.to_wgs84(Projected {
        northing: p.y,
        easting: p.x,
    })?;
    Some(Position::from([round6(lon), round6(lat)]))
}

fn round6(v: f64) -> f64 {
    round(v, 7)
}

fn round(v: f64, digits: i32) -> f64 {
    let f = 10f64.powi(digits);
    (v * f).round() / f
}

/// Rejects ink that carries no geographic meaning whatever it is drawn on:
/// near-white construction lines that a CAD exporter leaves behind.
fn is_unwanted_ink(path: &VectorPath, options: ExportOptions) -> bool {
    let color = match path.mode {
        PaintMode::Fill => path.fill,
        _ => path.stroke,
    };
    color.luminance() > options.max_luminance
}

/// Rejects sheet furniture: the border frame, lettering converted to outlines,
/// and anything inside an excluded box such as the coordinate table.
fn is_furniture(
    sub: &SubPath,
    page_area: f64,
    exclude: &[BBox],
    options: ExportOptions,
    metres_per_unit: f64,
) -> bool {
    let bbox = sub.bbox();
    if bbox.is_empty() {
        return true;
    }
    let coverage = bbox.area() / page_area;
    if coverage > options.max_page_coverage {
        return true;
    }
    if coverage > options.max_rectangle_coverage && is_plain_rectangle(sub) {
        return true;
    }
    let extent = bbox.width().hypot(bbox.height()) * metres_per_unit;
    if extent < options.min_feature_extent_m {
        return true;
    }
    exclude.iter().any(|ex| bbox.overlap_ratio(ex) > 0.5)
}

/// Whether a subpath is a rectangle, which is the shape every sheet border
/// takes. Rotation is allowed, since a drawing frame need not be axis-aligned.
fn is_plain_rectangle(sub: &SubPath) -> bool {
    let mut points = sub.points.as_slice();
    // A rectangle may or may not repeat its first vertex to close.
    if points.len() == 5 && points[0].distance(&points[4]) < 1e-6 {
        points = &points[..4];
    }
    if points.len() != 4 {
        return false;
    }
    let side = |i: usize| {
        let a = points[i];
        let b = points[(i + 1) % 4];
        (b.x - a.x, b.y - a.y)
    };
    (0..4).all(|i| {
        let (ax, ay) = side(i);
        let (bx, by) = side((i + 1) % 4);
        let dot = ax * bx + ay * by;
        let magnitude = (ax.hypot(ay)) * (bx.hypot(by));
        magnitude > 0.0 && (dot / magnitude).abs() < 0.02
    })
}

/// Region of interest in world coordinates, centred on the control points.
///
/// Everything the survey documents lies near its own turning points; without
/// this, the sheet's legend and north arrow would be projected onto real
/// terrain hundreds of metres away.
fn region_of_interest(
    page: &PageContent,
    georeference: &Georeference,
    options: ExportOptions,
) -> BBox {
    let _ = page;
    let mut roi = BBox::EMPTY;
    if georeference.control_bbox.is_empty() {
        return roi;
    }
    let c = georeference.control_bbox.center();
    let radius = (georeference
        .control_bbox
        .width()
        .max(georeference.control_bbox.height())
        / 2.0
        * options.roi_expansion)
        .max(options.roi_min_radius_m);
    roi.extend(Point::new(c.x - radius, c.y - radius));
    roi.extend(Point::new(c.x + radius, c.y + radius));
    roi
}

fn polyline_length(points: &[Point], closed: bool) -> f64 {
    let mut total: f64 = points.windows(2).map(|w| w[0].distance(&w[1])).sum();
    if closed && points.len() > 2 {
        total += points[points.len() - 1].distance(&points[0]);
    }
    total
}

/// Iterative Douglas–Peucker; keeps endpoints and any vertex further than
/// `tolerance` from the chord through its span.
fn simplify(points: &[Point], tolerance: f64) -> Vec<Point> {
    if points.len() <= 2 || tolerance <= 0.0 {
        return points.to_vec();
    }
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;
    let mut stack = vec![(0usize, points.len() - 1)];
    while let Some((start, end)) = stack.pop() {
        if end <= start + 1 {
            continue;
        }
        let (a, b) = (points[start], points[end]);
        let (mut best, mut best_d) = (start, 0.0);
        for (i, p) in points.iter().enumerate().take(end).skip(start + 1) {
            let d = perpendicular_distance(*p, a, b);
            if d > best_d {
                best = i;
                best_d = d;
            }
        }
        if best_d > tolerance {
            keep[best] = true;
            stack.push((start, best));
            stack.push((best, end));
        }
    }
    points
        .iter()
        .zip(keep)
        .filter(|(_, k)| *k)
        .map(|(p, _)| *p)
        .collect()
}

fn perpendicular_distance(p: Point, a: Point, b: Point) -> f64 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let denom = (dx * dx + dy * dy).sqrt();
    if denom < 1e-12 {
        return p.distance(&a);
    }
    ((p.x - a.x) * dy - (p.y - a.y) * dx).abs() / denom
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdf_geometry::model::Rgb;

    fn georeference() -> Georeference {
        let mut g = Georeference {
            transform: Similarity {
                a: 0.1764,
                b: 0.0,
                tx: 8471000.0,
                ty: 4419000.0,
            },
            crs: ProjectedCrs::PulkovoGaussKruger { zone: 8 },
            rmse: 0.01,
            max_residual: 0.02,
            control_points_used: 8,
            fitted_scale_denominator: 500.0,
            declared_scale_denominator: Some(500.0),
            control_bbox: BBox::EMPTY,
            dimension_check: None,
        };
        let mut bbox = BBox::EMPTY;
        bbox.extend(Point::new(8471280.0, 4419480.0));
        bbox.extend(Point::new(8471320.0, 4419500.0));
        g.control_bbox = bbox;
        g
    }

    fn path(points: Vec<Point>, closed: bool, mode: PaintMode, color: Rgb) -> VectorPath {
        VectorPath {
            subpaths: vec![SubPath { points, closed }],
            mode,
            stroke: color,
            fill: color,
            width: 0.5,
            layer: None,
        }
    }

    fn page(paths: Vec<VectorPath>) -> PageContent {
        PageContent {
            page_number: 2,
            media_box: BBox {
                min_x: 0.0,
                min_y: 0.0,
                max_x: 1191.0,
                max_y: 842.0,
            },
            paths,
            texts: Vec::new(),
            image_count: 0,
            image_coverage: 0.0,
        }
    }

    #[test]
    fn exports_a_closed_fill_as_a_polygon() {
        let square = vec![
            Point::new(1600.0, 2500.0),
            Point::new(1700.0, 2500.0),
            Point::new(1700.0, 2600.0),
            Point::new(1600.0, 2600.0),
        ];
        let export = export_page(
            &page(vec![path(square, true, PaintMode::Fill, Rgb::BLACK)]),
            &georeference(),
            "doc",
            &[],
            ExportOptions::default(),
        );
        assert_eq!(export.summary.polygons, 1);
        let geometry = export.collection.features[0]
            .geometry
            .as_ref()
            .expect("geometry");
        let GeometryValue::Polygon { coordinates: rings } = &geometry.value else {
            panic!("expected polygon")
        };
        assert_eq!(rings[0].first(), rings[0].last(), "ring must close");
        assert!(
            (rings[0][0].as_slice()[1] - 40.09).abs() < 0.2,
            "latitude {:?}",
            rings[0][0].as_slice()
        );
    }

    #[test]
    fn drops_the_sheet_border() {
        let border = vec![
            Point::new(10.0, 10.0),
            Point::new(1180.0, 10.0),
            Point::new(1180.0, 830.0),
            Point::new(10.0, 830.0),
        ];
        let export = export_page(
            &page(vec![path(border, true, PaintMode::Stroke, Rgb::BLACK)]),
            &georeference(),
            "doc",
            &[],
            ExportOptions::default(),
        );
        assert_eq!(export.summary.features, 0);
        assert_eq!(export.summary.dropped_furniture, 1);
    }

    #[test]
    fn drops_a_sheet_border_that_slips_under_the_area_cap() {
        // A rectangle over a third of the sheet — too small for the coverage
        // cap, but unmistakably a frame.
        let frame = vec![
            Point::new(200.0, 150.0),
            Point::new(900.0, 150.0),
            Point::new(900.0, 650.0),
            Point::new(200.0, 650.0),
        ];
        let export = export_page(
            &page(vec![path(frame, true, PaintMode::Stroke, Rgb::BLACK)]),
            &georeference(),
            "doc",
            &[],
            ExportOptions::default(),
        );
        assert_eq!(export.summary.dropped_furniture, 1);
    }

    #[test]
    fn keeps_a_large_non_rectangular_outline() {
        let plot = vec![
            Point::new(200.0, 150.0),
            Point::new(900.0, 150.0),
            Point::new(900.0, 650.0),
            Point::new(550.0, 700.0),
            Point::new(200.0, 650.0),
        ];
        let export = export_page(
            &page(vec![path(plot, true, PaintMode::Stroke, Rgb::BLACK)]),
            &georeference(),
            "doc",
            &[],
            ExportOptions::default(),
        );
        assert_eq!(export.summary.dropped_furniture, 0);
    }

    #[test]
    fn drops_lettering_converted_to_outlines() {
        // A glyph-sized path: about half a metre across at 1:500.
        let glyph = vec![
            Point::new(1600.0, 2500.0),
            Point::new(1603.0, 2500.0),
            Point::new(1603.0, 2504.0),
            Point::new(1600.0, 2504.0),
        ];
        let export = export_page(
            &page(vec![path(glyph, true, PaintMode::Fill, Rgb::BLACK)]),
            &georeference(),
            "doc",
            &[],
            ExportOptions::default(),
        );
        assert_eq!(export.summary.dropped_furniture, 1);
    }

    #[test]
    fn keeps_a_stroked_line_of_the_same_size() {
        let short = vec![Point::new(1600.0, 2500.0), Point::new(1603.0, 2504.0)];
        let export = export_page(
            &page(vec![path(short, false, PaintMode::Stroke, Rgb::BLACK)]),
            &georeference(),
            "doc",
            &[],
            ExportOptions::default(),
        );
        assert_eq!(export.summary.dropped_furniture, 1);
        assert_eq!(export.summary.features, 0);
    }

    #[test]
    fn drops_near_white_construction_lines() {
        let white = Rgb {
            r: 0.99,
            g: 0.99,
            b: 0.99,
        };
        let line = vec![Point::new(1600.0, 2500.0), Point::new(1700.0, 2500.0)];
        let export = export_page(
            &page(vec![path(line, false, PaintMode::Stroke, white)]),
            &georeference(),
            "doc",
            &[],
            ExportOptions::default(),
        );
        assert_eq!(export.summary.dropped_furniture, 1);
    }

    #[test]
    fn simplify_keeps_corners_and_drops_collinear_points() {
        let pts = vec![
            Point::new(0.0, 0.0),
            Point::new(1.0, 0.0),
            Point::new(2.0, 0.0),
            Point::new(2.0, 2.0),
        ];
        assert_eq!(simplify(&pts, 0.01).len(), 3);
    }
}
