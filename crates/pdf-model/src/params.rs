//! The per-document parameters the reconstruction runs on.
//!
//! Everything that was a constant in the reference implementation — which
//! sheet is which storey, the plan scale, the storey levels, the fill colour
//! each element type is exported with, the grid labels the storeys are
//! aligned on — is a field here. That is the seam that turns a one-off into a
//! pipeline: [`crate::detect`] fills the fields from the document itself, a
//! reviewed JSON file overrides them, and a hosted model may propose them
//! (`pdf-vlm`). **Coordinates never come from here**; only these numbers do.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// One storey: the plan sheet drawing it and the levels it spans.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FloorParams {
    /// One-based page of the floor plan.
    pub page: usize,
    pub name: String,
    /// Finished floor level, metres above ±0.000.
    pub level_m: f64,
    /// Level of the storey above (the roof for the top storey), metres.
    pub top_m: f64,
}

/// The fill colour an element type is exported with, RGB in 0..=1.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rgb(pub [f64; 3]);

impl Rgb {
    pub fn matches(&self, other: [f64; 3], tolerance: f64) -> bool {
        self.0
            .iter()
            .zip(other)
            .all(|(a, b)| (a - b).abs() < tolerance)
    }
}

/// The fill colours the plan sheets use for each element type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FillColours {
    /// Wall bodies (external walls and partitions).
    pub wall: Rgb,
    /// The building envelope rectangle and the terraces hugging it.
    pub envelope: Rgb,
    /// Room floor fills (and, harmlessly, furniture).
    pub room: Rgb,
    /// Per-channel tolerance of the colour match.
    pub tolerance: f64,
}

/// The grid axes every storey is aligned on: one column axis and one row
/// axis, named by their bubble labels. Their intersection is the model's
/// origin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AxisParams {
    /// Label of the column axis whose position gives the origin's x, e.g. `1`.
    pub column_label: String,
    /// Label of the row axis whose position gives the origin's y, e.g. `Ա`.
    pub row_label: String,
    /// Font size of the axis bubbles, when known; narrows the label search.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_size: Option<f64>,
}

/// Where the parameters came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParamsSource {
    /// Read from the document's own sheets: titles, level marks, fills.
    Detected,
    /// Loaded from a `--params` file.
    File,
    /// Proposed by a hosted model and checked against the document.
    Model,
}

/// Everything the reconstruction needs beyond the PDF itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Params {
    /// Plan-sheet scale denominator, e.g. `75` for 1:75.
    pub scale_denominator: f64,
    /// Storeys, bottom first.
    pub floors: Vec<FloorParams>,
    /// One-based page of the roof plan, when the set has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roof_page: Option<usize>,
    /// Top of the roof slab, metres.
    pub roof_level_m: f64,
    /// Top of the parapet, metres.
    pub parapet_level_m: f64,
    /// Thickness of the intermediate and roof slabs, metres.
    pub slab_thickness_m: f64,
    /// Depth of the ground slab below ±0.000, metres.
    pub plinth_m: f64,
    pub rail_height_m: f64,
    /// Width of the ring along the roof outline that stands in for a parapet
    /// the roof plan does not draw, metres.
    pub parapet_width_m: f64,
    pub colours: FillColours,
    pub axis: AxisParams,
    #[serde(default = "ParamsSource::file_default")]
    pub source: ParamsSource,
}

impl ParamsSource {
    fn file_default() -> Self {
        ParamsSource::File
    }
}

impl Params {
    /// The reference document's parameters (an ArchiCAD sketch-phase set of
    /// a two-storey house at 1:75), used as the starting point detection
    /// refines and as the office template's colour map.
    pub fn archicad_template() -> Self {
        Self {
            scale_denominator: 75.0,
            floors: Vec::new(),
            roof_page: None,
            roof_level_m: 7.2,
            parapet_level_m: 7.78,
            slab_thickness_m: 0.5,
            plinth_m: 0.35,
            rail_height_m: 1.1,
            parapet_width_m: 0.15,
            colours: FillColours {
                wall: Rgb([0.286, 0.267, 0.251]),
                envelope: Rgb([0.953, 0.953, 0.953]),
                room: Rgb([1.0, 1.0, 1.0]),
                tolerance: 0.02,
            },
            axis: AxisParams {
                column_label: "1".into(),
                row_label: "Ա".into(),
                font_size: None,
            },
            source: ParamsSource::Detected,
        }
    }

    pub fn load(path: &Path) -> Result<Self> {
        let body = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read params file '{}'", path.display()))?;
        let mut params: Params = serde_json::from_str(&body)
            .with_context(|| format!("params file '{}' is not valid", path.display()))?;
        params.source = ParamsSource::File;
        params.validate()?;
        Ok(params)
    }

    /// Millimetres on the ground per PDF point at the plan scale.
    pub fn mm_per_point(&self) -> f64 {
        pdf_geo::local::metres_per_page_unit(self.scale_denominator) * 1000.0
    }

    /// Refuses parameters that cannot describe a building.
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            (10.0..=10_000.0).contains(&self.scale_denominator),
            "scale denominator {} is not a drawing scale",
            self.scale_denominator
        );
        anyhow::ensure!(!self.floors.is_empty(), "no floor plan sheet named");
        for floor in &self.floors {
            anyhow::ensure!(
                floor.top_m > floor.level_m,
                "floor '{}' has its top {} at or below its level {}",
                floor.name,
                floor.top_m,
                floor.level_m
            );
        }
        anyhow::ensure!(
            self.parapet_level_m >= self.roof_level_m,
            "parapet {} below the roof {}",
            self.parapet_level_m,
            self.roof_level_m
        );
        anyhow::ensure!(
            self.slab_thickness_m > 0.0 && self.slab_thickness_m < 2.0,
            "slab thickness {} is not a slab",
            self.slab_thickness_m
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_template_round_trips_through_json_and_validates_once_it_has_floors() {
        let mut params = Params::archicad_template();
        assert!(params.validate().is_err(), "no floors yet");
        params.floors.push(FloorParams {
            page: 7,
            name: "Ground floor".into(),
            level_m: 0.0,
            top_m: 3.6,
        });
        params.validate().expect("a one-storey house is valid");
        let text = serde_json::to_string(&params).unwrap();
        let back: Params = serde_json::from_str(&text).unwrap();
        assert_eq!(back, params);
        assert!((params.mm_per_point() - 26.458).abs() < 0.001);
    }

    #[test]
    fn nonsense_levels_are_refused() {
        let mut params = Params::archicad_template();
        params.floors.push(FloorParams {
            page: 7,
            name: "x".into(),
            level_m: 3.0,
            top_m: 3.0,
        });
        assert!(params.validate().is_err());
    }
}
