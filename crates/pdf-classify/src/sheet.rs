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

use pdf_geometry::model::{PageContent, Point, TextItem};

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
            // The stem, not one inflection: the same sheet is titled
            // `իրադրական հատակագիծ` in one office and `իրադրության
            // հատակագիծ` in the next, and the second used to fall through
            // to the generic floor-plan keyword below.
            "իրադր",
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

fn millimetre_level_regex() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?:^|[^\d.,])([+±\-−–]?)\s?(\d{3,5})\s*նիշ").expect("static regex")
    })
}

/// The level a sheet's NAME gives: a level mark (`-3,960`), or a whole number
/// of millimetres the name calls a mark (`7920 նիշում` → 7.92), which is how
/// one corpus set titles the storey its other titles write as `-3,960`.
fn title_level(title: &str) -> Option<f64> {
    levels_in(title).next().or_else(|| {
        let caps = millimetre_level_regex().captures(title)?;
        let millimetres: f64 = caps[2].parse().ok()?;
        let negative = matches!(&caps[1], "-" | "−" | "–");
        Some(if negative { -millimetres } else { millimetres } / 1000.0)
    })
}

/// A line of text as printed: items on one baseline, left to right.
struct Line {
    y: f64,
    /// The left edge: the first item's origin.
    x: f64,
    size: f64,
    text: String,
}

/// Groups horizontal text items into lines by baseline.
///
/// A run placed further off the page than the page is across is not part of
/// the sheet's text: one corpus plan carries a stray `0250` thirty pages to the
/// right of its title, on the title's baseline, where it joined the title line.
///
/// With `same_size`, a run joins a line only in that line's type size: a wrapped
/// title's rows then stay apart from the grid numbers and section marks a
/// plan prints at nearly the same baselines in smaller type.
fn lines(page: &PageContent, same_size: bool) -> Vec<Line> {
    let page_box = page.media_box;
    let reach = page_box.width().hypot(page_box.height());
    let on_page = |t: &TextItem| {
        page_box.width() <= 0.0
            || (t.origin.x >= page_box.min_x - reach
                && t.origin.x <= page_box.max_x + reach
                && t.origin.y >= page_box.min_y - reach
                && t.origin.y <= page_box.max_y + reach)
    };
    let mut items: Vec<&TextItem> = page
        .texts
        .iter()
        .filter(|t| t.angle.abs() < 0.05 && !t.text.trim().is_empty() && on_page(t))
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
            Some((y, size, members))
                if (item.origin.y - *y).abs() <= 0.4 * size.max(item.size)
                    && (!same_size || (item.size - *size).abs() <= 0.05 * size.max(item.size)) =>
            {
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
            let x = members.first().map_or(0.0, |t| t.origin.x);
            Line { y, x, size, text }
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
    // `-0,000` is the ground level, not a negative zero to print as `-0`.
    cleaned.parse::<f64>().ok().map(|level| level + 0.0)
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

/// A level mark as printed: its value and where the run that prints it sits,
/// for a reader that has to find the line the mark labels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlacedLevel {
    /// Metres.
    pub value: f64,
    /// The run's baseline origin, page units.
    pub origin: Point,
    /// The run's font size, page units.
    pub size: f64,
}

/// Every level mark on the page with the position of its run, in content
/// order; a level printed twice is listed twice.
pub fn placed_level_marks(page: &PageContent) -> Vec<PlacedLevel> {
    page.texts
        .iter()
        .filter(|item| item.angle.abs() < 0.05)
        .flat_map(|item| {
            levels_in(&item.text)
                .filter(|level| level.abs() < 100.0)
                .map(move |value| PlacedLevel {
                    value,
                    origin: item.origin,
                    size: item.size,
                })
        })
        .collect()
}

/// The role a title names, by keyword.
pub fn role_of(title: &str) -> Option<SheetRole> {
    let lower = title.to_lowercase();
    ROLE_RULES
        .iter()
        .find(|(_, keywords)| keywords.iter().any(|k| lower.contains(k)))
        .map(|(role, _)| *role)
}

/// A sheet name printed as a heading over the drawing rather than in a
/// title block wraps no further than this; past it the line is prose.
const MAX_HEADING_CHARS: usize = 120;

/// Words that turn the generic "plan" into a *storey* plan.
///
/// `հատակագիծ` on its own is Armenian for any plan at all — the parcel's,
/// the site's, the construction-organisation one — so the generic keyword
/// in [`ROLE_RULES`] names a floor plan only for a heading that also names
/// a storey or the level it is drawn at.
const STOREY_WORDS: &[&str] = &[
    "հարկ",
    "նկուղ",
    "ձեղնահարկ",
    "этаж",
    "подвал",
    "floor",
    "storey",
    "story",
    "basement",
];

/// Words that make a heading a *plan* of something rather than a mention of
/// it: `ՏԱՆԻՔԻ ՀԱՏԱԿԱԳԻԾ` is the roof plan, a bare `ՏԱՆԻՔ` on an elevation
/// is a note about the roofing material.
const PLAN_WORDS: &[&str] = &["հատակագ", "plan", "план"];

/// How many DIFFERENT sheet kinds one page may name before it is the sheet
/// list rather than a sheet. The explanatory note prints every drawing's
/// name in a table; three kinds in one place is a table of contents and
/// never a drawing's own heading.
const SHEET_LIST_ROLES: usize = 3;

/// Sheet kinds that name a drawing, as opposed to describing the set.
fn names_a_drawing(role: SheetRole) -> bool {
    !matches!(role, SheetRole::SheetIndex | SheetRole::Views3d)
}

/// The same text with every space taken out.
fn despace(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Whether `haystack`, already despaced, contains `keyword`.
///
/// Only 13 of the 43 keywords in [`ROLE_RULES`] contain a space at all, and
/// the rest are their own despaced form -- so the allocation is skipped for
/// them rather than repeated for every keyword on every candidate line.
fn despaced_contains(haystack: &str, keyword: &str) -> bool {
    if keyword.contains(' ') {
        haystack.contains(&despace(keyword))
    } else {
        haystack.contains(keyword)
    }
}

/// Whether a line reads as a sentence rather than a name.
///
/// The Armenian full stop closes a sentence and never sits inside a sheet
/// name; the one place it appears in a heading is a scale (`Մ1:75`, written
/// `Մ1։75` with the same character), where no space follows it. The
/// explanatory note's prose — `... իրականացվել է 1-5 առանցքներով
/// ճակատից։ Այդ ...` — is the reason a drawing-bearing note page could
/// otherwise be read as an elevation.
fn is_prose(line: &str) -> bool {
    line.contains("։ ")
}

/// A heading that is only a storey's name wraps no further than this; past
/// it the line has joined its neighbours and is no longer a name.
const MAX_STOREY_NAME_CHARS: usize = 24;

/// The nouns a storey is called by, as a heading ends on one.
const STOREY_NOUNS: &[&str] = &["հարկ", "հարկը", "նկուղ", "ձեղնահարկ", "этаж", "подвал"];

/// Whether the whole line names a storey and says nothing else.
///
/// DELIBERATELY NARROW. `հարկ` runs through a dossier's prose — `2-րդ
/// հարկում՝ միջանցք`, `ԱՌԱՋԻՆ ՀԱՐԿԻ ԿԱՌՈՒՑԱՊԱՏՄԱՆ ՄԱԿԵՐԵՍ` — and labels a
/// floor level down the side of an elevation. Only a short line that ENDS on
/// the noun is the sheet naming itself, which is why the ordinal in front of
/// it is not spelled out here: whatever precedes, the line has to stop there.
/// Sheets that also carry a keyword from [`ROLE_RULES`] never reach this,
/// since that match is tried first and outranks a floor plan.
fn names_a_storey(line: &str) -> bool {
    let text = line.trim();
    if !(4..=MAX_STOREY_NAME_CHARS).contains(&text.chars().count()) {
        return false;
    }
    let lower = text.to_lowercase();
    let tail = lower.trim_end_matches(|c: char| c.is_whitespace() || c == ':' || c == '.');
    STOREY_NOUNS.iter().any(|noun| tail.ends_with(noun))
}

/// The role a free-standing heading names.
///
/// STRICTER THAN [`role_of`], and deliberately so. A title block's row is
/// the office SAYING what the sheet is, so a loose keyword match on it is
/// safe. A heading is picked out of the page's ordinary text, where the
/// same words occur as annotations, so each loose keyword is required to
/// appear in the company that makes it a sheet name.
fn heading_role(line: &str) -> Option<SheetRole> {
    if is_prose(line) {
        return None;
    }
    // A CAD exporter that tracks letters individually prints the spacing as
    // real spaces — `ՀԱ ՏԱ ԿԱԳԻԾ` for `ՀԱՏԱԿԱԳԻԾ` — so every keyword here is
    // matched with the whitespace taken out of both sides.
    let lower = despace(&line.to_lowercase());
    let says = |words: &[&str]| words.iter().any(|w| despaced_contains(&lower, w));
    let role = match ROLE_RULES
        .iter()
        .find(|(_, keywords)| says(keywords))
        .map(|(role, _)| *role)
    {
        Some(role) => role,
        // A sheet may name only the storey it draws. `ԱՌԱՋԻՆ ՀԱՐԿ` — first
        // storey — is the whole heading, with no word for "plan" anywhere on
        // the sheet, and one corpus dossier titles all three of its plans
        // that way.
        None if names_a_storey(line) => SheetRole::FloorPlan,
        None => return None,
    };
    match role {
        // `ՏԱՆԻՔԻ ԾԱԼՔԱՎՈՐ ԹԻԹԵՂ` (folded roof sheeting) and `ՊԱՐԻՍՊ`
        // (fence) label materials and objects on sheets that are neither.
        SheetRole::RoofPlan | SheetRole::Fence if !says(PLAN_WORDS) => None,
        // Reached only through the generic `հատակագիծ`; every specific
        // floor-plan keyword in ROLE_RULES already names the storey.
        SheetRole::FloorPlan
            if !says(&["հարկի հատակագիծ", "floor plan", "план этажа"])
                && !says(STOREY_WORDS)
                && !says(&["նիշ", "отм"])
                // A level mark carries its own sign and separator, so it
                // reads the same with the spacing gone -- unless a number
                // printed after it on the same baseline runs into it once
                // the spaces are out (`-0,000 0250` → `-0,0000250`), so the
                // spaced line is asked too.
                && levels_in(&lower).next().is_none()
                && levels_in(&line.to_lowercase()).next().is_none() =>
        {
            None
        }
        role => Some(role),
    }
}

/// Reads the sheet name where the set prints no title block at all.
///
/// WHY THIS EXISTS. [`sheet_title`] reads the name out of a labelled
/// title-block row (`Անվանում :`), which is what a large-format CAD sheet
/// carries. Most permit dossiers are not drawn that way: the sheet prints
/// its name as a plain heading over the drawing — `ԱՌԱՋԻՆ ՀԱՐԿԻ
/// ՀԱՏԱԿԱԳԻԾ Մ1։75` — and nothing labels it. Across this project's corpus
/// exactly one document in fourteen has a title block, so reading only
/// labelled rows left thirteen sets with no sheet roles at all, and
/// `pdf_model`'s reconstruction refused them for want of a floor plan.
///
/// It is a SECOND reading, never a competing one: the caller tries
/// [`sheet_title`] first and comes here only when the page labels nothing,
/// so a document that does print title blocks is decided exactly as before.
///
/// The page must already be known to carry line work. A heading is only
/// evidence about a drawing, and the same words run through the prose of
/// the application letter.
pub fn sheet_heading(page: &PageContent) -> Option<SheetTitle> {
    let lines = lines(page, false);
    // `lines` has already collapsed each line's whitespace and trimmed it, so
    // `text` is the normalised form throughout this function.
    let named: Vec<(String, SheetRole)> = lines
        .iter()
        .filter(|line| (3..=MAX_HEADING_CHARS).contains(&line.text.chars().count()))
        .filter_map(|line| heading_role(&line.text).map(|role| (line.text.clone(), role)))
        .filter(|(_, role)| names_a_drawing(*role))
        .collect();
    // A NAME WRAPPED OVER STACKED LINES -- `ԵՐՐՈՐԴ ՀԱՐԿԻ` / `ՀԱՏԱԿԱԳԻԾ 7920` /
    // `ՆԻՇՈՒՄ` -- names its sheet only read as one. Tried only where no line
    // does on its own, so a page whose heading already reads is decided as
    // before.
    let named = if named.is_empty() {
        stacked_headings(&self::lines(page, true))
            .into_iter()
            .filter_map(|text| heading_role(&text).map(|role| (text, role)))
            .filter(|(_, role)| names_a_drawing(*role))
            .collect()
    } else {
        named
    };

    // How many DIFFERENT kinds of drawing the page names: a sheet index lists
    // several, a sheet names its own. Counted by scanning the handful of
    // matches rather than sorting, since `SheetRole` has no `Ord` and the list
    // is never longer than the page has headings.
    let mut kinds: Vec<SheetRole> = Vec::new();
    for (_, role) in &named {
        if !kinds.contains(role) {
            kinds.push(*role);
        }
    }
    if kinds.len() >= SHEET_LIST_ROLES {
        return None;
    }

    // ROLE_RULES is ordered by precedence, so the earliest rule that any
    // heading on the page matches is the page's kind — the same order that
    // decides a title block naming a fence elevation.
    let (title, role) = named.into_iter().min_by_key(|(_, role)| {
        ROLE_RULES
            .iter()
            .position(|(r, _)| r == role)
            .unwrap_or(usize::MAX)
    })?;

    let level_m = title_level(&title);
    Some(SheetTitle {
        role: Some(role),
        level_m,
        title,
    })
}

/// Lines stacked as one wrapped name: the same left edge and type size, each
/// baseline within a line and a half of the one above. From each line, the
/// longest such stack of up to three, joined, within [`MAX_HEADING_CHARS`]:
/// the whole wrapped name, so a level its last line names (`… 7920` /
/// `ՆԻՇՈՒՄ`) is read with it.
fn stacked_headings(lines: &[Line]) -> Vec<String> {
    let stacked = |a: &Line, b: &Line| {
        let size = a.size.max(b.size);
        (a.x - b.x).abs() <= 0.5 * size
            && (a.size - b.size).abs() <= 0.1 * size
            && a.y > b.y
            && a.y - b.y <= 1.8 * size
    };
    let mut out = Vec::new();
    for (i, first) in lines.iter().enumerate() {
        let mut text = first.text.clone();
        let mut last = first;
        let mut joined = 0;
        let mut longest = None;
        // `lines` runs top to bottom, so the next stacked line is the first
        // below `last` that lines up with it; anything further down than a
        // line and a half ends the name.
        for next in &lines[i + 1..] {
            if joined == 2 || next.y < last.y - 1.8 * last.size.max(next.size) {
                break;
            }
            if !stacked(last, next) {
                continue;
            }
            text = format!("{text} {}", next.text);
            last = next;
            joined += 1;
            if text.chars().count() <= MAX_HEADING_CHARS {
                longest = Some(text.clone());
            }
        }
        out.extend(longest);
    }
    out
}

/// Reads the sheet name off the title block, when the page prints one.
pub fn sheet_title(page: &PageContent) -> Option<SheetTitle> {
    let lines = lines(page, false);
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
    let level_m = title_level(&title);
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
    fn a_sheet_with_no_title_block_is_read_from_its_heading() {
        // Thirteen of the fourteen corpus dossiers print no `Անվանում :`
        // row at all; the sheet names itself over the drawing.
        let page = page(vec![
            text("ԱՌԱՋԻՆ ՀԱՐԿԻ ՀԱՏԱԿԱԳԻԾ ±0.000 ՆԻՇԻ ՎՐԱ Մ1։50", 267.5, 288.2),
            text("2 100", 268.5, 361.7),
        ]);
        assert!(sheet_title(&page).is_none(), "no labelled row to read");
        let heading = sheet_heading(&page).expect("a heading");
        assert_eq!(heading.role, Some(SheetRole::FloorPlan));
        assert_eq!(heading.level_m, Some(0.0));
    }

    #[test]
    fn a_heading_needs_more_than_the_generic_word_for_plan() {
        // `հատակագիծ` is Armenian for any plan; only a storey or a level
        // makes it a floor plan. Both of these are site sheets.
        for name in ["հողամասի հատակագիծը", "ՇԻՆ. ԿԱԶՄԱԿԵՐՊՄԱՆ ՀԱՏԱԿԱԳԻԾ"]
        {
            let page = page(vec![text(name, 100.0, 700.0)]);
            assert_eq!(sheet_heading(&page).and_then(|t| t.role), None, "{name}");
        }
        // ...and an inflected situation plan is a situation plan, not the
        // generic keyword falling through.
        let page = page(vec![text("ԻՐԱԴՐՈՒԹՅԱՆ ՀԱՏԱԿԱԳԻԾ Մ 1:2000", 100.0, 700.0)]);
        assert_eq!(
            sheet_heading(&page).and_then(|t| t.role),
            Some(SheetRole::SituationPlan)
        );
    }

    #[test]
    fn a_roof_named_in_an_annotation_is_not_a_roof_plan() {
        // `ՏԱՆԻՔ` labels the roofing material on an elevation; the roof
        // PLAN says so. Without this the elevations of one dossier all
        // came back as its roof plan.
        let page = page(vec![
            text("ՏԱՆԻՔԻ ԾԱԼՔԱՎՈՐ ԹԻԹԵՂ", 100.0, 600.0),
            text("ԿՏՐՎԱԾՔ 1-1 Մ 1։100", 100.0, 700.0),
        ]);
        assert_eq!(
            sheet_heading(&page).and_then(|t| t.role),
            Some(SheetRole::Section)
        );
    }

    /// A sheet may name the storey and never the word "plan".
    #[test]
    fn a_sheet_that_names_only_its_storey_is_a_floor_plan() {
        for name in ["ԱՌԱՋԻՆ ՀԱՐԿ", "ՆԿՈՒՂԱՅԻՆ ՀԱՐԿ", "ԵՐԿՐՈՐԴ ՀԱՐԿ", "1-ԻՆ ՀԱՐԿ"]
        {
            let page = page(vec![text(name, 267.5, 288.2)]);
            assert_eq!(
                sheet_heading(&page).and_then(|t| t.role),
                Some(SheetRole::FloorPlan),
                "{name}"
            );
        }
    }

    /// ...but only when the line stops there. `հարկ` runs through a
    /// dossier's prose and labels a level down the side of an elevation.
    #[test]
    fn a_storey_named_mid_sentence_is_not_a_sheet_name() {
        for line in [
            "2-րդ հարկում՝ միջանցք, սանհանգույց, լոգախցիկ",
            "ԱՌԱՋԻՆ ՀԱՐԿԻ ԿԱՌՈՒՑԱՊԱՏՄԱՆ ՄԱԿԵՐԵՍ",
            "ԿԻՍԱՆԿՈՒՂԱՅԻՆ ՀԱՐԿԻ ՎԵՐԱԿԱՌՈՒՑՎԱԾ ՀԱՏՎԱԾԻ ՄԱԿԵՐԵՍԸ",
        ] {
            let page = page(vec![text(line, 100.0, 700.0)]);
            assert_eq!(sheet_heading(&page).and_then(|t| t.role), None, "{line}");
        }
        // And a sheet that names its kind outranks the storey it draws.
        let page = page(vec![
            text("ՃԱԿԱՏ ԸՍՏ Ա-Բ ԱՌԱՆՑՔՆԵՐԻ", 100.0, 760.0),
            text("1-ԻՆ ՀԱՐԿ", 100.0, 700.0),
        ]);
        assert_eq!(
            sheet_heading(&page).and_then(|t| t.role),
            Some(SheetRole::Elevation)
        );
    }

    #[test]
    fn the_sheet_list_is_not_every_sheet_it_names() {
        let page = page(vec![
            text("ԳԼԽԱՎՈՐ ՀԱՏԱԿԱԳԻԾ Մ1։500", 100.0, 760.0),
            text("ԱՌԱՋԻՆ ՀԱՐԿԻ ՀԱՏԱԿԱԳԻԾ ±0.000", 100.0, 740.0),
            text("ՃԱԿԱՏ 1-5 ԱՌԱՆՑՔՆԵՐՈՎ Մ1։50", 100.0, 720.0),
        ]);
        assert!(sheet_heading(&page).is_none());
    }

    #[test]
    fn a_sentence_that_mentions_a_facade_is_not_a_facade_sheet() {
        let page = page(vec![text(
            "Շինության գլխավոր մուտքը իրականացվել է 1-5 առանցքներով ճակատից։ Այդ",
            100.0,
            700.0,
        )]);
        assert!(sheet_heading(&page).is_none());
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
    fn placed_level_marks_keep_each_run_and_its_position() {
        let page = page(vec![
            text("+7,200 +6,700", 994.0, 570.4),
            text("+3,600", 994.0, 415.0),
            text("+3,600", 10.0, 480.0),
            text("+0,100 հատակի նիշ", 994.0, 308.6),
            text("14 800", 10.0, 200.0),
        ]);
        let marks = placed_level_marks(&page);
        let values: Vec<f64> = marks.iter().map(|m| m.value).collect();
        assert_eq!(
            values,
            vec![7.2, 6.7, 3.6, 3.6, 0.1],
            "every mark, in order"
        );
        assert_eq!(marks[2].origin, Point::new(994.0, 415.0));
        assert_eq!(
            marks[4].origin,
            Point::new(994.0, 308.6),
            "a mark with a label after it"
        );
        assert_eq!(marks[0].size, 11.66);
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

    #[test]
    fn a_level_mark_is_read_when_a_stray_number_runs_into_it_despaced() {
        let content = page(vec![
            text("հատակագիծ", 896.0, 66.0),
            text("  -0,000", 1018.0, 66.0),
            text("0250", 1500.0, 66.0),
        ]);
        let heading = sheet_heading(&content).expect("the plan's heading");
        assert_eq!(heading.role, Some(SheetRole::FloorPlan));
        assert_eq!(heading.level_m, Some(0.0));
    }

    #[test]
    fn a_run_far_off_the_page_does_not_join_the_title_line() {
        let content = page(vec![
            text("հատակագիծ", 896.0, 66.0),
            text("  -3,960", 1018.0, 66.0),
            text("0250", 33_363.0, 66.0),
        ]);
        let heading = sheet_heading(&content).expect("the plan's heading");
        assert_eq!(heading.title, "հատակագիծ -3,960");
    }

    #[test]
    fn a_name_wrapped_over_stacked_lines_is_read_as_one_with_its_millimetre_level() {
        let content = page(vec![
            text("Երրորդ", 896.0, 96.0),
            text("հարկի", 976.0, 96.0),
            text("հատակագիծ", 896.0, 81.0),
            text("  7920", 1018.0, 81.0),
            text("նիշում/չափադրում", 896.0, 66.0),
        ]);
        let heading = sheet_heading(&content).expect("the wrapped name");
        assert_eq!(heading.role, Some(SheetRole::FloorPlan));
        assert_eq!(heading.level_m, Some(7.92));
        assert!(heading.title.starts_with("Երրորդ հարկի հատակագիծ 7920"));
    }

    #[test]
    fn a_millimetre_level_needs_the_word_for_a_mark() {
        assert_eq!(title_level("հատակագիծ 7920 նիշում"), Some(7.92));
        assert_eq!(title_level("հատակագիծ -3960 նիշում"), Some(-3.96));
        assert_eq!(title_level("հատակագիծ 7920"), None);
        assert_eq!(title_level("+3,60 նիշով հարկի հատակագիծ"), Some(3.6));
        assert_eq!(parse_level("-0,000"), Some(0.0));
        assert!(parse_level("-0,000").is_some_and(f64::is_sign_positive));
    }
}
