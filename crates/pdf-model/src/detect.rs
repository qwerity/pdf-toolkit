//! Reading the parameters off the document itself.
//!
//! A drawing set states most of what the reconstruction needs, in text:
//! each sheet's title block names it (`+3,60 նիշով հարկի հատակագիծ`) and
//! prints its scale, the section prints the storey levels and slab
//! soffits as level marks, the roof plan names the roof level. The fill
//! colours are the plan sheets' own statistics: the many dark fills are the
//! walls — or, where the dark ones draw no wall-shaped band, the colour that
//! does — the one big light fill is the envelope where a plan draws one, and
//! the white fills are the rooms. The grid labels are the single characters
//! drawn in bubbles.
//!
//! Every field detected here is checkable against the document, and each
//! carries a note saying where it came from, so a reviewer can see what
//! was read and what fell back to the office template.

use std::collections::{BTreeMap, HashMap};

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
/// A wall colour draws at least this many fills.
const MIN_WALL_FILLS: usize = 10;
/// A wall fill is a band of a wall's thickness (m) at its narrowest...
const WALL_BAND_M: std::ops::RangeInclusive<f64> = 0.05..=0.6;
/// ...and at least this long (m): shorter bands are door leaves and symbols.
const WALL_BAND_MIN_M: f64 = 0.5;
/// A colour stands in for the walls when the dark fills draw no band at all
/// and it draws at least this many.
const MIN_WALL_BANDS: usize = 10;
/// Two storey levels read off the elevations are at least this far apart (m).
const MIN_STOREY_M: f64 = 2.4;
/// The storey height assumed when nothing on the drawings states one (m).
const DEFAULT_STOREY_M: f64 = 3.0;

/// What one sheet says about itself, gathered from the vector pass.
#[derive(Clone)]
struct SheetFacts {
    page: usize,
    class: PageClass,
    role: Option<SheetRole>,
    title: Option<String>,
    level_m: Option<f64>,
    scale: Option<f64>,
}

/// Sheet roles a caller knows from elsewhere — the dossier's reviewed or
/// vision-pass labels — for sets whose title blocks the classifier cannot
/// read. A hint fills in a role the sheet does not state; it never
/// overrides one it does.
#[derive(Debug, Clone, Default)]
pub struct SheetHints {
    pub roles: HashMap<usize, SheetRole>,
}

impl SheetHints {
    /// From answer-style labels per page: `floor_plan`, `roof_plan`,
    /// `elevation_or_section`, `site_plan`.
    pub fn from_labels<'a>(pages: impl IntoIterator<Item = (usize, &'a [String])>) -> Self {
        let mut roles = HashMap::new();
        for (page, labels) in pages {
            let role = labels.iter().find_map(|l| match l.as_str() {
                "floor_plan" => Some(SheetRole::FloorPlan),
                "roof_plan" => Some(SheetRole::RoofPlan),
                "elevation_or_section" => Some(SheetRole::Elevation),
                "site_plan" | "general_plan" => Some(SheetRole::SitePlan),
                _ => None,
            });
            if let Some(role) = role {
                roles.insert(page, role);
            }
        }
        Self { roles }
    }
}

fn facts(vector: &DocumentVector, hints: &SheetHints) -> Vec<SheetFacts> {
    vector
        .pages
        .iter()
        .map(|p| SheetFacts {
            page: p.page_number,
            class: p.classification.class,
            role: p
                .classification
                .features
                .sheet_role
                .or_else(|| hints.roles.get(&p.page_number).copied()),
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
    hints: &SheetHints,
) -> Result<Detection> {
    let mut params = template.clone();
    params.source = ParamsSource::Detected;
    let mut notes = Vec::new();
    let sheets = facts(vector, hints);
    let hinted: Vec<usize> = sheets
        .iter()
        .filter(|s| hints.roles.contains_key(&s.page))
        .map(|s| s.page)
        .collect();
    if !hinted.is_empty() {
        notes.push(format!(
            "sheet roles for page(s) {} taken from the answers' labels",
            hinted
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    // Storeys: the floor plans, one per storey, ordered by the level their titles name.
    let floors = storeys(
        sheets
            .iter()
            .filter(|s| s.role == Some(SheetRole::FloorPlan) || s.class == PageClass::FloorPlan)
            .cloned()
            .collect(),
        &mut notes,
    );
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
    // Elevations: the sheets the stone piers' heights are read from.
    params.elevation_pages = sheets
        .iter()
        .filter(|s| {
            s.role == Some(SheetRole::Elevation)
                || (s.role.is_none() && s.class == PageClass::ElevationOrSection)
        })
        .map(|s| s.page)
        .collect();
    if !params.elevation_pages.is_empty() {
        notes.push(format!(
            "elevations: page(s) {}",
            params
                .elevation_pages
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    // Sections: the sheets the slabs' edge bands are read from.
    params.section_pages = sheets
        .iter()
        .filter(|s| s.role == Some(SheetRole::Section))
        .map(|s| s.page)
        .collect();
    if !params.section_pages.is_empty() {
        notes.push(format!(
            "sections: page(s) {}",
            params
                .section_pages
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
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

    // Plans whose titles name no level: the elevations' marks name the
    // storeys — climbing from the ground, each the first mark a storey's
    // height above the last — and the roof is the first mark above the top
    // storey when its own sheet names none.
    let titled = floors.iter().filter(|f| f.level_m.is_some()).count();
    let mut derived: Vec<Option<f64>> = vec![None; floors.len()];
    if titled == 0 && !elevation_marks.is_empty() {
        let mut current = 0.0;
        derived[0] = Some(0.0);
        for slot in derived.iter_mut().skip(1) {
            let next = elevation_marks
                .iter()
                .copied()
                .find(|m| *m >= current + MIN_STOREY_M)
                .unwrap_or(current + DEFAULT_STOREY_M);
            *slot = Some(next);
            current = next;
        }
        if roof_sheet.is_some_and(|r| r.level_m.is_none()) || roof_sheet.is_none() {
            let roof = elevation_marks
                .iter()
                .copied()
                .find(|m| *m >= current + MIN_STOREY_M)
                .unwrap_or(current + DEFAULT_STOREY_M);
            let parapet_height = template.parapet_level_m - template.roof_level_m;
            params.roof_level_m = roof;
            params.parapet_level_m = elevation_marks
                .iter()
                .copied()
                .find(|m| *m > roof)
                .unwrap_or(roof + parapet_height.max(0.1));
        }
        notes.push(format!(
            "storey levels {} and roof level {} from the elevations' level marks (the plan titles name none)",
            derived
                .iter()
                .flatten()
                .map(|l| l.to_string())
                .collect::<Vec<_>>()
                .join(", "),
            params.roof_level_m
        ));
    }

    // A roof plan that names no level, over storeys whose titles do: the
    // template's roof is an office default, and where it does not clear the top
    // storey it would cut it, so the roof stands a storey above the top one.
    let top_level = floors
        .iter()
        .filter_map(|f| f.level_m)
        .max_by(f64::total_cmp);
    if titled > 0
        && roof_sheet.is_none_or(|r| r.level_m.is_none())
        && let Some(top) = top_level
        && params.roof_level_m < top + MIN_STOREY_M
    {
        let storey = storey_height(floors.iter().filter_map(|f| f.level_m));
        let parapet_height = (template.parapet_level_m - template.roof_level_m).max(0.1);
        params.roof_level_m = round_mm(top + storey);
        if params.parapet_level_m <= params.roof_level_m {
            params.parapet_level_m = round_mm(params.roof_level_m + parapet_height);
        }
        notes.push(format!(
            "roof level {} a storey ({storey}) above the top storey at {top}: the roof plan names \
             none and the template's {} would cut it",
            params.roof_level_m, template.roof_level_m
        ));
    }

    params.floors = Vec::new();
    let level_of = |index: usize| -> Option<f64> { floors[index].level_m.or(derived[index]) };
    let top_of = |index: usize| -> f64 {
        (index + 1 < floors.len())
            .then(|| level_of(index + 1))
            .flatten()
            .unwrap_or(params.roof_level_m)
    };
    for (index, sheet) in floors.iter().enumerate() {
        let level_m = level_of(index).unwrap_or_else(|| {
            // A plan without a level in its title: stack it on the one below.
            params
                .floors
                .last()
                .map_or(0.0, |below: &FloorParams| below.top_m)
        });
        let mut top_m = top_of(index);
        if top_m <= level_m {
            top_m = level_m + DEFAULT_STOREY_M;
            notes.push(format!(
                "page {}: no storey above and no roof level; assuming {DEFAULT_STOREY_M} m",
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

    // Colours and grid labels: from the ground-floor plan's own content. Read
    // across the plan sheets instead and one sheet's beige hatch becomes another
    // storey's envelope — measured on the corpus, it costs more than it gives.
    let ground = document.page_content(params.floors[0].page)?;
    match detect_colours(&ground, params.mm_per_point() / 1000.0) {
        Some((colours, note)) => {
            params.colours = colours;
            notes.push(note);
        }
        None => notes.push(format!(
            "no fill on page {} draws a wall band: the plan is line work, which these rules read \
             nothing from; the template's colours stand in and match little",
            ground.page_number
        )),
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

/// One sheet per storey, each at the level it stands at, ascending.
///
/// TWO SHEETS, ONE STOREY. A set may draw each storey twice in a row -- once
/// dimensioned, once furnished -- and both name the same level: consecutive
/// plan sheets at one level are one storey, drawn on the first.
///
/// A TITLE THAT NAMES THE WRONG LEVEL. Plans run up the set in page order. A
/// storey whose title puts it no higher than the one before it, between two
/// that climb by at least two storeys, is a misprint -- one corpus set titles
/// its second floor `-3,960`, its basement's level -- and stands halfway
/// between its neighbours. Read before the sheets are grouped by level, or the
/// misprint would be taken for another drawing of the basement.
fn storeys(mut plans: Vec<SheetFacts>, notes: &mut Vec<String>) -> Vec<SheetFacts> {
    plans.sort_by_key(|p| p.page);
    let same = |a: Option<f64>, b: Option<f64>| matches!((a, b), (Some(a), Some(b)) if (a - b).abs() < 0.005);

    let mut floors: Vec<SheetFacts> = Vec::new();
    let mut last_page = 0;
    for plan in plans {
        if let Some(storey) = floors.last()
            && plan.page == last_page + 1
            && same(storey.level_m, plan.level_m)
        {
            notes.push(format!(
                "page {} draws the storey of page {} again (level {}); one storey",
                plan.page,
                storey.page,
                plan.level_m.unwrap_or_default()
            ));
            last_page = plan.page;
            continue;
        }
        last_page = plan.page;
        floors.push(plan);
    }

    for i in 1..floors.len().saturating_sub(1) {
        let (Some(below), Some(level), Some(above)) = (
            floors[i - 1].level_m,
            floors[i].level_m,
            floors[i + 1].level_m,
        ) else {
            continue;
        };
        if level <= below && above - below >= 2.0 * MIN_STOREY_M {
            let between = round_mm((below + above) / 2.0);
            notes.push(format!(
                "page {}: its title names level {level}, no higher than page {}'s {below} though \
                 drawn after it; taken as {between}, between that and page {}'s {above}",
                floors[i].page,
                floors[i - 1].page,
                floors[i + 1].page
            ));
            floors[i].level_m = Some(between);
        }
    }

    floors.sort_by(|a, b| {
        a.level_m
            .unwrap_or(f64::INFINITY)
            .total_cmp(&b.level_m.unwrap_or(f64::INFINITY))
            .then(a.page.cmp(&b.page))
    });
    floors
}

/// The storey height the levels climb by: the median step between distinct
/// levels at least [`MIN_STOREY_M`] apart, or [`DEFAULT_STOREY_M`].
fn storey_height(levels: impl Iterator<Item = f64>) -> f64 {
    let mut levels: Vec<f64> = levels.collect();
    levels.sort_by(f64::total_cmp);
    levels.dedup_by(|a, b| (*a - *b).abs() < 0.005);
    let mut steps: Vec<f64> = levels
        .windows(2)
        .map(|w| w[1] - w[0])
        .filter(|step| *step >= MIN_STOREY_M)
        .collect();
    if steps.is_empty() {
        return DEFAULT_STOREY_M;
    }
    steps.sort_by(f64::total_cmp);
    round_mm(steps[steps.len() / 2])
}

/// Metres to the millimetre, the precision a drawing prints a level at.
fn round_mm(metres: f64) -> f64 {
    (metres * 1000.0).round() / 1000.0
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

/// Reads the element colours off a plan. The walls are the dark fills — or,
/// where those draw no wall-shaped band, the colour that does, since one office
/// draws its walls near-black and another mid-grey — the envelope is the
/// largest light fill that could hold those walls, and white the rooms.
/// `metres_per_unit` is the sheet's scale, which is what makes a band a band.
fn detect_colours(content: &PageContent, metres_per_unit: f64) -> Option<(FillColours, String)> {
    #[derive(Default)]
    struct Stat {
        count: usize,
        /// Fills shaped like a wall: a band of a wall's thickness, run out.
        bands: usize,
        /// The largest single fill of this colour, m².
        largest_m2: f64,
        /// The largest of its wall-shaped fills, m².
        largest_band_m2: f64,
    }
    let mut stats: BTreeMap<[i64; 3], Stat> = BTreeMap::new();
    for path in &content.paths {
        if !matches!(path.mode, PaintMode::Fill | PaintMode::FillStroke) {
            continue;
        }
        let key = [path.fill.r, path.fill.g, path.fill.b].map(|v| (v * 1000.0).round() as i64);
        let stat = stats.entry(key).or_default();
        for subpath in &path.subpaths {
            let area = subpath.signed_area().abs() * metres_per_unit * metres_per_unit;
            stat.count += 1;
            stat.largest_m2 = stat.largest_m2.max(area);
            let bbox = subpath.bbox();
            // The narrowest honest measure of the shape, because offices differ
            // in how they emit a storey's walls. One draws a rectangle per
            // segment, where the bounding box's short side *is* the thickness.
            // Another draws the whole wall network as one ring of forty
            // vertices, where the bounding box is the building: on one corpus
            // dossier that put nine of ten wall polygons outside `WALL_BAND_M`
            // -- a 5.9 x 12.3 m box around a 0.39 m wall -- and every storey
            // came out empty. `mean_thickness` does not care what shape the
            // band is bent into.
            let across =
                (bbox.width().min(bbox.height())).min(subpath.mean_thickness()) * metres_per_unit;
            let along = bbox.width().max(bbox.height()) * metres_per_unit;
            if WALL_BAND_M.contains(&across) && along >= WALL_BAND_MIN_M {
                stat.bands += 1;
                stat.largest_band_m2 = stat.largest_band_m2.max(area);
            }
        }
    }
    let colour = |key: &[i64; 3]| Rgb(key.map(|v| v as f64 / 1000.0));
    let white = |key: &[i64; 3]| luminance(colour(key).0) >= 0.995;
    // The walls are the dark fills, as most offices draw them. A dark colour
    // that draws no wall-shaped band at all is something else — dossier 14's
    // arrowheads — and the colour that does draw bands takes its place.
    let dark = stats
        .iter()
        .filter(|(k, s)| luminance(colour(k).0) < WALL_MAX_LUMINANCE && s.count >= MIN_WALL_FILLS)
        .max_by_key(|(_, s)| s.count);
    let banded = stats
        .iter()
        .filter(|(k, s)| !white(k) && s.bands >= MIN_WALL_BANDS)
        .max_by_key(|(_, s)| s.bands);
    let wall = match (dark, banded) {
        (Some(dark), Some(banded)) if dark.1.bands == 0 => banded,
        (Some(dark), _) => dark,
        (None, banded) => banded?,
    };
    // The envelope is the biggest light fill, and it has to be able to hold the
    // walls: a fill smaller than the widest wall band is a blob — a hatch key, a
    // stair tread — and cutting a storey to it leaves nothing. A plan that draws
    // no envelope is not a plan these rules cannot read: the building box comes
    // from the outline of the walls and rooms, which the storey rules fall back
    // to on their own.
    let envelope = stats
        .iter()
        .filter(|(k, s)| {
            ENVELOPE_LUMINANCE.contains(&luminance(colour(k).0))
                && *k != wall.0
                && s.largest_m2 >= wall.1.largest_band_m2
        })
        .max_by(|a, b| a.1.largest_m2.total_cmp(&b.1.largest_m2));
    let room = stats
        .iter()
        .filter(|(k, _)| white(k))
        .max_by_key(|(_, s)| s.count)
        .map(|(k, _)| colour(k))
        .unwrap_or(Rgb([1.0, 1.0, 1.0]));
    let colours = FillColours {
        wall: colour(wall.0),
        // A plan that draws no envelope fill leaves the building box to the
        // outline of the walls and rooms; the template's colour then matches
        // nothing on the sheet, which is what that fallback wants.
        envelope: envelope.map_or(Params::archicad_template().colours.envelope, |(k, _)| {
            colour(k)
        }),
        room,
        tolerance: 0.02,
    };
    let envelope_note = match envelope {
        Some((_, stat)) => format!(
            "envelope {:?} ({:.0} m²)",
            colours.envelope.0, stat.largest_m2
        ),
        None => {
            "no light fill big enough for the envelope, so the box comes from the walls and rooms"
                .to_string()
        }
    };
    let note = format!(
        "fill colours from page {}: walls {:?} ({} fills, {} bands), {envelope_note}, rooms {:?}",
        content.page_number, colours.wall.0, wall.1.count, wall.1.bands, colours.room.0
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
    use pdf_geometry::model::{BBox, Point, Rgb as Fill, SubPath, VectorPath};

    /// Page units in a metre at 1:75.
    const UNITS_PER_M: f64 = 72.0 / (75.0 * 0.0254);

    /// A closed ring of `points`, in metres on a 1:75 sheet.
    fn ring(points: &[(f64, f64)]) -> SubPath {
        SubPath {
            points: points
                .iter()
                .map(|(x, y)| Point::new(x * UNITS_PER_M, y * UNITS_PER_M))
                .collect(),
            closed: true,
        }
    }

    /// A wall reads as its own thickness however the office bent it.
    ///
    /// The office whose plans prompted this draws a storey's whole wall
    /// network as one polygon: a 10 x 8 m rectangle with a 9.4 x 7.4 m hole,
    /// traced as a single ring. Its bounding box is the building, which is
    /// what used to be measured, and its walls are 300 mm.
    #[test]
    fn a_wall_bent_into_a_ring_is_still_a_wall() {
        let outer = [(0.0, 0.0), (10.0, 0.0), (10.0, 8.0), (0.0, 8.0)];
        let inner = [(0.3, 0.3), (0.3, 7.7), (9.7, 7.7), (9.7, 0.3)];
        // One ring out and back, the way a filled wall network is emitted.
        let mut points: Vec<(f64, f64)> = outer.to_vec();
        points.push((0.0, 0.0));
        points.extend_from_slice(&inner);
        points.push((0.3, 0.3));
        let wall = ring(&points);
        let m = 1.0 / UNITS_PER_M;
        let thickness = wall.mean_thickness() * m;
        assert!(
            WALL_BAND_M.contains(&thickness),
            "a 0.3 m wall measured {thickness:.3} m"
        );
        // ...while its bounding box says the whole building, which is the
        // measurement that used to reject it.
        let bbox = wall.bbox();
        let across = bbox.width().min(bbox.height()) * m;
        assert!(!WALL_BAND_M.contains(&across), "bbox across {across:.3} m");
    }

    /// And a compact fill is still not a wall: a square room of side `s` has
    /// a mean thickness of `s / 2`, far outside any wall band.
    #[test]
    fn a_room_sized_square_is_not_a_wall_band() {
        let square = ring(&[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)]);
        let m = 1.0 / UNITS_PER_M;
        let thickness = square.mean_thickness() * m;
        assert!((thickness - 2.0).abs() < 1e-6, "{thickness}");
        assert!(!WALL_BAND_M.contains(&thickness));
    }

    /// A filled rectangle of that colour, given in metres on a 1:75 sheet.
    fn fill(colour: [f64; 3], x0: f64, y0: f64, x1: f64, y1: f64) -> VectorPath {
        let p = |x: f64, y: f64| Point::new(x * UNITS_PER_M, y * UNITS_PER_M);
        VectorPath {
            subpaths: vec![SubPath {
                points: vec![p(x0, y0), p(x1, y0), p(x1, y1), p(x0, y1), p(x0, y0)],
                closed: true,
            }],
            mode: PaintMode::Fill,
            stroke: Fill::BLACK,
            fill: Fill {
                r: colour[0],
                g: colour[1],
                b: colour[2],
            },
            width: 0.0,
            layer: None,
        }
    }

    fn page(paths: Vec<VectorPath>) -> PageContent {
        PageContent {
            page_number: 13,
            media_box: BBox {
                min_x: 0.0,
                min_y: 0.0,
                max_x: 1190.0,
                max_y: 842.0,
            },
            paths,
            texts: Vec::new(),
            image_count: 0,
            image_coverage: 0.0,
        }
    }

    /// Twelve wall bands of that colour, 0.4 m thick and 3 m long.
    fn bands(colour: [f64; 3]) -> Vec<VectorPath> {
        (0..12)
            .map(|i| {
                let y = 0.5 + f64::from(i) * 0.6;
                fill(colour, 0.5, y, 3.5, y + 0.4)
            })
            .collect()
    }

    #[test]
    fn the_walls_are_the_colour_that_draws_bands_whatever_its_tone() {
        let mut paths = vec![fill([0.95, 0.95, 0.95], 0.0, 0.0, 10.0, 8.0)];
        // Mid-grey walls: a rule that asked for a dark tone would pass them by.
        paths.extend(bands([0.753, 0.753, 0.753]));
        paths.push(fill([1.0, 1.0, 1.0], 1.0, 1.0, 4.0, 4.0));
        let (colours, note) =
            detect_colours(&page(paths), 1.0 / UNITS_PER_M).expect("the colours read");
        assert_eq!(colours.wall.0, [0.753, 0.753, 0.753], "{note}");
        assert_eq!(colours.envelope.0, [0.95, 0.95, 0.95], "{note}");
        assert_eq!(colours.room.0, [1.0, 1.0, 1.0], "{note}");
        assert!(
            note.contains("12 fills, 12 bands") && note.contains("80 m²"),
            "{note}"
        );
    }

    #[test]
    fn a_light_fill_smaller_than_the_walls_is_no_envelope() {
        let mut paths = bands([0.753, 0.753, 0.753]);
        // A light blob of 0.6 m², narrower than the 1.2 m² wall bands beside it:
        // a hatch key or a stair tread, not a building box.
        paths.push(fill([0.95, 0.95, 0.95], 8.0, 8.0, 8.8, 8.75));
        let (colours, note) =
            detect_colours(&page(paths), 1.0 / UNITS_PER_M).expect("the walls read");
        assert_eq!(colours.wall.0, [0.753, 0.753, 0.753], "{note}");
        assert!(
            note.contains("no light fill big enough for the envelope"),
            "the blob is refused and the note says so: {note}"
        );
    }

    #[test]
    fn a_plan_drawn_in_line_work_reads_no_colours() {
        // White fills and one small dark blob: nothing shaped like a wall.
        let paths = vec![
            fill([1.0, 1.0, 1.0], 0.0, 0.0, 2.0, 2.0),
            fill([0.0, 0.0, 0.0], 0.0, 0.0, 0.1, 0.1),
        ];
        assert!(detect_colours(&page(paths), 1.0 / UNITS_PER_M).is_none());
    }

    #[test]
    fn walls_without_a_light_fill_leave_the_building_box_to_them() {
        let mut paths = bands([0.753, 0.753, 0.753]);
        // Nothing light enough to be an envelope: the only other fill is a
        // mid-grey block, which a plan draws for a shaft or a stair.
        paths.push(fill([0.7, 0.7, 0.7], 0.0, 0.0, 2.0, 1.0));
        let (colours, note) =
            detect_colours(&page(paths), 1.0 / UNITS_PER_M).expect("the walls read");
        assert_eq!(colours.wall.0, [0.753, 0.753, 0.753]);
        assert!(
            note.contains("no light fill big enough for the envelope"),
            "the note says where the box will come from: {note}"
        );
    }

    #[test]
    fn a_dark_colour_that_draws_no_band_gives_way_to_one_that_does() {
        let mut paths = bands([0.753, 0.753, 0.753]);
        // Twelve black arrowheads: dark and numerous, but none is a wall.
        for i in 0..12 {
            let x = f64::from(i) * 0.3;
            paths.push(fill([0.0, 0.0, 0.0], x, 9.0, x + 0.12, 9.12));
        }
        let (colours, _) = detect_colours(&page(paths), 1.0 / UNITS_PER_M).expect("the walls read");
        assert_eq!(colours.wall.0, [0.753, 0.753, 0.753]);
    }

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

    fn plan(page: usize, level_m: Option<f64>) -> SheetFacts {
        SheetFacts {
            page,
            class: PageClass::FloorPlan,
            role: Some(SheetRole::FloorPlan),
            title: None,
            level_m,
            scale: None,
        }
    }

    #[test]
    fn a_storey_drawn_on_two_sheets_in_a_row_is_one_storey() {
        let mut notes = Vec::new();
        let floors = storeys(
            vec![
                plan(10, Some(-7.92)),
                plan(11, Some(-7.92)),
                plan(12, Some(0.0)),
            ],
            &mut notes,
        );
        assert_eq!(
            floors.iter().map(|f| f.page).collect::<Vec<_>>(),
            vec![10, 12]
        );
        assert!(notes[0].contains("page 11 draws the storey of page 10"));
    }

    #[test]
    fn a_title_that_names_a_lower_level_between_climbing_neighbours_is_a_misprint() {
        let mut notes = Vec::new();
        let floors = storeys(
            vec![
                plan(12, Some(-3.96)),
                plan(14, Some(0.0)),
                plan(16, Some(-3.96)),
                plan(17, Some(-3.96)),
                plan(18, Some(7.92)),
            ],
            &mut notes,
        );
        let levels: Vec<(usize, Option<f64>)> =
            floors.iter().map(|f| (f.page, f.level_m)).collect();
        assert_eq!(
            levels,
            vec![
                (12, Some(-3.96)),
                (14, Some(0.0)),
                (16, Some(3.96)),
                (18, Some(7.92))
            ]
        );
    }

    #[test]
    fn the_same_level_on_sheets_apart_is_left_alone_when_nothing_climbs_around_it() {
        let mut notes = Vec::new();
        // Two wings at one level, a sheet between: no climb either side, no repair.
        let floors = storeys(
            vec![plan(5, Some(0.0)), plan(6, Some(3.0)), plan(8, Some(0.0))],
            &mut notes,
        );
        assert_eq!(floors.len(), 3);
        assert!(notes.is_empty());
    }

    #[test]
    fn storey_height_is_the_median_step_of_the_levels() {
        assert_eq!(
            storey_height([-7.92, -3.96, 0.0, 3.96, 7.92].into_iter()),
            3.96
        );
        assert_eq!(storey_height([0.0].into_iter()), DEFAULT_STOREY_M);
        assert_eq!(storey_height([0.0, 0.3, 3.3].into_iter()), 3.0);
    }
}
