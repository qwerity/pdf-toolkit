//! What a drawing sheet's title block says about it.
//!
//! A CAD title block prints the sheet's name after a fixed label
//! (`Անվանում :`, `Наименование:`, `Title:`), and the name says what the
//! sheet is in the office's own words — `+0,00 նիշով հարկի հատակագիծ` is a
//! floor plan at ±0.000, `Ճակատ Ա-Բ առանցքով` an elevation, `Տանիքի
//! հատակագիծ` the roof plan. Reading that line settles the sheet's role
//! where line density and path counts cannot tell a plan from a facade, and
//! it does so from the text layer alone. Level marks (`+3,600`) are read the
//! same way: a floor plan names one level in its title, an elevation prints
//! several down its side.

use regex::Regex;
use serde::{Deserialize, Serialize};

use pdf_geometry::model::{PageContent, TextItem};

/// The kind of sheet a title names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SheetRole {
    FloorPlan,
    RoofPlan,
    Elevation,
    Section,
    /// A general/master plan of the site.
    SitePlan,
    /// A situational or location plan over the surroundings.
    SituationPlan,
    Fence,
    /// Renders and perspective views.
    Views3d,
    /// The explanatory note or the list of sheets.
    SheetIndex,
}

/// The title-block reading of one sheet.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SheetTitle {
    /// The sheet name as printed, whitespace collapsed.
    pub title: String,
    /// What the name says the sheet is, when it says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<SheetRole>,
    /// A level named in the title (`+3,60 նիշով` → 3.6), metres.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level_m: Option<f64>,
}

/// The labels a title block prints before the sheet name, lower-cased.
const TITLE_LABELS: &[&str] = &[
    "անվանում",
    "наименование",
    "title",
    "drawing name",
    "sheet name",
];

/// Labels of the other title-block rows; a line starting with one is not
/// the continuation of the title.
const OTHER_LABELS: &[&str] = &[
    "մասշտաբ",
    "փուլ",
    "масштаб",
    "стадия",
    "scale",
    "stage",
    "date",
    "sheet",
];

/// Keyword rules, first match wins; the order encodes precedence (a fence
/// elevation is a fence sheet, a roof plan is not a floor plan).
const ROLE_RULES: &[(SheetRole, &[&str])] = &[
    (
        SheetRole::Fence,
        &["ցանկապատ", "պարիսպ", "fence", "ограждени", "забор"],
    ),
    (
        SheetRole::RoofPlan,
        &["տանիք", "roof plan", "кровл", "крыш"],
    ),
    (
        SheetRole::SituationPlan,
        &[
            "իրադրական",
            "իրավիճակ",
            "situation",
            "location plan",
            "ситуац",
        ],
    ),
    (
        SheetRole::SitePlan,
        &[
            "գլխավոր հատակագիծ",
            "general plan",
            "site plan",
            "master plan",
            "генплан",
            "генеральный план",
        ],
    ),
    (SheetRole::Section, &["կտրվածք", "section", "разрез"]),
    (
        SheetRole::Elevation,
        &["ճակատ", "elevation", "facade", "фасад"],
    ),
    (
        SheetRole::Views3d,
        &["եռաչափ", "3d", "визуализац", "перспектив", "render"],
    ),
    (
        SheetRole::SheetIndex,
        &[
            "անվանացանկ",
            "բացատրագիր",
            "содержание",
            "sheet list",
            "sheet index",
            "table of contents",
        ],
    ),
    (
        SheetRole::FloorPlan,
        &[
            "հարկի հատակագիծ",
            "floor plan",
            "план этажа",
            "этажа",
            "հատակագիծ",
        ],
    ),
];

fn level_regex() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[+±\-−–]\s?\d{1,2}[,.]\d{2,3}").expect("static regex"))
}

/// The level marks in one run of text: a signed number with two or three
/// decimals that is not part of a longer number (`14 800-3,50` is a
/// dimension, `+3,600` a level).
fn levels_in(text: &str) -> impl Iterator<Item = f64> + '_ {
    level_regex().find_iter(text).filter_map(move |m| {
        let before = text[..m.start()].chars().next_back();
        let after = text[m.end()..].chars().next();
        let bounded = !before.is_some_and(|c| c.is_ascii_digit() || c == '.' || c == ',')
            && !after.is_some_and(|c| c.is_ascii_digit());
        bounded.then(|| parse_level(m.as_str())).flatten()
    })
}

/// A line of text as printed: items on one baseline, left to right.
struct Line {
    y: f64,
    size: f64,
    text: String,
}

/// Groups horizontal text items into lines by baseline.
fn lines(page: &PageContent) -> Vec<Line> {
    let mut items: Vec<&TextItem> = page
        .texts
        .iter()
        .filter(|t| t.angle.abs() < 0.05 && !t.text.trim().is_empty())
        .collect();
    items.sort_by(|a, b| {
        b.origin
            .y
            .total_cmp(&a.origin.y)
            .then_with(|| a.origin.x.total_cmp(&b.origin.x))
    });
    let mut lines: Vec<(f64, f64, Vec<&TextItem>)> = Vec::new();
    for item in items {
        match lines.last_mut() {
            Some((y, size, members)) if (item.origin.y - *y).abs() <= 0.4 * size.max(item.size) => {
                members.push(item);
            }
            _ => lines.push((item.origin.y, item.size, vec![item])),
        }
    }
    lines
        .into_iter()
        .map(|(y, size, mut members)| {
            members.sort_by(|a, b| a.origin.x.total_cmp(&b.origin.x));
            let text = members
                .iter()
                .map(|t| t.text.trim())
                .collect::<Vec<_>>()
                .join(" ")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            Line { y, size, text }
        })
        .collect()
}

/// A level mark such as `+3,600`, `±0.000` or `-0,35`, in metres.
pub fn parse_level(text: &str) -> Option<f64> {
    let cleaned: String = text
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| match c {
            ',' => '.',
            '−' | '–' => '-',
            '±' => '+',
            c => c,
        })
        .collect();
    cleaned.parse::<f64>().ok()
}

/// Every distinct level mark printed on the page, ascending.
pub fn level_marks(page: &PageContent) -> Vec<f64> {
    let mut marks: Vec<f64> = Vec::new();
    for item in &page.texts {
        for level in levels_in(&item.text) {
            if level.abs() < 100.0 && !marks.iter().any(|m| (m - level).abs() < 0.001) {
                marks.push(level);
            }
        }
    }
    marks.sort_by(f64::total_cmp);
    marks
}

/// The role a title names, by keyword.
pub fn role_of(title: &str) -> Option<SheetRole> {
    let lower = title.to_lowercase();
    ROLE_RULES
        .iter()
        .find(|(_, keywords)| keywords.iter().any(|k| lower.contains(k)))
        .map(|(role, _)| *role)
}

/// Reads the sheet name off the title block, when the page prints one.
pub fn sheet_title(page: &PageContent) -> Option<SheetTitle> {
    let lines = lines(page);
    let index = lines.iter().position(|line| {
        let lower = line.text.to_lowercase();
        TITLE_LABELS.iter().any(|label| {
            lower.starts_with(label)
                || lower.contains(&format!("{label} :"))
                || lower.contains(&format!("{label}:"))
        })
    })?;
    let line = &lines[index];
    let mut title = match line.text.find(':') {
        Some(colon) => line.text[colon + 1..].trim().to_string(),
        None => {
            let lower = line.text.to_lowercase();
            let label = TITLE_LABELS
                .iter()
                .find(|l| lower.starts_with(*l))
                .copied()
                .unwrap_or("");
            line.text[label.len().min(line.text.len())..]
                .trim()
                .to_string()
        }
    };
    // A name too long for its cell wraps onto the next line of the block.
    if let Some(next) = lines.get(index + 1) {
        let lower = next.text.to_lowercase();
        let is_row_label = OTHER_LABELS.iter().any(|l| lower.starts_with(l))
            || TITLE_LABELS.iter().any(|l| lower.starts_with(l));
        if !is_row_label && line.y - next.y < 1.8 * line.size.max(next.size) {
            title = format!("{title} {}", next.text.trim());
        }
    }
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() {
        return None;
    }
    let level_m = levels_in(&title).next();
    Some(SheetTitle {
        role: role_of(&title),
        level_m,
        title,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdf_geometry::model::{BBox, Point, Rgb};

    fn text(t: &str, x: f64, y: f64) -> TextItem {
        TextItem {
            text: t.into(),
            origin: Point::new(x, y),
            size: 11.66,
            angle: 0.0,
            color: Rgb::BLACK,
        }
    }

    fn page(texts: Vec<TextItem>) -> PageContent {
        PageContent {
            page_number: 1,
            media_box: BBox {
                min_x: 0.0,
                min_y: 0.0,
                max_x: 1190.0,
                max_y: 842.0,
            },
            paths: Vec::new(),
            texts,
            image_count: 0,
            image_coverage: 0.0,
        }
    }

    #[test]
    fn a_title_split_across_runs_is_read_as_one_line() {
        let page = page(vec![
            text("Փուլ :էսքիզային", 72.5, 805.95),
            text("Անվանում", 72.5, 791.86),
            text(": +0,00 նիշով  հարկի ", 123.15, 791.86),
            text("հատակագիծ", 254.27, 791.86),
            text("Մասշտաբ", 72.5, 777.77),
            text(": 1:75, ", 122.05, 777.77),
        ]);
        let title = sheet_title(&page).expect("a title");
        assert_eq!(title.title, "+0,00 նիշով հարկի հատակագիծ");
        assert_eq!(title.role, Some(SheetRole::FloorPlan));
        assert_eq!(title.level_m, Some(0.0));
    }

    #[test]
    fn roles_follow_their_precedence() {
        assert_eq!(role_of("Ցանկապատի ճակատ"), Some(SheetRole::Fence));
        assert_eq!(
            role_of("+7,200 նիշով տանիքի հատակագիծ"),
            Some(SheetRole::RoofPlan)
        );
        assert_eq!(role_of("Ճակատ 1-3 առանցքով"), Some(SheetRole::Elevation));
        assert_eq!(role_of("Կտրվածք 1-1 առանցքով"), Some(SheetRole::Section));
        assert_eq!(
            role_of("Գլխավոր հատակագիծ/քանդվող շնություն"),
            Some(SheetRole::SitePlan)
        );
        assert_eq!(
            role_of("Իրադրական հատակագիծ"),
            Some(SheetRole::SituationPlan)
        );
        assert_eq!(role_of("Եռաչափ տեսքեր"), Some(SheetRole::Views3d));
        assert_eq!(
            role_of("Բացատրագիր, անվանացանկ"),
            Some(SheetRole::SheetIndex)
        );
        assert_eq!(role_of("План 2-го этажа"), Some(SheetRole::FloorPlan));
        assert_eq!(role_of("Ground floor plan"), Some(SheetRole::FloorPlan));
        assert_eq!(role_of("Node A"), None);
    }

    #[test]
    fn level_marks_are_distinct_and_sorted() {
        let page = page(vec![
            text("+7,750", 10.0, 700.0),
            text("+7,200 +6,700", 10.0, 650.0),
            text("+3,600", 10.0, 500.0),
            text("+3,600", 10.0, 480.0),
            text("±0,000", 10.0, 300.0),
            text("-0,350", 10.0, 280.0),
            text("14 800", 10.0, 200.0),
            text("0 0 6 3", 10.0, 190.0),
        ]);
        assert_eq!(level_marks(&page), vec![-0.35, 0.0, 3.6, 6.7, 7.2, 7.75]);
    }

    #[test]
    fn a_wrapped_title_takes_its_second_line_but_not_the_next_row() {
        let page = page(vec![
            text("Անվանում : Գլխավոր հատակագիծ", 72.5, 791.86),
            text("քանդվող շնություն", 123.0, 780.0),
            text("Մասշտաբ : 1:150", 72.5, 766.0),
        ]);
        let title = sheet_title(&page).expect("a title");
        assert_eq!(title.title, "Գլխավոր հատակագիծ քանդվող շնություն");
        assert_eq!(title.role, Some(SheetRole::SitePlan));
        assert!(title.level_m.is_none());
    }
}
