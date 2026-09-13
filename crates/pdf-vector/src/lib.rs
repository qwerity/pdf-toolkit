//! From a PDF's content streams to GeoJSON on a map.
//!
//! One pass per page, in three steps that each answer a different question:
//!
//! 1. **What is on the page?** `pdf-geometry` interprets the content stream and
//!    recovers the real vector paths and text runs a CAD exporter wrote.
//! 2. **Is it a drawing worth placing?** `pdf-classify` decides from cheap
//!    structural features — line density, layer count, cadastral codes.
//! 3. **Where on Earth is it?** `pdf-geo` fits a transform from page space to
//!    the ground and writes the geometry out as GeoJSON.
//!
//! # Two ways a page gets placed
//!
//! A survey sheet usually prints a table of boundary turning-point coordinates,
//! and that is enough on its own: the table is read from the text layer, the
//! same polygon is found in the drawing, and a similarity transform is fitted
//! between the two. Residuals come out in **millimetres**, which is the check
//! that the answer is right — a fit that does not agree with the drawing is
//! reported as a failure rather than guessed at.
//!
//! When a sheet prints no such table, nothing on the page says where it is. The
//! caller can then supply [`Reference`]s — a point in the drawing and where that
//! point actually is — and two of them settle position, rotation and scale
//! together.
//!
//! # A page is never silently dropped
//!
//! Every selected page yields a [`PageVector`] carrying GeoJSON, whether or not
//! it could be placed. An unplaced page's geometry is written in its own frame
//! and says so in `crs`: `local_metres` when the sheet's scale was readable,
//! `page_units` when it was not. The shapes are correct in both; only their
//! position on Earth is missing, which is exactly what [`Reference`]s supply.

use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;

use pdf_classify::{PageFeatures, heuristic_label};
use pdf_geo::ControlTable;
use pdf_geo::control::{declared_scale_denominators, table_candidates_from_text_layer};
use pdf_geo::fit::FitConstraints;
use pdf_geo::geojson::{ExportOptions, export_local, export_page, export_survey_parcel};
use pdf_geo::local::{LocalFrame, Reference, ScaleSource, place};
use pdf_geo::pipeline::georeference_page;
use pdf_geo::pipeline::{
    GeoreferenceFailure, GeoreferenceResult, enforce_document_crs, propagate_document_tables,
    propagate_external_tables,
};
use pdf_geometry::PdfDocument;
use pdf_geometry::model::{BBox, PageContent};

pub mod report;

pub use report::{DocumentVector, GeoreferenceReport, PageVector, Placement};

/// How to run the extraction.
#[derive(Debug, Clone, Default)]
pub struct VectorOptions {
    /// Real-world references for pages that carry no coordinate table.
    ///
    /// Two or more place a drawing exported in its own frame. They apply to
    /// every page that could not be placed from its own text layer, so passing
    /// them alongside a multi-page selection only affects the pages that need
    /// them.
    pub references: Vec<Reference>,
    /// Parcel polygons supplied by the caller — the cadastre's statement of
    /// the boundary — for sheets that draw the parcel but print no table.
    /// Matched by shape under the same consensus and credibility gate as a
    /// table lent by a sibling sheet; see [`read_parcel_file`].
    pub parcels: Vec<ControlTable>,
    pub export: ExportOptions,
}

/// The provenance a `--parcel` polygon carries into the GeoJSON it places
/// (`coordinate_source: "external:parcel"`).
const PARCEL_ORIGIN: &str = "parcel";

/// Reads WGS 84 parcel polygons from a GeoJSON file: a `Polygon`,
/// `MultiPolygon`, `Feature` or `FeatureCollection`; every outer ring becomes
/// one control table. Interior rings are ignored — a hole is not a boundary a
/// sheet draws.
pub fn read_parcel_file(path: &Path) -> Result<Vec<ControlTable>> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read parcel file '{}'", path.display()))?;
    let value: geojson::GeoJson = raw
        .parse()
        .with_context(|| format!("'{}' is not GeoJSON", path.display()))?;
    let mut rings: Vec<Vec<pdf_geo::LonLat>> = Vec::new();
    fn collect(geometry: &geojson::Geometry, rings: &mut Vec<Vec<pdf_geo::LonLat>>) {
        use geojson::GeometryValue as V;
        let ring_of = |ring: &[geojson::Position]| -> Vec<pdf_geo::LonLat> {
            ring.iter()
                .filter(|p| p.len() >= 2)
                .map(|p| pdf_geo::LonLat {
                    lon: p[0],
                    lat: p[1],
                })
                .collect()
        };
        match &geometry.value {
            V::Polygon { coordinates } => {
                if let Some(outer) = coordinates.first() {
                    rings.push(ring_of(outer));
                }
            }
            V::MultiPolygon { coordinates } => {
                for polygon in coordinates {
                    if let Some(outer) = polygon.first() {
                        rings.push(ring_of(outer));
                    }
                }
            }
            V::GeometryCollection { geometries } => {
                geometries.iter().for_each(|g| collect(g, rings));
            }
            _ => {}
        }
    }
    match &value {
        geojson::GeoJson::Geometry(g) => collect(g, &mut rings),
        geojson::GeoJson::Feature(f) => {
            if let Some(g) = &f.geometry {
                collect(g, &mut rings);
            }
        }
        geojson::GeoJson::FeatureCollection(fc) => {
            for f in &fc.features {
                if let Some(g) = &f.geometry {
                    collect(g, &mut rings);
                }
            }
        }
    }
    let tables: Vec<ControlTable> = rings
        .iter()
        .filter_map(|ring| ControlTable::from_wgs84_ring(ring, PARCEL_ORIGIN))
        .collect();
    if tables.is_empty() {
        anyhow::bail!(
            "'{}' holds no polygon with three or more vertices",
            path.display()
        );
    }
    Ok(tables)
}

/// Where a page's placement came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlacementSource {
    /// A turning-point table printed on the sheet and readable as text.
    TextLayer,
    /// A turning-point table printed on another sheet of the same document,
    /// matched to this sheet's drawn parcel outline by shape.
    DocumentTable,
    /// A parcel polygon supplied by the caller (`--parcel`), matched to this
    /// sheet's drawn parcel outline by shape.
    ParcelPolygon,
    /// References supplied by the caller.
    References,
}

/// Runs the pipeline over selected pages of one document.
///
/// `pages` is one-based; an empty slice means every page.
pub fn extract_document(
    path: &Path,
    pages: &[usize],
    options: &VectorOptions,
) -> Result<DocumentVector> {
    let document =
        PdfDocument::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let document_id = document.id();
    let total = document.page_count();

    let wanted: Vec<usize> = if pages.is_empty() {
        (1..=total).collect()
    } else {
        pages
            .iter()
            .copied()
            .filter(|n| *n >= 1 && *n <= total)
            .collect()
    };

    let mut contents = Vec::with_capacity(wanted.len());
    let mut warnings = Vec::new();
    for page_number in wanted {
        match document.page_content(page_number) {
            Ok(content) => contents.push(content),
            // One unreadable page does not end the run; the rest still export.
            Err(error) => warnings.push(format!("page {page_number}: {error:#}")),
        }
    }

    // Fit every page on its own first, then let the document as a whole have a
    // say. A page that fitted to centimetres has established which coordinate
    // system this dossier is drawn in, and that is worth more than any single
    // page's tolerance: its siblings are re-tried against that CRS with a
    // looser bound, which places sheets whose corners are drawn half a
    // line-width off. Every other guard still applies.
    let tables: Vec<Vec<ControlTable>> = contents
        .iter()
        .map(table_candidates_from_text_layer)
        .collect();
    let constraints = FitConstraints::default();
    let mut fits: Vec<Result<GeoreferenceResult, GeoreferenceFailure>> = contents
        .iter()
        .zip(&tables)
        .map(|(page, candidates)| georeference_page(page, candidates, constraints))
        .collect();
    enforce_document_crs(&contents, &tables, &mut fits, constraints);
    // The survey sheet's coordinates are a statement about the whole
    // dossier: sheets that draw the same parcel but print no table of
    // their own are placed by matching that shape.
    propagate_document_tables(&contents, &mut fits, constraints);
    // Last, the caller's own statement of the parcel — after the document's
    // sheets have had their say, so a sheet that could be placed from the
    // dossier's own survey still is, and the external polygon only fills in.
    propagate_external_tables(&contents, &mut fits, constraints, &options.parcels);

    let pages = contents
        .iter()
        .zip(&tables)
        .zip(fits)
        .map(|((page, candidates), fit)| export(page, candidates, fit, &document_id, options))
        .collect();

    Ok(DocumentVector {
        document: document_id,
        source: path.display().to_string(),
        page_count: total,
        pages,
        warnings,
    })
}

/// Classifies one page and exports whatever geometry it holds.
///
/// Georeferences the page on its own. [`extract_document`] is preferred where a
/// whole dossier is available, because a page that cannot be placed by itself
/// often can be once a sibling has established the document's CRS.
pub fn extract_page(page: &PageContent, document_id: &str, options: &VectorOptions) -> PageVector {
    let tables = table_candidates_from_text_layer(page);
    let fit = georeference_page(page, &tables, FitConstraints::default());
    export(page, &tables, fit, document_id, options)
}

/// Turns one page's fit — or its failure — into the reported result.
fn export(
    page: &PageContent,
    tables: &[ControlTable],
    fit: Result<GeoreferenceResult, GeoreferenceFailure>,
    document_id: &str,
    options: &VectorOptions,
) -> PageVector {
    let features = PageFeatures::compute(page);
    let verdict = heuristic_label(&features);
    let classification = report::classification(&verdict, &features);
    let exclude = exclusion_boxes(page, tables);

    match fit {
        // The sheet's own table wins over supplied references: it is the
        // draughtsman's statement of where the parcel is, and it can be checked
        // against the drawing, which a reference pair cannot.
        Ok(result) => {
            let mut export = export_page(
                page,
                &result.georeference,
                document_id,
                &exclude,
                options.export,
            );
            // The subject parcel, from the survey's own turning points rather
            // than the line work — the one feature a caller can find without
            // guessing which drawn ring is the boundary (`kind:
            // "survey_parcel"`). A lent table is still the parcel's survey.
            if let Some(parcel) =
                export_survey_parcel(&result.table, document_id, page.page_number, true)
            {
                export.summary.features += parcel.features.len();
                export.summary.polygons += parcel.features.len();
                export.collection.features.extend(parcel.features);
            }
            PageVector {
                page_number: page.page_number,
                classification,
                georeference: Some(GeoreferenceReport::placed(
                    if result.table.is_external() {
                        PlacementSource::ParcelPolygon
                    } else if result.borrowed_table {
                        PlacementSource::DocumentTable
                    } else {
                        PlacementSource::TextLayer
                    },
                    &result,
                )),
                crs: "EPSG:4326".into(),
                geojson: export.collection,
                summary: export.summary,
            }
        }
        Err(failure) => {
            let frame = local_frame(page);
            let mut export = export_local(page, &frame, document_id, &exclude, options.export);

            // The page could not place itself. Supplied references can still
            // put it on the ground: the geometry is exported in the page's own
            // frame first and then moved, because the local frame is what the
            // references are stated in, and composing in that order is what
            // makes a reference mean what the caller thinks it means.
            match place(&options.references) {
                Some(placement) => {
                    let mut bounds = BBox::EMPTY;
                    for feature in &mut export.collection.features {
                        if let Some(geometry) = feature.geometry.as_mut() {
                            report::reproject(&mut geometry.value, &placement, &mut bounds);
                        }
                    }
                    PageVector {
                        page_number: page.page_number,
                        classification,
                        georeference: Some(GeoreferenceReport::from_references(
                            &placement, &bounds,
                        )),
                        crs: "EPSG:4326".into(),
                        geojson: export.collection,
                        summary: export.summary,
                    }
                }
                None => PageVector {
                    page_number: page.page_number,
                    classification,
                    georeference: Some(GeoreferenceReport::unplaced(failure)),
                    crs: frame.coordinate_system.to_string(),
                    geojson: export.collection,
                    summary: export.summary,
                },
            }
        }
    }
}

/// The frame an unplaced page's geometry is written in — metres when the sheet
/// prints a scale, raw page units when it does not.
fn local_frame(page: &PageContent) -> LocalFrame {
    let scale = declared_scale_denominators(page)
        .first()
        .map(|d| (*d, ScaleSource::Declared));
    LocalFrame::new(page.vector_bbox(), scale)
}

/// Regions to keep out of the export: a coordinate table is lettering, not
/// terrain, and drawing it would put the sheet's own text on the map.
fn exclusion_boxes(page: &PageContent, tables: &[pdf_geo::ControlTable]) -> Vec<BBox> {
    tables
        .iter()
        .filter_map(|table| pdf_geo::control::table_exclusion_bbox(page, table))
        .collect()
}

/// Re-exported so a caller can build references without depending on `pdf-geo`.
pub use pdf_geo::local::Reference as GeoReference;

/// Parses `local_x,local_y,longitude,latitude`.
///
/// The local pair is a point in the drawing's own frame — the coordinates an
/// unplaced page's GeoJSON is written in — and the second pair is where that
/// point actually is.
pub fn parse_reference(raw: &str) -> std::result::Result<Reference, String> {
    let values: Vec<f64> = raw
        .split(',')
        .map(|v| v.trim().parse::<f64>())
        .collect::<std::result::Result<_, _>>()
        .map_err(|_| format!("`{raw}` is not four comma-separated numbers"))?;

    let [lx, ly, lon, lat] = values[..] else {
        return Err(format!(
            "`{raw}` should be local_x,local_y,longitude,latitude ({} value(s) given)",
            values.len()
        ));
    };
    if !(-180.0..=180.0).contains(&lon) || !(-90.0..=90.0).contains(&lat) {
        return Err(format!(
            "`{raw}` has longitude/latitude outside the world: {lon}, {lat}"
        ));
    }
    Ok(Reference {
        local: [lx, ly],
        wgs84: [lon, lat],
    })
}

/// Rounds to `digits`, used everywhere a coordinate reaches the output.
pub(crate) fn round(v: f64, digits: i32) -> f64 {
    let f = 10f64.powi(digits);
    (v * f).round() / f
}

/// Convenience for callers that want the centre of a placed page.
pub(crate) fn centre(bounds: &BBox) -> [f64; 2] {
    let c = bounds.center();
    [round(c.x, 7), round(c.y, 7)]
}

/// File name one page's GeoJSON is written under, shared by `vector
/// --output` and `dossier --output` so the same page gets the same name
/// whichever command exported it.
pub fn page_geojson_file_name(document: &str, page_number: usize) -> String {
    format!("{document}_p{page_number:02}.geojson")
}

/// Writes a page's `FeatureCollection` as pretty-printed GeoJSON.
pub fn write_page_geojson(
    path: &std::path::Path,
    geojson: &geojson::FeatureCollection,
) -> Result<()> {
    write_pretty_json(path, geojson)
}

/// Writes any serializable value as pretty-printed JSON, streaming rather
/// than buffering the text — a dense CAD sheet's collection runs to tens of
/// megabytes. The one JSON writer every command's `--output` file goes
/// through.
pub fn write_pretty_json<T: Serialize>(path: &std::path::Path, value: &T) -> Result<()> {
    let file = std::fs::File::create(path)
        .with_context(|| format!("cannot write '{}'", path.display()))?;
    let mut writer = std::io::BufWriter::new(file);
    serde_json::to_writer_pretty(&mut writer, value)
        .with_context(|| format!("failed to serialize '{}'", path.display()))?;
    // An owned BufWriter flushes in Drop, where an ENOSPC on the final
    // chunk would vanish and a truncated file would be reported written.
    std::io::Write::flush(&mut writer)
        .with_context(|| format!("failed to flush '{}'", path.display()))
}
