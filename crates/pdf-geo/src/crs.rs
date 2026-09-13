//! Projected-CRS handling for Armenian cadastral drawings.
//!
//! Survey sheets in scope carry turning-point tables in Gauss-Krüger
//! coordinates with a zone-prefixed easting (e.g. northing `4419484.10`,
//! easting `8471288.80` for zone 8) — the form Pulkovo 1942 / EPSG:284xx
//! takes. This module recovers the zone, inverts the Transverse Mercator
//! projection on the Krassovsky 1940 ellipsoid, and writes the geodetic
//! result straight out as WGS 84.
//!
//! **No datum shift is applied, and that is measured, not assumed.** The
//! Yerevan cadastre publishes the same parcels (gis.yerevan.am) in WGS 84, and the
//! surveyor's tables land on them to the centimetre only when the Krassovsky
//! inverse is taken as WGS 84 as is: on dossier 2's parcel the centroid
//! agrees to 0.00 m, on dossier 4's to the same. Applying the EPSG Pulkovo
//! 1942 → WGS 84 Helmert (EPSG:1267) instead put both 105.6 m east and
//! 12.3 m north of the city's parcel — the shift the cadastre never applied.
//! The registry is the ground truth a placement is checked against, so the
//! tables are read the way the registry reads them.

use serde::{Deserialize, Serialize};

/// Krassovsky 1940, the ellipsoid underlying Pulkovo 1942.
const KRASSOVSKY_A: f64 = 6378245.0;
const KRASSOVSKY_F: f64 = 1.0 / 298.3;

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

    /// The 6° Gauss-Krüger zone a longitude falls in (Yerevan, 44.5°E, is zone 8).
    pub fn zone_for_longitude(lon: f64) -> u8 {
        ((lon / 6.0).floor() as i64 + 1).clamp(1, 60) as u8
    }

    /// Converts a WGS 84 position into this CRS — the same reading as
    /// [`to_wgs84`](Self::to_wgs84), inverted, so an external polygon can
    /// serve as a control table beside the sheets' own.
    pub fn from_wgs84(&self, p: LonLat) -> Option<Projected> {
        match *self {
            ProjectedCrs::Unknown => None,
            ProjectedCrs::PulkovoGaussKruger { zone } => Some(Self::gauss_kruger_forward(zone, p)),
        }
    }

    /// Projects a WGS 84 position into Gauss-Krüger `zone` with the
    /// zone-prefixed easting; every position projects, so no `Option`.
    pub fn gauss_kruger_forward(zone: u8, p: LonLat) -> Projected {
        let central_meridian = f64::from(zone) * 6.0 - 3.0;
        let (x, y) = tmerc_forward(
            p.lat.to_radians(),
            p.lon.to_radians(),
            central_meridian.to_radians(),
            KRASSOVSKY_A,
            KRASSOVSKY_F,
        );
        Projected {
            easting: x + f64::from(zone) * 1000000.0 + 500000.0,
            northing: y,
        }
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

/// Forward Transverse Mercator (scale factor 1, false northing 0), the
/// series inverse to [`tmerc_inverse`]; sub-millimetre within a zone.
fn tmerc_forward(lat: f64, lon: f64, lon0: f64, a: f64, f: f64) -> (f64, f64) {
    let e2 = f * (2.0 - f);
    let ep2 = e2 / (1.0 - e2);
    let (sin, cos) = lat.sin_cos();
    let n = a / (1.0 - e2 * sin * sin).sqrt();
    let t = (sin / cos).powi(2);
    let c = ep2 * cos * cos;
    let aa = (lon - lon0) * cos;
    let m = a
        * ((1.0 - e2 / 4.0 - 3.0 * e2 * e2 / 64.0 - 5.0 * e2.powi(3) / 256.0) * lat
            - (3.0 * e2 / 8.0 + 3.0 * e2 * e2 / 32.0 + 45.0 * e2.powi(3) / 1024.0)
                * (2.0 * lat).sin()
            + (15.0 * e2 * e2 / 256.0 + 45.0 * e2.powi(3) / 1024.0) * (4.0 * lat).sin()
            - (35.0 * e2.powi(3) / 3072.0) * (6.0 * lat).sin());
    let x = n
        * (aa
            + (1.0 - t + c) * aa.powi(3) / 6.0
            + (5.0 - 18.0 * t + t * t + 72.0 * c - 58.0 * ep2) * aa.powi(5) / 120.0);
    let y = m + n
        * (sin / cos)
        * (aa * aa / 2.0
            + (5.0 - t + 9.0 * c + 4.0 * c * c) * aa.powi(4) / 24.0
            + (61.0 - 58.0 * t + t * t + 600.0 * c - 330.0 * ep2) * aa.powi(6) / 720.0);
    (x, y)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A WGS 84 point goes into the projection and comes back to the
    /// millimetre — the property an external parcel polygon relies on.
    #[test]
    fn forward_and_inverse_round_trip_to_the_millimetre() {
        let crs = ProjectedCrs::PulkovoGaussKruger { zone: 8 };
        assert_eq!(ProjectedCrs::zone_for_longitude(44.5), 8);
        let original = Projected {
            northing: 4419484.1041,
            easting: 8471288.7976,
        };
        let there = crs.to_wgs84(original).unwrap();
        let back = crs.from_wgs84(there).unwrap();
        assert!(
            (back.easting - original.easting).abs() < 1e-3,
            "{}",
            back.easting
        );
        assert!(
            (back.northing - original.northing).abs() < 1e-3,
            "{}",
            back.northing
        );
    }

    /// Reference value produced by PostGIS/PROJ for the Krassovsky
    /// Transverse Mercator inverse taken as WGS 84
    /// (`+proj=tmerc +lon_0=45 +k=1 +x_0=8500000 +ellps=krass`), which is
    /// the reading that lands on the Yerevan cadastre's own parcels. The
    /// EPSG:28408 → EPSG:4326 value (44.6630029, 39.9082250) is 106 m away
    /// and is exactly what must NOT come out.
    #[test]
    fn matches_the_cadastre_reading_for_a_yerevan_control_point() {
        let crs = ProjectedCrs::PulkovoGaussKruger { zone: 8 };
        let p = crs
            .to_wgs84(Projected {
                northing: 4419484.1041,
                easting: 8471288.7976,
            })
            .expect("known CRS");
        assert!((p.lat - 39.9083426).abs() < 1e-6, "lat was {}", p.lat);
        assert!((p.lon - 44.6642335).abs() < 1e-6, "lon was {}", p.lon);
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
