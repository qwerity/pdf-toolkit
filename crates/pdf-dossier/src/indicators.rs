//! The drawing set's technical-economic indicators table (ՏՏՑ), as printed.
//!
//! The table states the plot area, the built footprint and the green area —
//! the numerators and denominators of every coverage and greening ratio a
//! permit is judged by — each often with the percentage it makes of the
//! plot. [`crate::facts`] reads areas by the words just before a number on
//! any page; this reads the one table whose rows mean exactly those three
//! things, with the qualifiers that tell its rows apart:
//!
//! - «ԸՆԴՀԱՆՈՒՐ կառուցապատման մակերես» is every storey summed, not the
//!   footprint; a built area followed by «/ԱՆՋՐԱՆՑԻԿ … ՆԵՐԱՌՅԱԼ/» includes
//!   the paving. A plain row is the footprint; without one, the house's row
//!   plus the fence's row is.
//! - Some CAD exports set each capital as its own text object, so a label
//!   reads «ՀՈՂ\nԱ\nՄ\nԱՍԻ»: whitespace is allowed between any two letters.
//! - A legacy CAD font prints «քմ/» as «Թ» and «հա» as «fճ»; both are read
//!   as the units they stand for, right after a number and nowhere else.
//!
//! A row printed with two different values, or a table that two sheets
//! print differently, gives that field up rather than choosing: the caller
//! has another reader (a vision model) for what this cannot settle, and a
//! wrong figure is worse than none. Nothing here judges the figures against
//! each other; the printed percentage is reported so the caller can.

use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;

use crate::numbers::parse_number;

/// Square metres in a hectare.
const M2_PER_HA: f64 = 10_000.0;
/// How far past the heading the table's rows can reach, in characters.
const TABLE_SPAN: usize = 2000;
/// How far past a label its value can be, in characters.
const VALUE_REACH: usize = 60;

/// One figure as the table prints it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Figure {
    /// The area, square metres.
    pub m2: f64,
    /// The percentage printed beside it, when the table prints one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pct: Option<f64>,
}

/// The table's figures, read from the first page that prints it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Indicators {
    /// One-based page the table was read from.
    pub page: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plot: Option<Figure>,
    /// The footprint as the parts the table prints: one row, or the house's
    /// and the fence's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub footprint: Option<Vec<Figure>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub green: Option<Figure>,
}

/// A label that matches however the CAD font spaced its letters.
fn label(words: &str) -> String {
    words
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| regex::escape(&c.to_string()))
        .collect::<Vec<_>>()
        .join(r"\s*")
}

fn compile(pattern: &str) -> Regex {
    Regex::new(&format!("(?i){pattern}")).expect("valid regex")
}

static HEADING: LazyLock<Regex> = LazyLock::new(|| compile(&label("ցուցանիշ")));
static PLOT: LazyLock<Regex> = LazyLock::new(|| {
    compile(&format!(
        r"(?:{}|{})\s*(?:{}\s*)?{}",
        label("հողամասի"),
        label("հողատարածքի"),
        label("ընդհանուր"),
        label("մակերես")
    ))
});
static BUILT: LazyLock<Regex> = LazyLock::new(|| {
    compile(&format!(
        r"{}\s*{}",
        label("կառուցապատման"),
        label("մակերես")
    ))
});
static GREEN: LazyLock<Regex> = LazyLock::new(|| {
    compile(&format!(
        r"{}\s*{}",
        label("կանաչապատման"),
        label("մակերես")
    ))
});
/// The number after a label: the label's own case ending («մակերեսը») and
/// the punctuation between a row's name and its value («--», «՝», «:») are
/// skipped; then the unit, mis-encodings included, and the percentage.
static VALUE: LazyLock<Regex> = LazyLock::new(|| {
    compile(
        r"^[\s\p{L}]{0,3}?[\s\-–—:՝`=|*]*(\d+(?:[.,]\d+)?)\s*(հա|ha|fճ|քմ|ք\.\s*մ|մ²|մ2|m2|м2|Թ)?[\s/(]*(?:(\d+(?:[.,]\d+)?)\s*%)?",
    )
});
static HECTARES: LazyLock<Regex> = LazyLock::new(|| compile(r"^(?:հա|ha|fճ)$"));
static WITH_IMPERVIOUS: LazyLock<Regex> = LazyLock::new(|| {
    compile(&format!(
        r"{}[^/]{{0,60}}{}",
        label("անջրանցիկ"),
        label("ներառյալ")
    ))
});
static FENCE_BEFORE: LazyLock<Regex> = LazyLock::new(|| compile(r"(?:պարսպի|պարիսպի|ցանկապատի)$"));
static HOUSE_BEFORE: LazyLock<Regex> = LazyLock::new(|| compile(r"(?:տան|շենքի|շինության)$"));

/// `s` from byte `start`, at most `chars` characters.
fn take_chars(s: &str, start: usize, chars: usize) -> &str {
    let rest = &s[start..];
    let end = rest
        .char_indices()
        .nth(chars)
        .map_or(rest.len(), |(i, _)| i);
    &rest[..end]
}

/// The `chars` characters before byte `end`.
fn chars_before(s: &str, end: usize, chars: usize) -> &str {
    let head = &s[..end];
    let start = head
        .char_indices()
        .rev()
        .nth(chars.saturating_sub(1))
        .map_or(0, |(i, _)| i);
    &head[start..]
}

struct Row {
    figure: Figure,
    /// The words just before the label, whitespace removed, lower-cased.
    before: String,
    /// The text just after the value, for a qualifier printed after it.
    after: String,
}

fn rows(table: &str, label: &Regex, hectares_allowed: bool) -> Vec<Row> {
    let mut out = Vec::new();
    for m in label.find_iter(table) {
        let tail = take_chars(table, m.end(), VALUE_REACH);
        let Some(v) = VALUE.captures(tail) else {
            continue;
        };
        let (Some(number), whole) = (v.get(1), v.get(0).expect("group 0")) else {
            continue;
        };
        let Some(mut m2) = parse_number(number.as_str()) else {
            continue;
        };
        let unit = v.get(2).map_or("", |u| u.as_str());
        let hectares = HECTARES.is_match(unit)
            || (unit.is_empty() && m2 > 0.0 && m2 < 1.0 && number.as_str().contains(['.', ',']));
        if hectares {
            if !hectares_allowed {
                continue;
            }
            m2 = (m2 * M2_PER_HA * 100.0).round() / 100.0;
        }
        if m2 <= 0.0 {
            continue;
        }
        out.push(Row {
            figure: Figure {
                m2,
                pct: v.get(3).and_then(|p| parse_number(p.as_str())),
            },
            before: chars_before(table, m.start(), 40)
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect::<String>()
                .to_lowercase(),
            after: take_chars(tail, whole.end(), 80).to_string(),
        });
    }
    out
}

/// One value, or none when the table prints the row with two different values.
fn single(rows: &[&Row]) -> Option<Figure> {
    let first = rows.first()?;
    rows.iter()
        .all(|r| r.figure.m2 == first.figure.m2)
        .then(|| first.figure.clone())
}

fn footprint(table: &str) -> Option<Vec<Figure>> {
    let built: Vec<Row> = rows(table, &BUILT, false)
        .into_iter()
        .filter(|r| !r.before.ends_with("ընդհանուր") && !WITH_IMPERVIOUS.is_match(&r.after))
        .collect();
    let fence: Vec<&Row> = built
        .iter()
        .filter(|r| FENCE_BEFORE.is_match(&r.before))
        .collect();
    let house: Vec<&Row> = built
        .iter()
        .filter(|r| HOUSE_BEFORE.is_match(&r.before))
        .collect();
    let plain: Vec<&Row> = built
        .iter()
        .filter(|r| !FENCE_BEFORE.is_match(&r.before) && !HOUSE_BEFORE.is_match(&r.before))
        .collect();
    if !plain.is_empty() {
        return single(&plain).map(|whole| vec![whole]);
    }
    let main = single(&house)?;
    Some(match single(&fence) {
        Some(extra) => vec![main, extra],
        None => vec![main],
    })
}

/// The table on one page, from its heading on. None when the page has none.
fn read_page(page: usize, text: &str) -> Option<Indicators> {
    let heading = HEADING.find(text)?;
    let table = take_chars(text, heading.start(), TABLE_SPAN);
    let all = |label: &Regex, ha: bool| rows(table, label, ha);
    let found = Indicators {
        page,
        plot: single(&all(&PLOT, true).iter().collect::<Vec<_>>()),
        footprint: footprint(table),
        green: single(&all(&GREEN, false).iter().collect::<Vec<_>>()),
    };
    (found.plot.is_some() || found.footprint.is_some() || found.green.is_some()).then_some(found)
}

/// Drops `mine` when another sheet prints the same field differently.
fn agree<T>(mine: &mut Option<T>, theirs: &Option<T>, same: impl Fn(&T, &T) -> bool) {
    if let (Some(a), Some(b)) = (mine.as_ref(), theirs)
        && !same(a, b)
    {
        *mine = None;
    }
}

/// The table in a dossier's page texts, `(page number, text)` in page order.
///
/// An album can repeat its table on several sheets: the first page wins, and
/// a field another page prints differently is dropped rather than chosen.
pub fn read(pages: &[(usize, &str)]) -> Option<Indicators> {
    let mut found = pages
        .iter()
        .filter_map(|(page, text)| read_page(*page, text));
    let mut merged = found.next()?;
    let areas = |parts: &Vec<Figure>| parts.iter().map(|f| f.m2).collect::<Vec<_>>();
    for other in found {
        agree(&mut merged.plot, &other.plot, |a, b| a.m2 == b.m2);
        agree(&mut merged.green, &other.green, |a, b| a.m2 == b.m2);
        agree(&mut merged.footprint, &other.footprint, |a, b| {
            areas(a) == areas(b)
        });
    }
    Some(merged)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A CAD sheet: letter-spaced labels, «fճ» for հա, «Թ» for քմ/, a
    /// percentage across a line end, a paving-inclusive row and a total.
    const CAD_ALBUM: &str = "ԲՆԱԿԵԼԻ ՏՆԵՐԻ ՏԵԽՆԻԿԱՏՆՏԵՍԱԿԱՆ ՑՈՒՑԱՆԻՇՆԵՐ 1. ՀՈՂ\nԱ\nՄ\nԱՍԻ ԸՆԴՀ\nԱՆՈՒՐ Մ\nԱԿԵՐԵՍ -- 0.032752 fճ -- \
        2. Կ\nԱՌՈՒՑ\nԱ\nՊ\nԱՏՄ\nԱՆ Մ\nԱԿԵՐԵՍ -- 128.0Թ39,2\n% -- \
        3. ԿԱՌՈՒՑԱՊԱՏՄԱՆ ՄԱԿԵՐԵՍ -- 162.10Թ49.5\n% -- /ԱՆՋՐԱՆՑԻԿ ՄԱԿԵՐԵՍՆԵՐ ՆԵՐԱՌՅԱԼ/ \
        4. ԿԱՆԱՉԱՊԱՏՄԱՆ ՄԱԿԵՐԵՍ -- 165,42քմ/50,5\n% -- \
        5. ԸՆԴՀԱՆՈՒՐ ԿԱՌՈՒՑԱՊԱՏՄԱՆ ՄԱԿԵՐԵՍ -- 388.4քմ --";

    /// An explanatory note: the house and the fence on rows of their own.
    const NOTE: &str = "ՏԵԽՆԻԿԱԿԱՆ ՑՈՒՑԱՆԻՇՆԵՐ\nՀՈՂԱՄԱՍԻ ՄԱԿԵՐԵՍԸ 0,01904հա(190.4 ՔՄ)\n\
        1 - Բնակելի տան կառուցապատման մակերես 96.55քմ (50.71% ընդհանուր հողի մակերեսի)\n\
        2 - Անջրանցիկ մակերես(մայթ) 5.52քմ (2.90% ընդհանուր հողի մակերեսի)\n\
        3 - Պարիսպի կառուցապատման մակերես 4.03քմ (2.12% ընդհանուր հողի մակերեսի)\n\
        4 - Կանաչապատման մակերես 84.30քմ (44.27% ընդհանուր հողի մակերեսի)\n\
        5 - Բնակելի տան ընդհանուր կառուցապատման մակերեսը 195.51քմ\n\
        6 - Ընդհանուր կառուցապատման մակերեսը 199.54քմ";

    /// A paragraph, the percentages in slashes, a fence's own area beside.
    const PARAGRAPH: &str = "ՏԵԽՆԻԿԱԿԱՆ ՑՈՒՑԱՆԻՇՆԵՐ Հողամասի ընդհանուր մակերես՝ 0.051 հա \
        Պարսպի մակերես՝ 1.3 քմ Կառուցապատման մակերես՝ 207.1 քմ /40.6 %/ \
        Կանաչապատման մակերես՝ 270.5 քմ /53 %/";

    fn fig(m2: f64, pct: Option<f64>) -> Figure {
        Figure { m2, pct }
    }

    #[test]
    fn a_letter_spaced_cad_table_reads_plot_footprint_and_green() {
        assert_eq!(
            read(&[(11, CAD_ALBUM)]),
            Some(Indicators {
                page: 11,
                plot: Some(fig(327.52, None)),
                footprint: Some(vec![fig(128.0, Some(39.2))]),
                green: Some(fig(165.42, Some(50.5))),
            })
        );
    }

    #[test]
    fn a_house_row_and_a_fence_row_are_the_footprint() {
        let found = read(&[(19, NOTE)]).expect("table");
        assert_eq!(found.plot, Some(fig(190.4, None)));
        assert_eq!(
            found.footprint,
            Some(vec![fig(96.55, Some(50.71)), fig(4.03, Some(2.12))])
        );
        assert_eq!(found.green, Some(fig(84.3, Some(44.27))));
    }

    #[test]
    fn a_paragraph_reads_hectares_and_ignores_the_fences_own_area() {
        let found = read(&[(2, PARAGRAPH)]).expect("table");
        assert_eq!(found.plot, Some(fig(510.0, None)));
        assert_eq!(found.footprint, Some(vec![fig(207.1, Some(40.6))]));
        assert_eq!(found.green, Some(fig(270.5, Some(53.0))));
    }

    #[test]
    fn a_row_printed_twice_differently_is_given_up() {
        let twice = format!("{PARAGRAPH} Կառուցապատման մակերես՝ 76.1 քմ");
        assert_eq!(read(&[(2, twice.as_str())]).expect("table").footprint, None);
    }

    #[test]
    fn sheets_that_disagree_give_that_field_up() {
        let same = read(&[(2, PARAGRAPH), (9, PARAGRAPH)]).expect("table");
        assert_eq!(same.page, 2);
        assert_eq!(same.plot, Some(fig(510.0, None)));
        let other = PARAGRAPH.replace("0.051 հա", "0.052 հա");
        let differ = read(&[(2, PARAGRAPH), (9, other.as_str())]).expect("table");
        assert_eq!(differ.plot, None);
        assert_eq!(differ.footprint, Some(vec![fig(207.1, Some(40.6))]));
    }

    #[test]
    fn a_page_without_the_heading_reads_nothing() {
        let plain = PARAGRAPH.replace("ՏԵԽՆԻԿԱԿԱՆ ՑՈՒՑԱՆԻՇՆԵՐ", "ԲԱՑԱՏՐԱԳԻՐ");
        assert_eq!(read(&[(2, plain.as_str())]), None);
        assert_eq!(read(&[]), None);
    }
}
