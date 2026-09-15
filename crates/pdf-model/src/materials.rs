//! The materials the drawings call out.
//!
//! An elevation labels what its surfaces are made of with a leader and a
//! callout — `ԵՐԵՍԱՊԱՏՈՒՄ ՏՐԱՎԵՏՐԻՆԵ ՍԱԼԻԿՈՎ`, facing in travertine tiles —
//! and so do the section and the roof plan. A callout names the part it labels
//! and the material, in the office's words and as the drafter spelt them.
//! [`read`] gathers the callouts off the sheets the parameters name, reads each
//! against tables of word stems — the part it names (a parapet, a railing, the
//! roof…) and the material (travertine, basalt, metal sheet…) — and settles
//! each part on the material the most sheets call out, with a warning when the
//! sheets disagree. A callout naming no part the model draws, or no material
//! the tables know, is left out; so is a sheet's title.

use indexmap::IndexMap;
use pdf_geometry::PdfDocument;
use pdf_geometry::model::{PageContent, TextItem};

use crate::model::{Material, MaterialCall, Part};
use crate::params::Params;

/// Runs within this many font sizes of one baseline are on one line.
const BASELINE_SIZES: f64 = 0.4;
/// A gap wider than this many font sizes between runs on a line starts
/// another callout.
const CALLOUT_GAP_SIZES: f64 = 3.0;
/// The average advance of a glyph, as a share of the font size.
const AVERAGE_GLYPH_WIDTH: f64 = 0.6;

/// Words that make a line a sheet's title rather than a callout: plan, facade,
/// section, scale.
const TITLE_STEMS: &[&str] = &["ՀԱՏԱԿԱԳԻԾ", "ՃԱԿԱՏ", "ԿՏՐՎԱԾՔ", "ՄԱՍՇՏԱԲ"];

/// The parts a callout names by their own word, first match winning: a
/// callout of stone masonry for the parapet is the parapet's.
const PART_STEMS: &[(Part, &[&str])] = &[
    (Part::Parapet, &["ՊԱՐԱՊԵՏ"]),
    (Part::Rail, &["ԲԱԶՐԻՔ"]),
    (Part::Glazing, &["ՎԻՏՐԱԺ", "ՊԱՏՈՒՀԱՆ"]),
    (Part::Plinth, &["ՈՐՄՆԱԽԱՐԻՍԽ", "ՑՈԿՈԼ"]),
    (Part::Roof, &["ՏԱՆԻՔ", "ԿՂՄԻՆԴՐ"]),
    (Part::Slab, &["ԾԱԾԿԻ ՍԱԼ", "ՄԻՋՀԱՐԿԱՅԻՆ"]),
];

/// The parts a callout names by how they are built, when it names none by its
/// word: stone masonry is the stone piers, a facing or a plaster the walls.
const SURFACE_STEMS: &[(Part, &[&str])] = &[
    (Part::Stone, &["ՔԱՐԻ ՇԱՐ", "ՔԱՐԵ ՇԱՐՎԱԾՔ", "ՔՐՔԱՏԱՇ"]),
    (Part::Wall, &["ԵՐԵՍԱՊԱՏ", "ՍՎԱՂ", "ՆԵՐԿՈՒՄ"]),
];

/// The materials, the most particular first — travertine tiles are stone,
/// but travertine — with the spellings the corpus prints (`ՏՐԱՎԵՏՐԻՆԵ`,
/// `ՄԵՏՂԱԿԱՆ`, `ՔԱՆԵ`).
const MATERIAL_STEMS: &[(Material, &[&str])] = &[
    (
        Material::Travertine,
        &["ՏՐԱՎԵՐՏԻՆ", "ՏՐԱՎԵՏՐԻՆ", "ՐԱՎԵՐՏԻՆ"],
    ),
    (Material::Basalt, &["ԲԱԶԱԼՏ"]),
    (Material::Felsite, &["ՖԵԼԶԻՏ"]),
    (Material::Tuff, &["ՏՈՒՖ"]),
    (Material::RoughStone, &["ՔՐՔԱՏԱՇ"]),
    (Material::Plaster, &["ՍՎԱՂ"]),
    (Material::MetalSheet, &["ԹԻԹԵՂ", "ԿՂՄԻՆԴՐԱՏԻՊ"]),
    (Material::Metal, &["ՄԵՏԱՂ", "ՄԵՏՂ"]),
    (Material::Glass, &["ՎԻՏՐԱԺ", "ԱՊԱԿԻ"]),
    (Material::Concrete, &["Ե/Բ", "ԲԵՏՈՆ"]),
    (Material::Stone, &["ՔԱՐԵ", "ՔԱՐԻ", "ՔԱՆԵ"]),
];

impl Part {
    /// The part's name in a warning.
    pub fn name(self) -> &'static str {
        match self {
            Part::Wall => "walls",
            Part::Glazing => "glazing",
            Part::Rail => "railings",
            Part::Stone => "stone piers",
            Part::Roof => "roof",
            Part::Parapet => "parapet",
            Part::Slab => "slabs",
            Part::Plinth => "plinth",
        }
    }
}

impl Material {
    /// The material's name in a warning.
    pub fn name(self) -> &'static str {
        match self {
            Material::Travertine => "travertine",
            Material::Basalt => "basalt",
            Material::Felsite => "felsite",
            Material::Tuff => "tuff",
            Material::RoughStone => "rough stone",
            Material::Stone => "stone",
            Material::Plaster => "plaster",
            Material::Concrete => "concrete",
            Material::Metal => "metal",
            Material::MetalSheet => "metal sheet",
            Material::Glass => "glass",
        }
    }
}

/// Each part's material off the callouts of the sheets `params` names: the
/// elevations, the sections and the roof plan.
pub fn read(
    document: &PdfDocument,
    params: &Params,
    warnings: &mut Vec<String>,
) -> IndexMap<Part, MaterialCall> {
    let mut pages: Vec<usize> = params
        .elevation_pages
        .iter()
        .chain(&params.section_pages)
        .copied()
        .chain(params.roof_page)
        .collect();
    pages.sort_unstable();
    pages.dedup();
    let sheets = pages.into_iter().filter_map(|page| {
        document
            .page_content(page)
            .ok()
            .map(|content| (page, callouts(&content)))
    });
    settle(sheets, warnings)
}

/// A page's callouts as lines of text: horizontal runs on one baseline, left
/// to right, a gap wider than [`CALLOUT_GAP_SIZES`] starting another callout.
pub fn callouts(content: &PageContent) -> Vec<String> {
    let mut runs: Vec<&TextItem> = content
        .texts
        .iter()
        .filter(|run| run.angle.abs() < 0.05 && !run.text.trim().is_empty())
        .collect();
    runs.sort_by(|a, b| {
        b.origin
            .y
            .total_cmp(&a.origin.y)
            .then_with(|| a.origin.x.total_cmp(&b.origin.x))
    });
    let mut lines: Vec<Vec<&TextItem>> = Vec::new();
    for run in runs {
        match lines.last_mut() {
            Some(line)
                if (run.origin.y - line[0].origin.y).abs()
                    <= BASELINE_SIZES * run.size.max(line[0].size) =>
            {
                line.push(run);
            }
            _ => lines.push(vec![run]),
        }
    }
    let mut out = Vec::new();
    for mut line in lines {
        line.sort_by(|a, b| a.origin.x.total_cmp(&b.origin.x));
        let mut words: Vec<&str> = Vec::new();
        let mut end = f64::NEG_INFINITY;
        for run in line {
            if !words.is_empty() && run.origin.x - end > CALLOUT_GAP_SIZES * run.size {
                out.push(words.join(" "));
                words.clear();
            }
            words.extend(run.text.split_whitespace());
            let advance = run.text.chars().count() as f64 * AVERAGE_GLYPH_WIDTH * run.size;
            end = end.max(run.origin.x + advance);
        }
        if !words.is_empty() {
            out.push(words.join(" "));
        }
    }
    out
}

/// The part a callout names and its material; `None` for a sheet's title, or
/// a callout naming no part the model draws or no material the tables know.
pub fn classify(text: &str) -> Option<(Part, Material)> {
    let text = text.to_uppercase();
    let names = |stem: &&str| text.contains(stem);
    if TITLE_STEMS.iter().any(names) {
        return None;
    }
    let named = |table: &[(Part, &[&str])]| {
        table
            .iter()
            .find(|(_, stems)| stems.iter().any(names))
            .map(|(part, _)| *part)
    };
    let part = named(PART_STEMS).or_else(|| named(SURFACE_STEMS))?;
    let material = MATERIAL_STEMS
        .iter()
        .find(|(_, stems)| stems.iter().any(names))
        .map(|(material, _)| *material)?;
    Some((part, material))
}

/// Each part's material from the sheets' callouts, `(page, callouts)` in page
/// order: the material the most sheets call out — the first read on a tie —
/// with the first callout's text and every page that calls it out.
fn settle(
    sheets: impl IntoIterator<Item = (usize, Vec<String>)>,
    warnings: &mut Vec<String>,
) -> IndexMap<Part, MaterialCall> {
    let mut called: IndexMap<Part, IndexMap<Material, MaterialCall>> = IndexMap::new();
    for (page, texts) in sheets {
        for text in texts {
            let Some((part, material)) = classify(&text) else {
                continue;
            };
            let call = called
                .entry(part)
                .or_default()
                .entry(material)
                .or_insert_with(|| MaterialCall {
                    material,
                    text,
                    pages: Vec::new(),
                });
            if !call.pages.contains(&page) {
                call.pages.push(page);
            }
        }
    }
    let mut materials: IndexMap<Part, MaterialCall> = IndexMap::new();
    for (part, calls) in called {
        let Some(kept) = calls
            .values()
            .fold(None, |kept: Option<&MaterialCall>, call| match kept {
                Some(kept) if kept.pages.len() >= call.pages.len() => Some(kept),
                _ => Some(call),
            })
            .cloned()
        else {
            continue;
        };
        if calls.len() > 1 {
            let named: Vec<String> = calls
                .values()
                .map(|call| format!("{} ({})", call.material.name(), pages(&call.pages)))
                .collect();
            warnings.push(format!(
                "{}: the callouts name {}; {} is kept",
                part.name(),
                named.join(" and "),
                kept.material.name()
            ));
        }
        materials.insert(part, kept);
    }
    materials.sort_keys();
    materials
}

/// `page 19`, or `pages 16, 17, 18`.
fn pages(pages: &[usize]) -> String {
    let listed: Vec<String> = pages.iter().map(|page| page.to_string()).collect();
    match listed.len() {
        1 => format!("page {}", listed[0]),
        _ => format!("pages {}", listed.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdf_geometry::model::{BBox, Point, Rgb};

    fn run(text: &str, x: f64, y: f64, angle: f64) -> TextItem {
        TextItem {
            text: text.into(),
            origin: Point::new(x, y),
            size: 7.8,
            angle,
            color: Rgb::BLACK,
        }
    }

    #[test]
    fn a_callout_is_a_line_of_runs_and_a_wide_gap_starts_another() {
        let content = PageContent {
            page_number: 15,
            media_box: BBox {
                min_x: 0.0,
                min_y: 0.0,
                max_x: 1190.0,
                max_y: 842.0,
            },
            paths: Vec::new(),
            texts: vec![
                run("ՏԱՆԻՔԻ ", 787.4, 640.7, 0.0),
                run("ԾԱԼՔԱՎՈՐ ", 824.9, 640.7, 0.0),
                run("ԹԻԹԵՂ", 873.8, 640.7, 0.0),
                // Another callout on nearly the same baseline, well to the right.
                run("ՔԱՆԵ ", 960.4, 641.6, 0.0),
                run("ՊԱՐԱՊԵՏ", 986.5, 641.6, 0.0),
                // A line below, and a dimension set upright.
                run("ՉՇԱՀԱԳՈՐԾՎՈՂ ՏԱՆԻՔ", 561.2, 625.1, 0.0),
                run("5 900", 300.0, 500.0, std::f64::consts::FRAC_PI_2),
            ],
            image_count: 0,
            image_coverage: 0.0,
        };
        assert_eq!(
            callouts(&content),
            [
                "ՏԱՆԻՔԻ ԾԱԼՔԱՎՈՐ ԹԻԹԵՂ",
                "ՔԱՆԵ ՊԱՐԱՊԵՏ",
                "ՉՇԱՀԱԳՈՐԾՎՈՂ ՏԱՆԻՔ"
            ]
        );
    }

    #[test]
    fn a_callout_names_its_part_and_material_however_the_drafter_spelt_them() {
        for (text, expected) in [
            (
                "ԵՐԵՍԱՊԱՏՈՒՄ ՏՐԱՎԵՏՐԻՆԵ ՍԱԼԻԿՈՎ",
                Some((Part::Wall, Material::Travertine)),
            ),
            (
                "ԵՐԵՍԱՊԱՏՈՒՄ ԲԱՑ ԵՐԱՆԳԻ /ՏՐԱՎԵՐՏԻՆԵ",
                Some((Part::Wall, Material::Travertine)),
            ),
            (
                "Ց/ԱՎԱԶԱՅԻՆ ԴԵԿՈՐԱՏԻՎ ՍՎԱՂ/ՆԵՐԿՈՒՄՈՎ",
                Some((Part::Wall, Material::Plaster)),
            ),
            (
                "ՔՐՔԱՏԱՇ ՔԱՐԻ ՇԱՐ",
                Some((Part::Stone, Material::RoughStone)),
            ),
            (
                "ՔԱՐԵ ՇԱՐՎԱԾՔ/ՊԱՐԱՊԵՏ",
                Some((Part::Parapet, Material::Stone)),
            ),
            ("ՔԱՆԵ ՊԱՐԱՊԵՏ", Some((Part::Parapet, Material::Stone))),
            ("ՄԵՏՂԱԿԱՆ ԲԱԶՐԻՔ", Some((Part::Rail, Material::Metal))),
            (
                "ՎԻՏՐԱԺԱՅԻՆ ՊԱՏՈՒՀԱՆՆԵՐ",
                Some((Part::Glazing, Material::Glass)),
            ),
            (
                "ՏԱՆԻՔԻ ԾԱԼՔԱՎՈՐ ԹԻԹԵՂ",
                Some((Part::Roof, Material::MetalSheet)),
            ),
            ("Ե/Բ ԾԱԾԿԻ ՍԱԼ", Some((Part::Slab, Material::Concrete))),
            (
                "ՈՐՄՆԱԽԱՐԻՍԽ ԲԱԶԱԼՏԵ ՍԱԼԻԿԻ ԵՐԵՍԱՊԱՏՄԱՄԲ",
                Some((Part::Plinth, Material::Basalt)),
            ),
            // A roof covering of metal tiles is the roof's, not a slab's; and a
            // callout set in lower case reads as one set in capitals.
            (
                "ՄԵՏԱՂԱԿԱՆ ԿՂՄԻՆԴՐԱՏԻՊԾԱԾԿԻ",
                Some((Part::Roof, Material::MetalSheet)),
            ),
            (
                "կղմինդրատիպ մետաղական",
                Some((Part::Roof, Material::MetalSheet)),
            ),
            (
                "քարե երեսապատում դեկորատիվ",
                Some((Part::Wall, Material::Stone)),
            ),
            // No part the model draws, no material the tables know, a title.
            ("Ե/Բ ԱՍՏԻՃԱՆԱՎԱՆԴԱԿ", None),
            ("ԱՆՁՐԵՎԱՏԱՐ ԽՈՂՈՎԱԿ", None),
            ("ՉՇԱՀԱԳՈՐԾՎՈՂ ՏԱՆԻՔ", None),
            ("ԱԼԻՔԱՎՈՐ ՑԻՆԿԱՊԱՏ ԹԻԹԵՂ", None),
            ("ՏԱՆԻՔԻ ՀԱՏԱԿԱԳԻԾ Մ 1։100", None),
        ] {
            assert_eq!(classify(text), expected, "{text}");
        }
    }

    #[test]
    fn each_part_keeps_the_material_most_sheets_call_out() {
        let facing = "ԵՐԵՍԱՊԱՏՈՒՄ ՏՐԱՎԵՏՐԻՆԵ ՍԱԼԻԿՈՎ";
        let mut warnings = Vec::new();
        let materials = settle(
            [
                (16, vec![facing.to_string(), "ՔՐՔԱՏԱՇ ՔԱՐԻ ՇԱՐ".to_string()]),
                (18, vec![facing.to_string(), facing.to_string()]),
                (
                    19,
                    vec![
                        "Ց/ԱՎԱԶԱՅԻՆ ԴԵԿՈՐԱՏԻՎ ՍՎԱՂ/ՆԵՐԿՈՒՄՈՎ".to_string(),
                        "ՔԱՐԵ ՊԱՐԱՊԵՏ".to_string(),
                    ],
                ),
            ],
            &mut warnings,
        );
        assert_eq!(
            materials.keys().copied().collect::<Vec<_>>(),
            [Part::Wall, Part::Stone, Part::Parapet]
        );
        assert_eq!(
            materials[&Part::Wall],
            MaterialCall {
                material: Material::Travertine,
                text: facing.into(),
                pages: vec![16, 18],
            }
        );
        assert_eq!(materials[&Part::Stone].material, Material::RoughStone);
        assert_eq!(materials[&Part::Parapet].pages, [19]);
        assert_eq!(
            warnings,
            [
                "walls: the callouts name travertine (pages 16, 18) and plaster (page 19); travertine is kept"
            ]
        );
    }
}
