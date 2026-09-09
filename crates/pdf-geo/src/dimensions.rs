//! Cross-checking the distances printed on a survey sheet against the
//! coordinates printed beside them.
//!
//! A turning-point table states each boundary segment twice over: once as the
//! coordinates of its two endpoints, and once as a side length in metres. Those
//! are two independent statements by the draughtsman, so comparing them tests
//! the numbers this tool actually read — a transposed digit, a swapped column or
//! a row read out of order all move a side length, and none of them can be seen
//! in the georeferencing residual, which is fitted to the same coordinates it is
//! scoring.
//!
//! This is the check that answers "the sheet says this side is 18.5 m — is it?"

use serde::{Deserialize, Serialize};

use super::control::ControlTable;

/// Where the printed side lengths came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DimensionSource {
    /// Read from the page's text layer.
    TextLayer,
}

/// One printed side length checked against the coordinates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SideCheck {
    /// Point numbers this side runs between, as printed.
    pub from: String,
    pub to: String,
    /// The length printed on the sheet, in metres.
    pub printed_m: f64,
    /// The length implied by the two points' coordinates, in metres.
    pub computed_m: f64,
    pub error_m: f64,
    pub agrees: bool,
}

/// Summary of the comparison, carried in the report and the GeoJSON metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DimensionReport {
    pub source: DimensionSource,
    pub sides_printed: usize,
    pub sides_agreeing: usize,
    /// Largest disagreement, in metres.
    pub max_error_m: f64,
    pub mean_error_m: f64,
    /// True when every printed side length matches its coordinates.
    pub all_agree: bool,
    pub sides: Vec<SideCheck>,
}

/// How far a printed side length may sit from the computed one and still agree.
///
/// Sheets print two decimals, so rounding alone allows half a centimetre; the
/// proportional term covers a draughtsman rounding a long side more loosely.
fn tolerance_for(printed_m: f64) -> f64 {
    (printed_m * 0.005).max(0.02)
}

/// Compares every side length the sheet prints against the distance implied by
/// the coordinates of its endpoints.
///
/// Returns `None` when the sheet prints no side lengths, which is common: many
/// CAD exports convert them to outlines, leaving nothing to read.
pub fn check(table: &ControlTable) -> Option<DimensionReport> {
    let mut sides = Vec::new();
    for (index, printed) in table.printed_distances.iter().enumerate() {
        let Some(printed_m) = *printed else { continue };
        let (Some(from), Some(to)) = (table.rows.get(index), table.rows.get(index + 1)) else {
            continue;
        };
        let computed_m = (to.projected.easting - from.projected.easting)
            .hypot(to.projected.northing - from.projected.northing);
        let error_m = (computed_m - printed_m).abs();
        sides.push(SideCheck {
            from: from.label.clone(),
            to: to.label.clone(),
            printed_m,
            computed_m: (computed_m * 1e4).round() / 1e4,
            error_m: (error_m * 1e4).round() / 1e4,
            agrees: error_m <= tolerance_for(printed_m),
        });
    }
    if sides.is_empty() {
        return None;
    }

    let agreeing = sides.iter().filter(|s| s.agrees).count();
    let max_error = sides.iter().map(|s| s.error_m).fold(0.0, f64::max);
    let mean_error = sides.iter().map(|s| s.error_m).sum::<f64>() / sides.len() as f64;
    Some(DimensionReport {
        source: DimensionSource::TextLayer,
        sides_printed: sides.len(),
        sides_agreeing: agreeing,
        max_error_m: (max_error * 1e4).round() / 1e4,
        mean_error_m: (mean_error * 1e4).round() / 1e4,
        all_agree: agreeing == sides.len(),
        sides,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::{ControlRow, TableSource};
    use crate::crs::Projected;

    /// Four points on a 3-4-5 grid, so the side lengths are exact by hand.
    fn table(printed: Vec<Option<f64>>) -> ControlTable {
        let rows = [(0.0, 0.0), (3.0, 0.0), (3.0, 4.0), (0.0, 4.0)]
            .iter()
            .enumerate()
            .map(|(i, (e, n))| ControlRow {
                label: (i + 1).to_string(),
                projected: Projected {
                    northing: 4419000.0 + n,
                    easting: 8471000.0 + e,
                },
            })
            .collect();
        ControlTable::from_rows(rows, TableSource::TextLayer, None).with_printed_distances(printed)
    }

    #[test]
    fn agrees_when_the_sheet_matches_its_coordinates() {
        let report = check(&table(vec![Some(3.0), Some(4.0), Some(3.0)])).expect("report");
        assert_eq!(report.sides_printed, 3);
        assert!(report.all_agree);
        assert!(report.max_error_m < 1e-6);
    }

    #[test]
    fn catches_a_side_that_does_not_match() {
        // 18.50 printed where the coordinates say 3 m: a transposed digit, a
        // swapped column or a row read out of order all look like this.
        let report = check(&table(vec![Some(18.50), Some(4.0), Some(3.0)])).expect("report");
        assert!(!report.all_agree);
        assert_eq!(report.sides_agreeing, 2);
        assert!((report.max_error_m - 15.5).abs() < 1e-6);
        assert_eq!(report.sides[0].from, "1");
        assert_eq!(report.sides[0].to, "2");
    }

    #[test]
    fn tolerates_the_rounding_a_sheet_prints_with() {
        // 3.00 m printed as 3.0; half a centimetre of rounding is not an error.
        let report = check(&table(vec![Some(3.004), Some(3.996), Some(3.0)])).expect("report");
        assert!(report.all_agree, "{:?}", report.sides);
    }

    #[test]
    fn reports_nothing_when_the_sheet_prints_no_side_lengths() {
        assert!(check(&table(vec![None, None, None])).is_none());
    }
}
