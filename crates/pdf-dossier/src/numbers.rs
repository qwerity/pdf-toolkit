//! Numbers as the corpus prints them: `261`, `0.02612`, `6,40`, `9,33.0`,
//! `239․24` (U+2024), and areas with the unit spelled a dozen ways
//! (հա, քմ, ՔՄ, ք.մ, Ք/Մ, մ², մ2, m2). One place decides what a printed
//! figure is worth in square metres.

use std::sync::LazyLock;

use regex::Regex;

/// Square metres in a hectare.
const M2_PER_HA: f64 = 10_000.0;

/// A printed decimal, whatever the separator. `9,33.0` (a typo the corpus
/// contains) reads as `9.33`: the first separator is the decimal point.
pub fn parse_number(printed: &str) -> Option<f64> {
    let cleaned: String = printed
        .trim()
        .chars()
        .map(|c| match c {
            ',' | '\u{2024}' => '.',
            c => c,
        })
        .filter(|c| !c.is_whitespace())
        .collect();
    let mut parts = cleaned.splitn(2, '.');
    let whole = parts.next()?;
    let fraction = parts.next().map(|f| f.split('.').next().unwrap_or(""));
    let text = match fraction {
        Some(f) if !f.is_empty() => format!("{whole}.{f}"),
        _ => whole.to_string(),
    };
    text.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// An area unit as printed, in square metres per unit; `None` for a token
/// that is not an area unit.
pub fn unit_factor(unit: &str) -> Option<f64> {
    let compact: String = unit
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '.' && *c != '/')
        .collect::<String>()
        .to_lowercase();
    match compact.as_str() {
        "հա" | "ha" => Some(M2_PER_HA),
        "քմ" | "մ²" | "մ2" | "m²" | "m2" | "քառմ" => Some(1.0),
        _ => None,
    }
}

/// `<number> <unit>` occurrences: the number's text, the unit's text, and the
/// byte range of the whole match. A letter right after the unit
/// (`մ2` inside a word) is not a unit.
static AREA: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(\d+(?:[.,․\u{2024}]\d+)*)\s*(հա|ՀԱ|ha|ք\.?\s?/?\s?մ|Ք\.?\s?/?\s?Մ|մ\s?²|Մ\s?²|մ\s?2|Մ\s?2|m²|m2|M2|քառ\.?\s?մ)(?:[^\p{L}]|$)",
    )
    .expect("valid regex")
});

/// One printed area.
#[derive(Debug, Clone, PartialEq)]
pub struct PrintedArea {
    pub printed: String,
    pub m2: f64,
    /// Byte offset of the number in the text, for reading the words before it.
    pub start: usize,
    pub end: usize,
}

/// Every printed area in `text`, in order.
pub fn printed_areas(text: &str) -> Vec<PrintedArea> {
    AREA.captures_iter(text)
        .filter_map(|c| {
            let number = c.get(1)?;
            let unit = c.get(2)?;
            let factor = unit_factor(unit.as_str())?;
            let value = parse_number(number.as_str())?;
            Some(PrintedArea {
                printed: format!("{} {}", number.as_str(), unit.as_str().trim()),
                m2: value * factor,
                start: number.start(),
                end: unit.end(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separators_and_units_are_read() {
        assert_eq!(parse_number("6,40"), Some(6.4));
        assert_eq!(parse_number("0.02612"), Some(0.02612));
        assert_eq!(parse_number("9,33.0"), Some(9.33));
        assert_eq!(parse_number("239․24"), Some(239.24));
        assert_eq!(unit_factor("հա"), Some(10_000.0));
        assert_eq!(unit_factor("Ք/Մ"), Some(1.0));
        assert_eq!(unit_factor("ք. մ"), Some(1.0));
        assert_eq!(unit_factor("մ"), None);
    }

    #[test]
    fn areas_are_found_with_their_words() {
        let text =
            "Հողամասի մակերեսի չափը կազմում է 261 քմ։ Մակերեսի չափը (հա)՝ 0.02612 ցանկապատ 2.0քմ";
        let areas = printed_areas(text);
        let m2: Vec<f64> = areas.iter().map(|a| a.m2).collect();
        assert_eq!(m2, vec![261.0, 2.0]);
        // A unit broken over a line end, as pdfium reads a superscript.
        assert_eq!(printed_areas("տուն-40.00մ\n2 002").len(), 1);
        assert!(text[..areas[0].start].ends_with("կազմում է "));
    }
}
