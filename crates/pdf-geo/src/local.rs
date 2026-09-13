//! Geometry for pages that cannot yet be placed on the map.
//!
//! A drawing with no turning-point table still carries everything except its
//! position: the shapes are exact, and the sheet's scale fixes their size. What
//! is missing is where on Earth to put it and which way it faces — two numbers
//! and a rotation.
//!
//! So the geometry is exported anyway, in a **local metric frame**: metres,
//! y-north-ish, origin at the middle of the drawing. Supplying a real-world
//! reference later is then a similarity fit, which is what [`place`] does. The
//! alternative — discarding the page because its position is unknown — throws
//! away the expensive part to avoid recomputing the cheap one.

use serde::{Deserialize, Serialize};

use super::crs::LonLat;
use super::fit::{Correspondence, Similarity, fit_similarity};
use pdf_geometry::model::{BBox, Point};

/// WGS 84 ellipsoid, for the tangent plane used when placing.
const WGS84_A: f64 = 6_378_137.0;
const WGS84_E2: f64 = 6.694_379_990_141_32e-3;

/// Where the sheet's scale came from, since it decides whether the local frame
/// is in metres or merely in page units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScaleSource {
    /// Printed on the sheet and readable in the text layer.
    Declared,
    /// Read off the sheet by a vision model.
    Model,
    /// Not established. Coordinates are page units, not metres.
    Unknown,
}

/// The frame a not-yet-placed page's geometry is written in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalFrame {
    /// `local_metres` when the scale is known, `page_units` when it is not.
    pub coordinate_system: &'static str,
    /// Sheet scale denominator, e.g. 500 for a 1:500 drawing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale_denominator: Option<f64>,
    pub scale_source: ScaleSource,
    /// Multiplier from PDF user units to the frame's units.
    pub units_per_page_unit: f64,
    /// Page-space point that maps to the frame's origin.
    pub origin_page: [f64; 2],
    /// Extent of the drawing in the frame's units, as `[width, height]`.
    pub extent: [f64; 2],
    /// Always unknown here: nothing on the page says which way is north.
    pub rotation: &'static str,
}

/// PDF user units are 1/72 inch, so one unit on a 1:`d` sheet is this many
/// metres on the ground.
pub fn metres_per_page_unit(scale_denominator: f64) -> f64 {
    scale_denominator * 0.0254 / 72.0
}

impl LocalFrame {
    /// Builds the frame for a page, given whatever is known about its scale.
    pub fn new(drawing: BBox, scale: Option<(f64, ScaleSource)>) -> Self {
        let (units_per_page_unit, denominator, source) = match scale {
            Some((denominator, source)) => {
                (metres_per_page_unit(denominator), Some(denominator), source)
            }
            // Without a scale the shapes are still right, just not in metres.
            None => (1.0, None, ScaleSource::Unknown),
        };
        let centre = drawing.center();
        Self {
            coordinate_system: if source == ScaleSource::Unknown {
                "page_units"
            } else {
                "local_metres"
            },
            scale_denominator: denominator,
            scale_source: source,
            units_per_page_unit,
            origin_page: [centre.x, centre.y],
            extent: [
                drawing.width() * units_per_page_unit,
                drawing.height() * units_per_page_unit,
            ],
            rotation: "unknown",
        }
    }

    /// The page-space to local-frame transform.
    pub fn transform(&self) -> Similarity {
        Similarity {
            a: self.units_per_page_unit,
            b: 0.0,
            tx: -self.origin_page[0] * self.units_per_page_unit,
            ty: -self.origin_page[1] * self.units_per_page_unit,
        }
    }
}

/// One correspondence supplied when placing a local drawing.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Reference {
    /// A point in the local frame, as written in the `_local.geojson`.
    pub local: [f64; 2],
    /// Where that point actually is, as `[longitude, latitude]`.
    pub wgs84: [f64; 2],
}

/// The transform derived from a set of references, and how well it fits.
#[derive(Debug, Clone)]
pub struct Placement {
    /// Local frame to the tangent plane at [`Placement::origin`].
    pub transform: Similarity,
    /// Tangent-plane origin, as `[longitude, latitude]`.
    pub origin: LonLat,
    /// RMS residual over the references, in metres.
    pub rmse_m: f64,
    pub max_residual_m: f64,
    pub references: usize,
    /// Scale the fit implies between the local frame and the ground. Near 1.0
    /// when the local frame was already in metres; a long way off means the
    /// sheet scale was wrong or the references were mismatched.
    pub scale: f64,
    /// Rotation from local north to true north, in degrees CCW.
    pub rotation_degrees: f64,
}

/// Fits a similarity from a local frame onto the ground.
///
/// The fit is done on a tangent plane in metres rather than in degrees, because
/// a degree of longitude is not a degree of latitude and a similarity fitted in
/// degrees would silently absorb that as scale.
pub fn place(references: &[Reference]) -> Option<Placement> {
    if references.len() < 2 {
        return None;
    }
    let origin = centroid(references);
    let (metres_north, metres_east) = local_radii(origin.lat);

    let pairs: Vec<Correspondence> = references
        .iter()
        .map(|r| Correspondence {
            page: Point::new(r.local[0], r.local[1]),
            world: Point::new(
                (r.wgs84[0] - origin.lon).to_radians() * metres_east,
                (r.wgs84[1] - origin.lat).to_radians() * metres_north,
            ),
            row: 0,
            source: super::fit::CandidateSource::TextMarker,
        })
        .collect();

    let transform = fit_similarity(&pairs)?;
    let mut sum_sq = 0.0;
    let mut max: f64 = 0.0;
    for pair in &pairs {
        let d = transform.apply(pair.page).distance(&pair.world);
        sum_sq += d * d;
        max = max.max(d);
    }
    Some(Placement {
        rmse_m: (sum_sq / pairs.len() as f64).sqrt(),
        max_residual_m: max,
        references: references.len(),
        scale: transform.scale(),
        rotation_degrees: transform.rotation_degrees(),
        transform,
        origin,
    })
}

impl Placement {
    /// Maps a local-frame point to WGS 84.
    pub fn to_wgs84(&self, local: Point) -> LonLat {
        let placed = self.transform.apply(local);
        let (metres_north, metres_east) = local_radii(self.origin.lat);
        LonLat {
            lon: self.origin.lon + (placed.x / metres_east).to_degrees(),
            lat: self.origin.lat + (placed.y / metres_north).to_degrees(),
        }
    }
}

fn centroid(references: &[Reference]) -> LonLat {
    let n = references.len() as f64;
    LonLat {
        lon: references.iter().map(|r| r.wgs84[0]).sum::<f64>() / n,
        lat: references.iter().map(|r| r.wgs84[1]).sum::<f64>() / n,
    }
}

/// Metres per degree of longitude and of latitude at `lat` — the same
/// ellipsoid radii a [`Placement`] is built on, so anything that converts
/// ground metres by hand agrees with it.
pub fn metres_per_degree(lat: f64) -> (f64, f64) {
    let (meridian, parallel) = local_radii(lat);
    (parallel.to_radians(), meridian.to_radians())
}

/// Metres per radian of latitude and of longitude at `lat`, from the ellipsoid's
/// meridian and prime-vertical radii. Exact enough that the tangent plane
/// contributes nothing measurable over a drawing a few hundred metres across.
fn local_radii(lat: f64) -> (f64, f64) {
    let sin = lat.to_radians().sin();
    let w = 1.0 - WGS84_E2 * sin * sin;
    let meridian = WGS84_A * (1.0 - WGS84_E2) / (w * w.sqrt());
    let prime_vertical = WGS84_A / w.sqrt();
    (meridian, prime_vertical * lat.to_radians().cos())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_metre_of_page_at_one_to_five_hundred() {
        // One PDF unit is 1/72 inch; at 1:500 that is 0.1764 m on the ground.
        assert!((metres_per_page_unit(500.0) - 0.176_388).abs() < 1e-5);
    }

    fn frame() -> LocalFrame {
        LocalFrame::new(
            BBox {
                min_x: 100.0,
                min_y: 200.0,
                max_x: 300.0,
                max_y: 400.0,
            },
            Some((500.0, ScaleSource::Declared)),
        )
    }

    #[test]
    fn the_local_frame_centres_the_drawing_and_scales_it_to_metres() {
        let f = frame();
        assert_eq!(f.coordinate_system, "local_metres");
        assert_eq!(f.origin_page, [200.0, 300.0]);
        // 200 page units across at 1:500 is 35.3 m.
        assert!((f.extent[0] - 35.277).abs() < 0.01);
        // The centre of the drawing maps to the origin.
        let centre = f.transform().apply(Point::new(200.0, 300.0));
        assert!(centre.x.abs() < 1e-9 && centre.y.abs() < 1e-9);
    }

    #[test]
    fn without_a_scale_the_frame_stays_in_page_units() {
        let f = LocalFrame::new(
            BBox {
                min_x: 0.0,
                min_y: 0.0,
                max_x: 100.0,
                max_y: 50.0,
            },
            None,
        );
        assert_eq!(f.coordinate_system, "page_units");
        assert_eq!(f.scale_source, ScaleSource::Unknown);
        assert_eq!(f.units_per_page_unit, 1.0);
    }

    #[test]
    fn two_references_place_a_drawing_exactly() {
        // A local frame already in metres, rotated 30° and offset.
        let references = [
            Reference {
                local: [0.0, 0.0],
                wgs84: [44.5, 40.1],
            },
            Reference {
                local: [100.0, 0.0],
                wgs84: [44.501103, 40.100501],
            },
        ];
        let placement = place(&references).expect("placement");
        assert!(placement.rmse_m < 0.01, "rmse {}", placement.rmse_m);
        // Round-tripping a reference must land back on it.
        let back = placement.to_wgs84(Point::new(100.0, 0.0));
        assert!((back.lon - 44.501103).abs() < 1e-7);
        assert!((back.lat - 40.100501).abs() < 1e-7);
    }

    #[test]
    fn a_bad_reference_shows_up_as_residual() {
        let references = [
            Reference {
                local: [0.0, 0.0],
                wgs84: [44.5, 40.1],
            },
            Reference {
                local: [100.0, 0.0],
                wgs84: [44.501103, 40.100501],
            },
            // Third point 20 m from where the other two imply.
            Reference {
                local: [50.0, 0.0],
                wgs84: [44.5006, 40.10043],
            },
        ];
        let placement = place(&references).expect("placement");
        assert!(
            placement.rmse_m > 1.0,
            "a mismatched reference must not fit silently"
        );
    }

    #[test]
    fn one_reference_is_not_enough() {
        let references = [Reference {
            local: [0.0, 0.0],
            wgs84: [44.5, 40.1],
        }];
        assert!(place(&references).is_none());
    }

    #[test]
    fn the_fit_reports_scale_and_rotation() {
        // Local frame in metres, rotated 90°: (1,0) local points north.
        let (lat0, lon0) = (40.1, 44.5);
        let (mn, me) = local_radii(lat0);
        let north_100 = [lon0, lat0 + (100.0 / mn).to_degrees()];
        let references = [
            Reference {
                local: [0.0, 0.0],
                wgs84: [lon0, lat0],
            },
            Reference {
                local: [100.0, 0.0],
                wgs84: north_100,
            },
        ];
        let p = place(&references).expect("placement");
        assert!((p.scale - 1.0).abs() < 0.001, "scale {}", p.scale);
        assert!(
            (p.rotation_degrees - 90.0).abs() < 0.5,
            "rotation {}",
            p.rotation_degrees
        );
        let _ = me;
    }
}
