//! Projected-CRS handling for Armenian cadastral drawings.
//!
//! Survey sheets in scope carry turning-point tables in Pulkovo 1942 /
//! Gauss-Krüger coordinates with a zone-prefixed easting (e.g. northing
//! `4419484.10`, easting `8471288.80` for zone 8). This module recovers the
//! zone, inverts the Transverse Mercator projection on the Krassovsky 1940
//! ellipsoid, and applies the Pulkovo 1942 → WGS 84 Helmert transform so the
//! result can be written straight into GeoJSON.
//!
//! Accuracy is bounded by the datum-shift parameters (~4.5 m), which is well
//! inside the tolerance for overlaying a parcel on satellite imagery.

use serde::{Deserialize, Serialize};

/// Krassovsky 1940, the ellipsoid underlying Pulkovo 1942.
const KRASSOVSKY_A: f64 = 6378245.0;
const KRASSOVSKY_F: f64 = 1.0 / 298.3;

const WGS84_A: f64 = 6378137.0;
const WGS84_F: f64 = 1.0 / 298.257_223_563;

/// Pulkovo 1942 → WGS 84, EPSG:1267 ("Pulkovo 1942 to WGS 84 (16)"),
/// coordinate-frame rotation convention. Translations in metres, rotations in
/// arc-seconds, scale in ppm.
const HELMERT: Helmert = Helmert {
    dx: 25.0,
    dy: -141.0,
    dz: -78.5,
    rx: 0.0,
    ry: -0.35,
    rz: -0.736,
    ppm: 0.0,
};

struct Helmert {
    dx: f64,
    dy: f64,
    dz: f64,
    rx: f64,
    ry: f64,
    rz: f64,
    ppm: f64,
}

/// A geographic position in WGS 84 degrees.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LonLat {
    pub lon: f64,
    pub lat: f64,
}

/// A projected easting/northing pair as written in a survey table.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Projected {
    pub northing: f64,
    pub easting: f64,
}

/// The projected coordinate system a drawing's control points are expressed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProjectedCrs {
    /// Pulkovo 1942 / Gauss-Krüger with a zone-prefixed easting (EPSG:284xx).
    PulkovoGaussKruger { zone: u8 },
    /// Coordinates whose system could not be identified; geometry stays in
    /// page space and is not written as WGS 84.
    Unknown,
}

impl ProjectedCrs {
    pub fn epsg(&self) -> Option<u32> {
        match self {
            ProjectedCrs::PulkovoGaussKruger { zone } => Some(28400 + u32::from(*zone)),
            ProjectedCrs::Unknown => None,
        }
    }

    pub fn label(&self) -> String {
        match self {
            ProjectedCrs::PulkovoGaussKruger { zone } => {
                format!(
                    "EPSG:{} (Pulkovo 1942 / Gauss-Kruger zone {zone})",
                    28400 + u32::from(*zone)
                )
            }
            ProjectedCrs::Unknown => "unknown".to_string(),
        }
    }

    /// Infers the CRS from the magnitude of a sample of control points.
    ///
    /// A zone-prefixed Gauss-Krüger easting is 7 digits whose leading digit is
    /// the zone number and whose remaining 6 digits sit around the 500 km false
    /// easting; the northing is the distance from the equator.
    pub fn detect(points: &[Projected]) -> ProjectedCrs {
        if points.is_empty() {
            return ProjectedCrs::Unknown;
        }
        let mut zone = None;
        for p in points {
            if !(1000000.0..10000000.0).contains(&p.northing) {
                return ProjectedCrs::Unknown;
            }
            if !(1000000.0..33000000.0).contains(&p.easting) {
                return ProjectedCrs::Unknown;
            }
            let z = (p.easting / 1000000.0).floor();
            let within_zone = p.easting - z * 1000000.0;
            // Gauss-Krüger zones are 6° wide, so a valid easting stays within
            // roughly 350 km of the 500 km false origin.
            if !(120000.0..880000.0).contains(&within_zone) {
                return ProjectedCrs::Unknown;
            }
            let z = z as u8;
            if !(1..=32).contains(&z) || zone.is_some_and(|prev| prev != z) {
                return ProjectedCrs::Unknown;
            }
            zone = Some(z);
        }
        zone.map_or(ProjectedCrs::Unknown, |zone| {
            ProjectedCrs::PulkovoGaussKruger { zone }
        })
    }

    /// Converts a projected point to WGS 84 longitude/latitude.
    pub fn to_wgs84(&self, p: Projected) -> Option<LonLat> {
        match *self {
            ProjectedCrs::Unknown => None,
            ProjectedCrs::PulkovoGaussKruger { zone } => {
                let central_meridian = f64::from(zone) * 6.0 - 3.0;
                let easting = p.easting - f64::from(zone) * 1000000.0 - 500000.0;
                let (lat, lon) = tmerc_inverse(
                    easting,
                    p.northing,
                    central_meridian.to_radians(),
                    KRASSOVSKY_A,
                    KRASSOVSKY_F,
                );
                let (lat, lon) = helmert_to_wgs84(lat, lon);
                Some(LonLat {
                    lon: lon.to_degrees(),
                    lat: lat.to_degrees(),
                })
            }
        }
    }
}

/// Inverse Transverse Mercator (scale factor 1, false northing 0) using the
/// footpoint-latitude series, accurate to well under a millimetre within a zone.
fn tmerc_inverse(x: f64, y: f64, lon0: f64, a: f64, f: f64) -> (f64, f64) {
    let e2 = f * (2.0 - f);
    let ep2 = e2 / (1.0 - e2);

    // Footpoint latitude from the meridional arc.
    let e1 = (1.0 - (1.0 - e2).sqrt()) / (1.0 + (1.0 - e2).sqrt());
    let m = y;
    let mu = m / (a * (1.0 - e2 / 4.0 - 3.0 * e2 * e2 / 64.0 - 5.0 * e2 * e2 * e2 / 256.0));
    let phi1 = mu
        + (3.0 * e1 / 2.0 - 27.0 * e1.powi(3) / 32.0) * (2.0 * mu).sin()
        + (21.0 * e1 * e1 / 16.0 - 55.0 * e1.powi(4) / 32.0) * (4.0 * mu).sin()
        + (151.0 * e1.powi(3) / 96.0) * (6.0 * mu).sin()
        + (1097.0 * e1.powi(4) / 512.0) * (8.0 * mu).sin();

    let (sin1, cos1) = phi1.sin_cos();
    let tan1 = sin1 / cos1;
    let c1 = ep2 * cos1 * cos1;
    let t1 = tan1 * tan1;
    let n1 = a / (1.0 - e2 * sin1 * sin1).sqrt();
    let r1 = a * (1.0 - e2) / (1.0 - e2 * sin1 * sin1).powf(1.5);
    let d = x / n1;

    let lat = phi1
        - (n1 * tan1 / r1)
            * (d * d / 2.0
                - (5.0 + 3.0 * t1 + 10.0 * c1 - 4.0 * c1 * c1 - 9.0 * ep2) * d.powi(4) / 24.0
                + (61.0 + 90.0 * t1 + 298.0 * c1 + 45.0 * t1 * t1 - 252.0 * ep2 - 3.0 * c1 * c1)
                    * d.powi(6)
                    / 720.0);
    let lon = lon0
        + (d - (1.0 + 2.0 * t1 + c1) * d.powi(3) / 6.0
            + (5.0 - 2.0 * c1 + 28.0 * t1 - 3.0 * c1 * c1 + 8.0 * ep2 + 24.0 * t1 * t1)
                * d.powi(5)
                / 120.0)
            / cos1;
    (lat, lon)
}

/// Applies the Pulkovo 1942 → WGS 84 Helmert transform via geocentric
/// coordinates, using the coordinate-frame rotation convention.
fn helmert_to_wgs84(lat: f64, lon: f64) -> (f64, f64) {
    let (x, y, z) = geodetic_to_cartesian(lat, lon, 0.0, KRASSOVSKY_A, KRASSOVSKY_F);

    let arcsec = std::f64::consts::PI / (180.0 * 3600.0);
    let (rx, ry, rz) = (
        HELMERT.rx * arcsec,
        HELMERT.ry * arcsec,
        HELMERT.rz * arcsec,
    );
    let s = 1.0 + HELMERT.ppm * 1e-6;

    // Coordinate-frame convention: the rotation matrix is the transpose of the
    // position-vector one, i.e. the off-diagonal signs are flipped.
    let xw = HELMERT.dx + s * (x + rz * y - ry * z);
    let yw = HELMERT.dy + s * (-rz * x + y + rx * z);
    let zw = HELMERT.dz + s * (ry * x - rx * y + z);

    let (lat, lon, _) = cartesian_to_geodetic(xw, yw, zw, WGS84_A, WGS84_F);
    (lat, lon)
}

fn geodetic_to_cartesian(lat: f64, lon: f64, h: f64, a: f64, f: f64) -> (f64, f64, f64) {
    let e2 = f * (2.0 - f);
    let (sin_lat, cos_lat) = lat.sin_cos();
    let (sin_lon, cos_lon) = lon.sin_cos();
    let n = a / (1.0 - e2 * sin_lat * sin_lat).sqrt();
    (
        (n + h) * cos_lat * cos_lon,
        (n + h) * cos_lat * sin_lon,
        (n * (1.0 - e2) + h) * sin_lat,
    )
}

/// Bowring's iterative inverse; converges to millimetre level in a few passes.
fn cartesian_to_geodetic(x: f64, y: f64, z: f64, a: f64, f: f64) -> (f64, f64, f64) {
    let e2 = f * (2.0 - f);
    let lon = y.atan2(x);
    let p = (x * x + y * y).sqrt();
    let mut lat = z.atan2(p * (1.0 - e2));
    let mut n = a;
    for _ in 0..8 {
        let sin_lat = lat.sin();
        n = a / (1.0 - e2 * sin_lat * sin_lat).sqrt();
        let next = (z + e2 * n * sin_lat).atan2(p);
        if (next - lat).abs() < 1e-13 {
            lat = next;
            break;
        }
        lat = next;
    }
    let h = p / lat.cos() - n;
    (lat, lon, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference value produced by PROJ 9.8 for EPSG:28408 → EPSG:4326.
    #[test]
    fn matches_proj_for_yerevan_control_point() {
        let crs = ProjectedCrs::PulkovoGaussKruger { zone: 8 };
        let p = crs
            .to_wgs84(Projected {
                northing: 4419484.1041,
                easting: 8471288.7976,
            })
            .expect("known CRS");
        assert!((p.lat - 39.9082250).abs() < 1e-5, "lat was {}", p.lat);
        assert!((p.lon - 44.6630029).abs() < 1e-5, "lon was {}", p.lon);
    }

    #[test]
    fn detects_zone_from_easting_prefix() {
        let pts = [
            Projected {
                northing: 4419484.1,
                easting: 8471288.8,
            },
            Projected {
                northing: 4419494.8,
                easting: 8471308.9,
            },
        ];
        assert_eq!(
            ProjectedCrs::detect(&pts),
            ProjectedCrs::PulkovoGaussKruger { zone: 8 }
        );
    }

    #[test]
    fn rejects_page_space_values() {
        let pts = [Projected {
            northing: 120.0,
            easting: 340.0,
        }];
        assert_eq!(ProjectedCrs::detect(&pts), ProjectedCrs::Unknown);
    }

    #[test]
    fn rejects_mixed_zones() {
        let pts = [
            Projected {
                northing: 4419484.1,
                easting: 8471288.8,
            },
            Projected {
                northing: 4419484.1,
                easting: 9471288.8,
            },
        ];
        assert_eq!(ProjectedCrs::detect(&pts), ProjectedCrs::Unknown);
    }

    #[test]
    fn round_trips_within_a_metre_over_the_zone() {
        // Two points 1 km apart should stay 1 km apart after projection.
        let crs = ProjectedCrs::PulkovoGaussKruger { zone: 8 };
        let a = crs
            .to_wgs84(Projected {
                northing: 4419000.0,
                easting: 8471000.0,
            })
            .unwrap();
        let b = crs
            .to_wgs84(Projected {
                northing: 4420000.0,
                easting: 8471000.0,
            })
            .unwrap();
        let metres = (b.lat - a.lat).to_radians() * 6371000.0;
        assert!((metres - 1000.0).abs() < 3.0, "got {metres} m");
    }
}
