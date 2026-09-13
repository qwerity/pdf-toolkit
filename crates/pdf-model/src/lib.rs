//! Deterministic 3D building reconstruction from a CAD-exported drawing set.
//!
//! A vector PDF exported from a CAD package is exact: every wall, room and
//! envelope is a filled path with the fill colour of its element type, and
//! the plan sheets are printed at a known scale. So a storey is read by
//! colour, the storeys are stacked on the printed grid, and the openings are
//! what the envelope minus the rooms minus the walls leaves behind — all set
//! operations on exact geometry, no rendering, no tracing, no model call.
//!
//! What a drawing set does not print as geometry is a **parameter**
//! ([`params::Params`]): which sheet is which storey, the plan scale, the
//! storey levels, the fill colours, the grid labels. [`detect`] reads those
//! off the sheets' own title blocks and level marks; a reviewed JSON file
//! overrides them; a hosted model may propose them (in `pdf-vlm`). The
//! geometry is never theirs to change.
//!
//! Outputs, all from one [`model::Model`]: `model.json` (2D polygons with
//! z ranges), a binary glTF for any 3D viewer ([`mesh`]), and a map-native
//! extruded GeoJSON ([`geojson`]) — placed on the ground when the dossier
//! lets it be ([`place`]), in the building's own metres otherwise.

pub mod detect;
pub mod elements;
pub mod geojson;
pub mod geom;
pub mod mesh;
pub mod model;
pub mod params;
pub mod place;
pub mod reconstruct;
pub mod report;
pub mod run;
pub mod sheet;

use anyhow::{Context, Result};
use pdf_geometry::PdfDocument;
use serde::Serialize;

pub use detect::Detection;
pub use model::Model;
pub use params::{FloorParams, Params, ParamsSource};
pub use report::ModelReport;
pub use run::{Proposer, RunOptions, run};
pub use sheet::{OriginSource, Sheet};

/// How each sheet was read, for the report.
#[derive(Debug, Clone, Serialize)]
pub struct SheetReport {
    pub page: usize,
    /// `floor` or `roof`.
    pub role: &'static str,
    /// Grid origin in page units, `[x, y]`.
    pub origin_page: [f64; 2],
    pub origin_source: OriginSource,
    pub wall_fills: usize,
    pub envelope_fills: usize,
    pub room_fills: usize,
}

/// The model plus how it was made.
#[derive(Debug)]
pub struct Reconstruction {
    pub model: Model,
    pub sheets: Vec<SheetReport>,
}

/// Reads the sheets the parameters name and builds the model.
pub fn reconstruct(document: &PdfDocument, params: &Params) -> Result<Reconstruction> {
    params.validate()?;
    let read = |page: usize| -> Result<Sheet> {
        let content = document
            .page_content(page)
            .with_context(|| format!("cannot read page {page}"))?;
        Sheet::read(&content, params)
    };
    let mut sheets = Vec::new();
    let mut floor_sheets = Vec::new();
    for floor in &params.floors {
        let sheet = read(floor.page)?;
        sheets.push(report(&sheet, "floor"));
        floor_sheets.push(sheet);
    }
    let roof = params.roof_page.map(read).transpose()?;
    if let Some(sheet) = &roof {
        sheets.push(report(sheet, "roof"));
    }

    let inputs: Vec<reconstruct::FloorInput<'_>> = params
        .floors
        .iter()
        .zip(&floor_sheets)
        .map(|(params, sheet)| reconstruct::FloorInput { params, sheet })
        .collect();
    let model = reconstruct::build(&inputs, roof.as_ref(), params, &document.id())?;
    Ok(Reconstruction { model, sheets })
}

fn report(sheet: &Sheet, role: &'static str) -> SheetReport {
    SheetReport {
        page: sheet.page,
        role,
        origin_page: [sheet.origin.x, sheet.origin.y],
        origin_source: sheet.origin_source,
        wall_fills: sheet.walls.len(),
        envelope_fills: sheet.envelopes.len(),
        room_fills: sheet.rooms.len(),
    }
}
