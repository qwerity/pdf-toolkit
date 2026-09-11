//! Deterministic entity extraction from a page's embedded text.
//!
//! Two layers. Bare regexes catch the identifiers that are unambiguous
//! anywhere they appear — cadastral codes, phone numbers, emails. Template
//! parsers handle the two document families that dominate the corpus and are
//! born-digital with a reliable text layer: the e-cadastre ՎԿԱՅԱԿԱՆ
//! (ownership certificate) / ՏԵՂԵԿԱՆՔ (reference), and the Ձև N 1-1
//! application form. Everything less structured — names in prose, handwritten
//! fields — is the vision pass's job, not a regex's.

use std::sync::LazyLock;

use regex::Regex;

/// One extracted entity occurrence, before registry deduplication. The id
/// already encodes the type; see [`split_id`].
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    /// `<type>:<value>` — the registry id.
    pub id: String,
    /// The value as printed on the page.
    pub value: String,
}

fn found(entity_type: &str, value: &str) -> Found {
    Found {
        id: entity_id(entity_type, value),
        value: value.to_string(),
    }
}

/// The registry id an entity gets — the one rule, shared with the vision
/// pass so the same name read twice lands on one graph node. People and
/// organisations are slugged (first two words, transliterated); identifier
/// types keep the value verbatim.
pub fn entity_id(entity_type: &str, value: &str) -> String {
    match entity_type {
        "person" | "org" => {
            let slug = value
                .split_whitespace()
                .take(2)
                .map(transliterate)
                .collect::<Vec<_>>()
                .join("-");
            format!("{entity_type}:{slug}")
        }
        _ => format!("{entity_type}:{value}"),
    }
}

/// The `<type>:<value>` halves of a registry id.
pub fn split_id(id: &str) -> (&str, &str) {
    id.split_once(':').unwrap_or((id, id))
}

static CADASTRAL_CODE: LazyLock<Regex> = LazyLock::new(|| {
    // XX-XXX-XXXX-XXXX, optionally with a -NNN building suffix.
    Regex::new(r"\b(\d{2}-\d{3}-\d{4}-\d{4}(?:-\d{3})?)\b").expect("static regex")
});
static CERTIFICATE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"ՎԿԱՅԱԿԱՆ N\s*(\d{8}-\d{2}-\d{4})").expect("static regex"));
static REFERENCE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"Փաստաթղթի համար՝\s*([Ա-Ֆ]{0,3}-?\d{8}-[\d-]+\d)").expect("static regex")
});
static APPLICATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b([ԾՃ]ՀԱ/\d+/\d+-\d+)\b").expect("static regex"));
static PHONE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\+374\d{2}-\d{3}-\d{3}").expect("static regex"));
static EMAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}")
        .expect("static regex")
});
static SHEET: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:ԷՋ[-  ]?\d+|Է[.\-]\d+|Ճ-\d+)").expect("static regex"));

/// Every unambiguous identifier in the text, in first-appearance order.
pub fn extract(text: &str) -> Vec<Found> {
    let mut out: Vec<Found> = Vec::new();
    let mut push = |f: Found| {
        if !out.iter().any(|e| e.id == f.id) {
            out.push(f);
        }
    };

    for capture in CADASTRAL_CODE.captures_iter(text) {
        push(found("cadastral_code", &capture[1]));
    }
    for capture in CERTIFICATE.captures_iter(text) {
        push(found("certificate_number", &capture[1]));
    }
    for capture in REFERENCE.captures_iter(text) {
        push(found("reference_number", &capture[1]));
    }
    for capture in APPLICATION.captures_iter(text) {
        push(found("application_number", &capture[1]));
    }
    for m in PHONE.find_iter(text) {
        push(found("phone", m.as_str()));
    }
    for m in EMAIL.find_iter(text) {
        push(found("email", m.as_str()));
    }
    out
}

/// The album sheet designation printed on the page, when exactly one is.
///
/// A sheet index page names every sheet; requiring a unique match keeps this
/// from mislabelling the table of contents as sheet 1.
pub fn sheet_designation(text: &str) -> Option<String> {
    let mut matches = SHEET.find_iter(text).map(|m| m.as_str());
    let first = matches.next()?;
    let unique = matches.all(|m| m == first);
    unique.then(|| first.to_string())
}

/// Which document template a page's text matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Template {
    /// e-cadastre ownership certificate (ՎԿԱՅԱԿԱՆ), first or later page.
    Certificate { continuation: bool },
    /// e-cadastre reference (ՏԵՂԵԿԱՆՔ).
    Reference,
    /// The Ձև N 1-1 design-permission application form.
    ApplicationForm,
}

/// Detects the template, if any. Order matters: a certificate quotes the word
/// ՏԵՂԵԿԱՆՔ nowhere, but a reference can mention certificates, so the
/// certificate header is checked against the page's own opening.
pub fn detect_template(text: &str) -> Option<Template> {
    if text.contains("Ձև N 1-1") {
        return Some(Template::ApplicationForm);
    }
    if CERTIFICATE.is_match(text) && text.contains("գաղտնաբառ") {
        let continuation = text.contains("Էջ 2/") || text.contains("ՇԻՆՈՒԹՅՈՒՆՆԵՐԻ ԲՆՈՒԹԱԳՐԵՐԸ");
        return Some(Template::Certificate { continuation });
    }
    let head: String = text.chars().take(200).collect();
    if head.contains("ՏԵՂԵԿԱՆՔ") && text.contains("ԿԱԴԱՍՏՐԻ ԿՈՄԻՏԵ") {
        return Some(Template::Reference);
    }
    None
}

/// The registered owner(s) a certificate's section 1 names.
///
/// The section header is fixed; the next non-empty line lists the subjects,
/// comma-separated, each optionally followed by an ownership share like
/// `(1/2)`.
pub fn certificate_owners(text: &str) -> Vec<Found> {
    let Some(position) = text.find("ԳՐԱՆՑՎԱԾ ԻՐԱՎՈՒՆՔԻ ՍՈՒԲՅԵԿՏ") else {
        return Vec::new();
    };
    let rest = &text[position..];
    let Some(line) = rest.lines().skip(1).find(|l| !l.trim().is_empty()) else {
        return Vec::new();
    };
    line.split(',')
        .filter_map(|part| {
            let name = part
                .split('(')
                .next()
                .unwrap_or_default()
                .trim()
                .trim_end_matches(':');
            let words: Vec<&str> = name.split_whitespace().collect();
            // A subject is FIRST LAST [PATRONYMIC]; anything shorter is noise.
            if words.len() < 2 || !name.chars().any(|c| ('Ա'..='Ֆ').contains(&c)) {
                return None;
            }
            let slug = person_slug(words[0], words[1]);
            Some(Found {
                id: format!("person:{slug}"),
                value: name.to_string(),
            })
        })
        .collect()
}

/// `first-last` in Latin letters, for a stable person entity id.
fn person_slug(first: &str, last: &str) -> String {
    format!("{}-{}", transliterate(first), transliterate(last))
}

/// Armenian-to-Latin romanization, lowercased. Only used for ids; the
/// entity's `value` keeps the original spelling.
pub fn transliterate(word: &str) -> String {
    let mut out = String::new();
    let mut previous_vo = false;
    for c in word.chars() {
        let lower = c.to_lowercase().next().unwrap_or(c);
        // ո+ւ is the digraph ու, one vowel: drop the 'o' already emitted.
        if lower == 'ւ' && previous_vo {
            out.pop();
        }
        previous_vo = lower == 'ո';
        let mapped: &str = match lower {
            'ա' => "a",
            'բ' => "b",
            'գ' => "g",
            'դ' => "d",
            'ե' => "e",
            'զ' => "z",
            'է' => "e",
            'ը' => "y",
            'թ' => "t",
            'ժ' => "zh",
            'ի' => "i",
            'լ' => "l",
            'խ' => "kh",
            'ծ' => "ts",
            'կ' => "k",
            'հ' => "h",
            'ձ' => "dz",
            'ղ' => "gh",
            'ճ' => "ch",
            'մ' => "m",
            'յ' => "y",
            'ն' => "n",
            'շ' => "sh",
            'ո' => "o",
            'չ' => "ch",
            'պ' => "p",
            'ջ' => "j",
            'ռ' => "r",
            'ս' => "s",
            'վ' => "v",
            'տ' => "t",
            'ր' => "r",
            'ց' => "ts",
            'ւ' => "u",
            'փ' => "p",
            'ք' => "k",
            'օ' => "o",
            'ֆ' => "f",
            'և' => "ev",
            c if c.is_ascii_alphanumeric() => {
                out.push(c.to_ascii_lowercase());
                continue;
            }
            _ => continue,
        };
        out.push_str(mapped);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cadastral_codes_with_and_without_building_suffix_are_found() {
        let text = "հողամասը՝ 01-011-0274-0052, շինությունը՝ 01-011-0274-0052-001";
        let extracted = extract(text);
        let ids: Vec<&str> = extracted.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "cadastral_code:01-011-0274-0052",
                "cadastral_code:01-011-0274-0052-001"
            ]
        );
    }

    #[test]
    fn certificate_reference_application_phone_and_email_are_found() {
        let text = "ՎԿԱՅԱԿԱՆ N 09012025-01-0144, գաղտնաբառ՝ 0EEEE0EEE0EE\n\
                    Փաստաթղթի համար՝ ԱՏ-05082025-99-0316\n\
                    Հ Ա Յ Տ\nԾՀԱ/11/00427-26\n\
                    +37400-000-002, Sample-owner-00@example.am";
        let ids: Vec<String> = extract(text).into_iter().map(|f| f.id).collect();
        assert!(ids.contains(&"certificate_number:09012025-01-0144".into()));
        assert!(ids.contains(&"reference_number:ԱՏ-05082025-99-0316".into()));
        assert!(ids.contains(&"application_number:ԾՀԱ/11/00427-26".into()));
        assert!(ids.contains(&"phone:+37400-000-002".into()));
        assert!(ids.contains(&"email:Sample-owner-00@example.am".into()));
    }

    #[test]
    fn duplicate_mentions_collapse_to_one_entity() {
        let text = "01-011-0388-0007 եւ նորից 01-011-0388-0007";
        assert_eq!(extract(text).len(), 1);
    }

    #[test]
    fn templates_are_told_apart() {
        assert_eq!(
            detect_template("Ձև N 1-1\nԵՐԵՎԱՆԻ ՔԱՂԱՔԱՊԵՏ"),
            Some(Template::ApplicationForm)
        );
        assert_eq!(
            detect_template("ՎԿԱՅԱԿԱՆ N 09012025-01-0144, գաղտնաբառ՝ X\nԷջ 1/2"),
            Some(Template::Certificate {
                continuation: false
            })
        );
        assert_eq!(
            detect_template(
                "ՎԿԱՅԱԿԱՆ N 09012025-01-0144, գաղտնաբառ՝ X\nԷջ 2/2\n5. ՇԻՆՈՒԹՅՈՒՆՆԵՐԻ ԲՆՈՒԹԱԳՐԵՐԸ"
            ),
            Some(Template::Certificate { continuation: true })
        );
        assert_eq!(
            detect_template("ՀԱՅԱՍՏԱՆԻ ՀԱՆՐԱՊԵՏՈՒԹՅՈՒՆ\nԿԱԴԱՍՏՐԻ ԿՈՄԻՏԵ\nՏԵՂԵԿԱՆՔ\nԳՈՒՅՔԻ"),
            Some(Template::Reference)
        );
        assert_eq!(detect_template("ԳԼԽԱՎՈՐ ՀԱՏԱԿԱԳԻԾ Մ 1:500"), None);
    }

    #[test]
    fn certificate_owners_come_from_section_one() {
        let text = "1. ԳՐԱՆՑՎԱԾ ԻՐԱՎՈՒՆՔԻ ՍՈՒԲՅԵԿՏ(ՆԵՐ)\n\
                    ՀՐԱՉ ԱԲՐԱՀԱՄՅԱՆ ՍԵՐԳԵՅԻ (1/2), ՆԵԼԼԻ ԱԲՐԱՀԱՄՅԱՆ ՌՈԲԵՐՏԻ (1/2)\n\
                    2. ԱՆՇԱՐԺ ԳՈՒՅՔԻ";
        let owners = certificate_owners(text);
        assert_eq!(owners.len(), 2);
        assert_eq!(owners[0].id, "person:hrach-abrahamyan");
        assert_eq!(owners[0].value, "ՀՐԱՉ ԱԲՐԱՀԱՄՅԱՆ ՍԵՐԳԵՅԻ");
        assert_eq!(owners[1].id, "person:nelli-abrahamyan");
    }

    #[test]
    fn entity_ids_slug_people_and_keep_identifiers_verbatim() {
        assert_eq!(
            entity_id("person", "Բադալյան Սուրեն Ռաֆայելի"),
            "person:badalyan-suren"
        );
        assert_eq!(
            entity_id("cadastral_code", "01-011-0388-0007"),
            "cadastral_code:01-011-0388-0007"
        );
        assert_eq!(
            split_id("certificate_number:22102024-01-0358"),
            ("certificate_number", "22102024-01-0358")
        );
    }

    #[test]
    fn transliteration_romanizes_common_names() {
        assert_eq!(transliterate("Խաչատրյան"), "khachatryan");
        assert_eq!(transliterate("Բադալյան"), "badalyan");
        assert_eq!(transliterate("Հովհաննիսյան"), "hovhannisyan");
        // ու is one vowel, not o+u
        assert_eq!(transliterate("Ավետիսյան"), "avetisyan");
        assert_eq!(transliterate("Հարությունյան"), "harutyunyan");
    }

    #[test]
    fn a_unique_sheet_designation_is_reported_and_a_list_is_not() {
        assert_eq!(sheet_designation("Ճ-1 ԺԱՄԱՆԱԿԱՑՈՒՅՑ"), Some("Ճ-1".into()));
        assert_eq!(sheet_designation("ԷՋ-01 ԷՋ-02 ԷՋ-03 անվանացանկ"), None);
        assert_eq!(sheet_designation("հատակագիծ"), None);
    }
}
