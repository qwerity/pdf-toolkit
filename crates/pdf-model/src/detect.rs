//! Reading the parameters off the document itself.
//!
//! A drawing set states most of what the reconstruction needs, in text:
//! each sheet's title block names it (`+3,60 նիշով հարկի հատակագիծ`) and
//! prints its scale, the section prints the storey levels and slab
//! soffits as level marks, the roof plan names the roof level. The fill
//! colours are the plan sheets' own statistics: the one big light fill is
//! the envelope, the many dark ones are the walls, the white ones the
//! rooms. The grid labels are the single characters drawn in bubbles.
//!
//! Every field detected here is checkable against the document, and each
//! carries a note saying where it came from, so a reviewer can see what
//! was read and what fell back to the office template.

use std::collections::BTreeMap;

use anyhow::{Result, anyhow};
use pdf_classify::PageClass;
use pdf_classify::sheet::{self, SheetRole};
use pdf_geometry::PdfDocument;
use pdf_geometry::model::{PageContent, PaintMode};
use pdf_vector::DocumentVector;

use crate::params::{AxisParams, FillColours, FloorParams, Params, ParamsSource, Rgb};
use crate::sheet::axis_bubbles;

/// The detected parameters plus how each was read.
#[derive(Debug)]
pub struct Detection {
    pub params: Params,
    /// One line per decision, in reading order.
    pub notes: Vec<String>,
}

/// Slab soffit marks sit this far below a storey level (m).
const SLAB_GAP_M: std::ops::RangeInclusive<f64> = 0.15..=0.8;
/// A wall colour is dark; an envelope fill is light but not white.
const WALL_MAX_LUMINANCE: f64 = 0.55;
const ENVELOPE_LUMINANCE: std::ops::Range<f64> = 0.8..0.995;

/// What one sheet says about itself, gathered from the vector pass.
struct SheetFacts {
    page: usize,
    class: PageClass,
    role: Option<SheetRole>,
    title: Option<String>,
    level_m: Option<f64>,
    scale: Option<f64>,
}

fn facts(vector: &DocumentVector) -> Vec<SheetFacts> {
    vector
        .pages
        .iter()
        .map(|p| SheetFacts {
            page: p.page_number,
            class: p.classification.class,
            role: p.classification.features.sheet_role,
            title: p.classification.features.sheet_title.clone(),
            level_m: p.classification.features.sheet_level_m,
            scale: p
                .classification
                .features
                .declared_scales
                .iter()
                .copied()
                .find(|d| *d >= 20.0),
        })
        .collect()
}

/// Detects the parameters, starting from `template` for whatever the
/// document does not state.
pub fn detect(
    document: &PdfDocument,
    vector: &DocumentVector,
    template: &Params,
) -> Result<Detection> {
    let mut params = template.clone();
    params.source = ParamsSource::Detected;
    let mut notes = Vec::new();
    let sheets = facts(vector);

    // Storeys: the floor plans, ordered by the level their titles name.
    let mut floors: Vec<&SheetFacts> = sheets
        .iter()
        .filter(|s| s.role == Some(SheetRole::FloorPlan) || s.class == PageClass::FloorPlan)
        .collect();
    floors.sort_by(|a, b| {
        a.level_m
            .unwrap_or(f64::INFINITY)
            .total_cmp(&b.level_m.unwrap_or(f64::INFINITY))
            .then(a.page.cmp(&b.page))
    });
    if floors.is_empty() {
        return Err(anyhow!(
            "no sheet is titled as a floor plan; pass --params naming the plan pages"
        ));
    }

    // Levels: from the titles first, then the elevations' and section's
    // level marks for what the titles do not say.
    let roof_sheet = sheets
        .iter()
        .find(|s| s.role == Some(SheetRole::RoofPlan) || s.class == PageClass::RoofPlan);
    let elevation_marks = elevation_level_marks(document, &sheets);
    if let Some(roof) = roof_sheet {
        params.roof_page = Some(roof.page);
        if let Some(level) = roof.level_m {
            params.roof_level_m = level;
            notes.push(format!(
                "roof level {level} from the roof plan's title (page {})",
                roof.page
            ));
        }
        notes.push(format!("roof plan: page {}", roof.page));
    } else {
        notes.push("no roof plan sheet; the roof is omitted".into());
    }
    if let Some(&parapet) = elevation_marks.last()
        && parapet >= params.roof_level_m
    {
        params.parapet_level_m = parapet;
        notes.push(format!(
            "parapet level {parapet} from the highest level mark on the elevations"
        ));
    }
    if let Some(thickness) =
        slab_thickness(&elevation_marks, floors.iter().filter_map(|f| f.level_m))
    {
        params.slab_thickness_m = thickness;
        notes.push(format!(
            "slab thickness {thickness:.2} from paired level marks on the section"
        ));
    }
    if let Some(lowest) = elevation_marks.first().copied()
        && lowest < 0.0
        && -lowest < 2.0
    {
        params.plinth_m = -lowest;
        notes.push(format!(
            "plinth {:.2} from the lowest level mark",
            params.plinth_m
        ));
    }

    params.floors = Vec::new();
    let top_of = |index: usize| -> f64 {
        floors
            .get(index + 1)
            .and_then(|next| next.level_m)
            .unwrap_or(params.roof_level_m)
    };
    for (index, sheet) in floors.iter().enumerate() {
        let level_m = sheet.level_m.unwrap_or_else(|| {
            // A plan without a level in its title: stack it on the one below.
            params
                .floors
                .last()
                .map_or(0.0, |below: &FloorParams| below.top_m)
        });
        let mut top_m = top_of(index);
        if top_m <= level_m {
            top_m = level_m + 3.0;
            notes.push(format!(
                "page {}: no storey above and no roof level; assuming 3 m",
                sheet.page
            ));
        }
        params.floors.push(FloorParams {
            page: sheet.page,
            name: sheet
                .title
                .clone()
                .unwrap_or_else(|| format!("Floor at {level_m}")),
            level_m,
            top_m,
        });
    }
    notes.push(format!(
        "floors: {}",
        params
            .floors
            .iter()
            .map(|f| format!("page {} at {} to {}", f.page, f.level_m, f.top_m))
            .collect::<Vec<_>>()
            .join(", ")
    ));

    // Scale: printed on the plan sheets.
    if let Some(scale) = floors.iter().find_map(|f| f.scale) {
        params.scale_denominator = scale;
        notes.push(format!("scale 1:{scale} from the plan sheet's title block"));
    } else {
        notes.push(format!(
            "no printed scale on the plan sheets; template 1:{}",
            params.scale_denominator
        ));
    }

    // Colours and grid labels: from the ground-floor plan's own content.
    let ground = document.page_content(params.floors[0].page)?;
    match detect_colours(&ground) {
        Some((colours, note)) => {
            params.colours = colours;
            notes.push(note);
        }
        None => {
            notes.push("fill colours: template (too few fills to read them off the plan)".into())
        }
    }
    match detect_axis(&ground) {
        Some(axis) => {
            notes.push(format!(
                "grid origin: column '{}' x row '{}' from the bubbles on page {}",
                axis.column_label, axis.row_label, ground.page_number
            ));
            params.axis = axis;
        }
        None => notes.push(format!(
            "grid labels: template '{}'/'{}' (no lettered bubbles found)",
            params.axis.column_label, params.axis.row_label
        )),
    }

    params.validate()?;
    Ok(Detection { params, notes })
}

/// Distinct level marks on the elevation and section sheets, ascending.
fn elevation_level_marks(document: &PdfDocument, sheets: &[SheetFacts]) -> Vec<f64> {
    let mut marks: Vec<f64> = Vec::new();
    for sheet in sheets.iter().filter(|s| {
        matches!(s.role, Some(SheetRole::Elevation | SheetRole::Section))
            || (s.role.is_none() && s.class == PageClass::ElevationOrSection)
    }) {
        let Ok(content) = document.page_content(sheet.page) else {
            continue;
        };
        for mark in sheet::level_marks(&content) {
            if !marks.iter().any(|m| (m - mark).abs() < 0.001) {
                marks.push(mark);
            }
        }
    }
    marks.sort_by(f64::total_cmp);
    marks
}

/// The slab thickness the section states: each storey level has a soffit
/// mark a slab below it (`+3,600` over `+3,100`); the most common gap wins.
fn slab_thickness(marks: &[f64], levels: impl Iterator<Item = f64>) -> Option<f64> {
    let mut gaps: BTreeMap<i64, usize> = BTreeMap::new();
    for level in levels.filter(|l| *l > 0.0) {
        for mark in marks {
            let gap = level - mark;
            if SLAB_GAP_M.contains(&gap) {
                *gaps.entry((gap * 100.0).round() as i64).or_default() += 1;
            }
        }
    }
    gaps.into_iter()
        .max_by_key(|(gap, count)| (*count, -gap))
        .map(|(gap, _)| gap as f64 / 100.0)
}

fn luminance(c: [f64; 3]) -> f64 {
    pdf_geometry::model::Rgb {
        r: c[0],
        g: c[1],
        b: c[2],
    }
    .luminance()
}

/// Reads the element colours off a plan: the largest light fill is the
/// envelope, the most numerous dark fill the walls, white the rooms.
fn detect_colours(content: &PageContent) -> Option<(FillColours, String)> {
    #[derive(Default)]
    struct Stat {
        count: usize,
        largest: f64,
    }
    let mut stats: BTreeMap<[i64; 3], Stat> = BTreeMap::new();
    for path in &content.paths {
        if !matches!(path.mode, PaintMode::Fill | PaintMode::FillStroke) {
            continue;
        }
        let key = [path.fill.r, path.fill.g, path.fill.b].map(|v| (v * 1000.0).round() as i64);
        let stat = stats.entry(key).or_default();
        for subpath in &path.subpaths {
            stat.count += 1;
            stat.largest = stat.largest.max(subpath.signed_area().abs());
        }
    }
    let colour = |key: &[i64; 3]| Rgb(key.map(|v| v as f64 / 1000.0));
    let envelope = stats
        .iter()
        .filter(|(k, _)| ENVELOPE_LUMINANCE.contains(&luminance(colour(k).0)))
        .max_by(|a, b| a.1.largest.total_cmp(&b.1.largest))?;
    let wall = stats
        .iter()
        .filter(|(k, _)| luminance(colour(k).0) < WALL_MAX_LUMINANCE)
        .filter(|(_, s)| s.count >= 10)
        .max_by_key(|(_, s)| s.count)?;
    let room = stats
        .iter()
        .filter(|(k, _)| luminance(colour(k).0) >= 0.995)
        .max_by_key(|(_, s)| s.count)
        .map(|(k, _)| colour(k))
        .unwrap_or(Rgb([1.0, 1.0, 1.0]));
    let colours = FillColours {
        wall: colour(wall.0),
        envelope: colour(envelope.0),
        room,
        tolerance: 0.02,
    };
    let note = format!(
        "fill colours from page {}: walls {:?} ({} fills), envelope {:?}, rooms {:?}",
        content.page_number, colours.wall.0, wall.1.count, colours.envelope.0, colours.room.0
    );
    Some((colours, note))
}

/// The grid labels: the lowest digit bubble names the column axis, the
/// lowest letter bubble the row axis.
fn detect_axis(content: &PageContent) -> Option<AxisParams> {
    let bubbles = axis_bubbles(content);
    let column = bubbles
        .iter()
        .filter(|b| b.label.chars().all(|c| c.is_ascii_digit()))
        .min_by(|a, b| {
            a.centre
                .y
                .total_cmp(&b.centre.y)
                .then(a.centre.x.total_cmp(&b.centre.x))
        })?;
    let row = bubbles
        .iter()
        .filter(|b| b.label.chars().count() == 1 && b.label.chars().all(char::is_alphabetic))
        .min_by(|a, b| {
            a.centre
                .y
                .total_cmp(&b.centre.y)
                .then(a.centre.x.total_cmp(&b.centre.x))
        })?;
    Some(AxisParams {
        column_label: column.label.clone(),
        row_label: row.label.clone(),
        font_size: Some((column.size * 10.0).round() / 10.0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slab_thickness_is_the_common_gap_below_the_storey_levels() {
        let marks = [-0.35, 0.0, 3.1, 3.6, 6.7, 7.2, 7.75];
        assert_eq!(
            slab_thickness(&marks, [0.0, 3.6, 7.2].into_iter()),
            Some(0.5)
        );
        assert_eq!(
            slab_thickness(&marks, [0.0].into_iter()),
            None,
            "ground has no soffit"
        );
    }
}
