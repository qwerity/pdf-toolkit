//! The figures a dossier states about its parcel, each with the page it was
//! read from and who vouches for that page.
//!
//! A certificate prints the parcel's area in hectares once; the explanatory
//! note repeats it in square metres; the architect's general plan carries
//! the turning-point table again; the application form quotes the fence's
//! area and the note quotes another. These are the same facts written
//! several times by several hands, and the point of listing them with their
//! provenance is that [`crate::checks`] can then say which copies agree.
//!
//! Reading is deterministic: fixed labels on the Committee's forms, and a
//! small vocabulary of context words before a printed area on everything
//! else. A figure whose context is not recognised is not a fact — an
//! unlabelled number is not evidence of anything.

use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;

use crate::entities::entity_id;
use crate::numbers::{parse_number, printed_areas};
use crate::report::Via;
use crate::trust::Trust;

/// What a fact is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FactKind {
    /// A parcel's cadastral code (`text`), the building suffix removed.
    CadastralCode,
    /// The plot's area, square metres.
    ParcelArea,
    /// A built-up (footprint) area the applicant states, square metres.
    FootprintArea,
    /// A registered building's area, square metres; `subject` is its code.
    BuildingArea,
    /// An area the application's «Նախատեսում եմ» line or a note states for a
    /// named item; `subject` names the item (`fence`, `building`, …).
    PlannedArea,
    /// A registered owner (`text` is the person entity id).
    Owner,
    /// Who applies or commissions (`text` is the person entity id).
    Applicant,
    /// The site address (`text` is a normalised key).
    Address,
    /// A certificate number as printed or cited.
    CertificateNumber,
    /// The area of the polygon a turning-point table describes, square metres.
    SurveyArea,
    /// The perimeter of that polygon, metres.
    SurveyPerimeter,
    /// How many turning points the table has.
    TurningPoints,
    /// Who signed a page BY HAND, as the vision pass read the written name
    /// (`text` is the person entity id, `printed` the name as written).
    Signatory,
    /// The person an identity document names — a passport's or ID card's
    /// holder (`text` is the person entity id).
    IdentityHolder,
}

/// One figure, as one page states it.
#[derive(Debug, Clone, Serialize)]
pub struct Fact {
    pub kind: FactKind,
    /// The figure in canonical units (m², metres, a count).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number: Option<f64>,
    /// The canonical text (a code, an entity id, an address key).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// As printed.
    pub printed: String,
    pub page: usize,
    pub trust: Trust,
    pub via: Via,
    /// What the fact is about when the kind needs it: a building code, a
    /// planned item, a side index.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// The parcel an official page is about, when it says: a neighbour's
    /// certificate states a neighbour's owner and area.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub about: Option<String>,
}

/// The words a page uses for the thing an area belongs to. Matched on the
/// lower-cased forty characters before the number; the nearest wins.
const CONTEXT_WORDS: &[(&str, FactKind, &str)] = &[
    ("ցանկապատ", FactKind::PlannedArea, "fence"),
    ("պարիսպ", FactKind::PlannedArea, "fence"),
    // պարիսպ drops its ի when inflected: «Պարսպի մակերես», «պարսպով».
    ("պարսպ", FactKind::PlannedArea, "fence"),
    ("հողամաս", FactKind::ParcelArea, ""),
    ("հողի", FactKind::ParcelArea, ""),
    ("կառուցապատման", FactKind::FootprintArea, ""),
    ("հիմնական շենք", FactKind::PlannedArea, "building"),
    ("առանձնատ", FactKind::PlannedArea, "building"),
    ("բնակելի տ", FactKind::PlannedArea, "building"),
];

/// Context words that mean the area is something else (a floor, greenery,
/// a room) and must not be read as any of the above even if a parcel word
/// is also near.
const EXCLUDING_WORDS: &[&str] = &[
    "կանաչապատ",
    "հարկ",
    "օգտակար",
    "ընդհանուր շին",
    "սենյակ",
    "խոհանոց",
    "միջանցք",
    "սանհանգույց",
];

/// A code and its optional `-NNN` building suffix. Where a code starts and ends
/// is checked by `standalone`, not by the pattern: `regex` has no look-around,
/// and a boundary written into the pattern consumed the space between two codes,
/// so a map listing its parcels with single spaces between them lost every
/// second code, the subject's among them.
static CODE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\d{2}-\d{3}-\d{4}-\d{4}(?:-\d{3})?").expect("valid regex"));
/// The length of a code without its building suffix, `NN-NNN-NNNN-NNNN`.
const CODE_LEN: usize = 16;
/// The quarter-and-parcel half of a code, as a fee table prints it.
static PARTIAL_CODE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\d{4}-\d{4}").expect("valid regex"));

/// The matches of `re` that stand alone, no digit or hyphen touching them on
/// either side, so a code is not read out of a longer run of digits.
fn standalone<'t>(re: &Regex, text: &'t str) -> impl Iterator<Item = regex::Match<'t>> {
    let joins = |c: char| c.is_ascii_digit() || c == '-';
    re.find_iter(text).filter(move |m| {
        !text[..m.start()].chars().next_back().is_some_and(joins)
            && !text[m.end()..].chars().next().is_some_and(joins)
    })
}
static CERT_AREA_HA: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"Մակերեսի չափը\s*\(հա\)\s*[՝:]?\s*(\d+(?:[.,]\d+)?)").expect("valid regex")
});
static CERT_CODE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"Կադաստրային ծածկագիրը\s*[՝:]?\s*(\d{2}-\d{3}-\d{4}-\d{4})").expect("valid regex")
});
static CERT_NUMBER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"ՎԿԱՅԱԿԱՆ N\s*(\d{8}-\d{2}-\d{4})").expect("valid regex"));
/// A certificate number cited on a page that is not the certificate.
static CERT_CITATION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:N|№|թիվ)\s*(\d{8}-\d{2}-\d{4})(?:$|[^\d-])").expect("valid regex")
});
/// A registered building's row: its code and, within reach, an area.
static BUILDING_ROW: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(\d{2}-\d{3}-\d{4}-\d{4}-(\d{3}))[^\d]{0,60}?(\d+(?:[.,]\d+)?)\s*(?:քմ|ՔՄ|մ2|մ²)")
        .expect("valid regex")
});
/// A building named by its suffix alone: `001 Բնակելի տուն-40.00մ2`.
static BUILDING_SUFFIX_ROW: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:^|[^\d-])(00\d)\s+[\p{L} /\-]{2,40}?[-\s]\s*(\d+(?:[.,]\d+)?)\s*(?:քմ|ՔՄ|մ\s?2|մ\s?²)",
    )
    .expect("valid regex")
});
/// «Դիմող՝ NAME» on a reference letter or fee table.
static APPLICANT_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"Դիմող[՝`'ª:\s*_]*\n?[ \t]*\**([Ա-Ֆ][Ա-Ֆա-ֆ]+(?:[ \t]+[Ա-Ֆ][Ա-Ֆա-ֆ]+){1,2})")
        .expect("valid regex")
});
/// The application form's «NAMEից» line.
static FROM_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^\s*\**([Ա-Ֆ][ա-ֆ]+\s+[Ա-Ֆ][ա-ֆ]+)ից\**\s*$").expect("valid regex")
});
/// «ՊԱՏՎԻՐԱՏՈՒ՝ NAME» in a title block, and «NAMEին պատկանող» in a note.
static CLIENT_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)պատվիրատու[՝`'ª:\s]*([Ա-Ֆ][Ա-Ֆա-ֆ.]+[ \t]+[Ա-Ֆ][Ա-Ֆա-ֆ]+)")
        .expect("valid regex")
});
static BELONGS_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"([Ա-Ֆ][Ա-Ֆա-ֆ]+\s+[Ա-Ֆ][Ա-Ֆա-ֆ]+?)(?:ին|ի)\s+պատկանող").expect("valid regex")
});
static CERT_ADDRESS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"ԳՏՆՎԵԼՈՒ ՎԱՅՐԸ ԵՎ ԱՆՎԱՆՈՒՄԸ\**\s*\n\s*([^\n]+)").expect("valid regex")
});
static ADDRESS_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)հասցե[նը]?\s*[՝`'ª:]\s*([^\n]*\d[^\n]*)").expect("valid regex")
});
/// Where an address stops, when it is not on a line of its own.
///
/// An application form gives the address a line; a drawing's title block runs
/// it into the next field — `ՀԱՍՑԵ՝ … ՓՈՂՈՑ 12/2 ԲՆԱԿԵԼԻ ՏՈՒՆ ՀԱՍՑԵՈՒՄ
/// ՊԱՏՎԻՐԱՏՈՒ՝ <name>` is one line — and [`ADDRESS_LINE`] reads to the end of
/// it, so the client's name ended up inside the address key. These pages only
/// became readable when the legacy Armenian fonts did.
const ADDRESS_ENDS_AT: &[&str] = &["ԲՆԱԿԵԼԻ ՏՈՒՆ", "ՀԱՍՑԵՈՒՄ", "ՊԱՏՎԻՐԱՏՈՒ", "ՏՆՕՐԵՆ"];

/// The album's place-and-date stamp, `ԵՐԵՎԱՆ 2026`, which a title block can
/// set on the same line as the address. Its year is not a house number: kept,
/// it made two copies of one address disagree on their numbers. `և` is one
/// letter in lower case and two, `ԵՎ`, in capitals, which case folding does
/// not bridge — hence both spellings.
static PLACE_DATE_STAMP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\s+(?:ք\.\s*)?եր(?:և|եվ)ան\s*,?\s+(?:19|20)\d{2}\b").expect("valid regex")
});

/// The address as printed, cut where the next field on the line begins.
fn address_up_to_the_next_field(line: &str) -> &str {
    let end = ADDRESS_ENDS_AT
        .iter()
        .filter_map(|label| line.find(label))
        .chain(PLACE_DATE_STAMP.find(line).map(|m| m.start()))
        .min()
        .unwrap_or(line.len());
    line[..end].trim_end_matches([' ', ',', '.', '՝', '`'])
}

static TRAILING_WORDS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s*(?:\([^)]*\))?\s*$").expect("valid regex"));

/// What the reader knows about a page before reading its figures.
pub struct PageInput<'a> {
    pub page: usize,
    pub labels: &'a [String],
    /// The page's text, legacy fonts decoded.
    pub text: &'a str,
    pub trust: Trust,
    pub via: Via,
}

fn fact(kind: FactKind, page: &PageInput<'_>, printed: &str) -> Fact {
    Fact {
        kind,
        number: None,
        text: None,
        printed: printed.trim().to_string(),
        page: page.page,
        trust: page.trust,
        via: page.via,
        subject: None,
        about: None,
    }
}

fn has_label(page: &PageInput<'_>, label: &str) -> bool {
    page.labels.iter().any(|l| l == label)
}

/// A name as a person entity id, dative and genitive endings dropped.
fn person_key(name: &str) -> String {
    entity_id("person", name.trim())
}

/// An address reduced to what identifies it: its numbers (house, plot) and
/// its street word, lower-cased; the city, district and the kind of
/// property are not part of the key.
pub fn address_key(address: &str) -> String {
    const NOISE: &[&str] = &[
        "ք",
        "քաղաք",
        "երևան",
        "երեւան",
        "հհ",
        "բնակելի",
        "տուն",
        "հողամաս",
        "հասցեում",
        "հասցե",
        "գտնվող",
        "վարչական",
        "շրջան",
        "համայնք",
        "մարզ",
        "փողոց",
        "փող",
        "պողոտա",
        "պող",
        "թաղամաս",
        "թաղ",
        "նրբանցք",
        "նրբ",
        "թղմ",
    ];
    let mut numbers: Vec<String> = Vec::new();
    let mut words: Vec<String> = Vec::new();
    for token in address
        .to_lowercase()
        .split(|c: char| c.is_whitespace() || matches!(c, ',' | '՝' | ':' | '։' | '.' | '`'))
        .filter(|t| !t.is_empty())
    {
        if token.chars().any(|c| c.is_ascii_digit()) {
            numbers.push(token.to_string());
        } else if !NOISE.contains(&token) && token.chars().count() >= 3 {
            words.push(token.chars().take(7).collect());
        }
    }
    numbers.sort();
    words.sort();
    format!("{}|{}", words.join(" "), numbers.join(" "))
}

/// Whether two address keys name the same place: the same numbers (house,
/// plot) and at least one street word in common. `None` when the keys carry
/// nothing comparable.
pub fn same_address(a: &str, b: &str) -> Option<bool> {
    let split = |key: &str| -> (Vec<String>, Vec<String>) {
        let (words, numbers) = key.split_once('|').unwrap_or((key, ""));
        (
            words.split_whitespace().map(str::to_string).collect(),
            numbers.split_whitespace().map(str::to_string).collect(),
        )
    };
    let (wa, na) = split(a);
    let (wb, nb) = split(b);
    if na.is_empty() && nb.is_empty() && (wa.is_empty() || wb.is_empty()) {
        return None;
    }
    if na != nb {
        return Some(false);
    }
    Some(wa.is_empty() || wb.is_empty() || wa.iter().any(|w| wb.contains(w)))
}

/// The facts a certificate's first page states.
fn certificate_facts(page: &PageInput<'_>, out: &mut Vec<Fact>) {
    let about = CERT_CODE.captures(page.text).map(|c| c[1].to_string());
    if let Some(code) = &about {
        let mut f = fact(FactKind::CadastralCode, page, code);
        f.text = Some(code.clone());
        f.about = about.clone();
        out.push(f);
    }
    if let Some(c) = CERT_AREA_HA.captures(page.text)
        && let Some(ha) = parse_number(&c[1])
    {
        let mut f = fact(FactKind::ParcelArea, page, &format!("{} հա", &c[1]));
        f.number = Some(ha * 10_000.0);
        f.about = about.clone();
        out.push(f);
    }
    if let Some(c) = CERT_NUMBER.captures(page.text) {
        let mut f = fact(FactKind::CertificateNumber, page, &c[1]);
        f.text = Some(c[1].to_string());
        f.about = about.clone();
        out.push(f);
    }
    for owner in crate::entities::certificate_owners(page.text) {
        let mut f = fact(FactKind::Owner, page, &owner.value);
        f.text = Some(owner.id);
        f.about = about.clone();
        out.push(f);
    }
    if let Some(c) = CERT_ADDRESS.captures(page.text) {
        let printed = TRAILING_WORDS.replace(c[1].trim_matches('*').trim(), "");
        let mut f = fact(FactKind::Address, page, &printed);
        f.text = Some(address_key(&printed));
        f.about = about.clone();
        out.push(f);
    }
}

/// Registered buildings, from a certificate's second page.
fn building_rows(page: &PageInput<'_>, out: &mut Vec<Fact>) {
    for c in BUILDING_ROW.captures_iter(page.text) {
        let Some(area) = parse_number(&c[3]) else {
            continue;
        };
        let mut f = fact(
            FactKind::BuildingArea,
            page,
            &format!("{} {} քմ", &c[1], &c[3]),
        );
        f.number = Some(area);
        f.subject = Some(c[1].to_string());
        f.about = Some(c[1][..c[1].len() - 4].to_string());
        out.push(f);
    }
}

/// Every cadastral code on the page, as a fact each. `subject` says how the
/// page presents it: `None` for a code declared as the site's (the form's,
/// a title block's, one introduced by «ծածկագիր»), `map` for the codes a
/// map labels its parcels with, `mention` for a code a note names in
/// passing (a neighbour, a demolished annex).
fn code_facts(page: &PageInput<'_>, out: &mut Vec<Fact>) {
    let drawing = page
        .labels
        .iter()
        .any(|l| l == "cad_drawing" || l == "scanned_drawing" || l == "map");
    let mut found: Vec<(String, usize)> = Vec::new();
    for m in standalone(&CODE, page.text) {
        let code = m.as_str()[..CODE_LEN].to_string();
        if found.iter().any(|(seen, _)| *seen == code) {
            continue;
        }
        found.push((code, m.start()));
    }
    let is_map = drawing && found.len() >= 2;
    let lower = page.text.to_lowercase();
    for (code, at) in found {
        let declared = has_label(page, "application_form")
            || (drawing && !is_map)
            || lower[..at]
                .rfind("ծածկագիր")
                .is_some_and(|word| at - word < 120);
        let mut f = fact(FactKind::CadastralCode, page, &code);
        f.text = Some(code);
        f.subject = if is_map {
            Some("map".into())
        } else if declared {
            None
        } else {
            Some("mention".into())
        };
        out.push(f);
    }
    if page.text.contains("Տրամադրված տեղեկատվությունը") {
        for m in standalone(&PARTIAL_CODE, page.text) {
            let mut f = fact(FactKind::CadastralCode, page, m.as_str());
            f.text = Some(m.as_str().to_string());
            f.subject = Some("partial".into());
            out.push(f);
        }
    }
}

/// Areas the page prints with a recognised word before them, skipping the
/// `taken` byte ranges (registered buildings already read by their rows).
fn context_area_facts(page: &PageInput<'_>, taken: &[(usize, usize)], out: &mut Vec<Fact>) {
    for area in printed_areas(page.text) {
        if taken
            .iter()
            .any(|(s, e)| area.start >= *s && area.start < *e)
        {
            continue;
        }
        // A hectare figure is the plot's whatever the words around it.
        if area.printed.to_lowercase().ends_with("հա") {
            let mut f = fact(FactKind::ParcelArea, page, &area.printed);
            f.number = Some(area.m2);
            out.push(f);
            continue;
        }
        // The words on the same line before the number, up to 45 characters:
        // a figure keeps the line it is printed on, not the previous one.
        let before: String = page.text[..area.start]
            .chars()
            .rev()
            .take_while(|c| *c != '\n')
            .take(45)
            .collect::<String>()
            .chars()
            .rev()
            .collect::<String>()
            .to_lowercase();
        let nearest = CONTEXT_WORDS
            .iter()
            .filter_map(|(word, kind, subject)| before.rfind(word).map(|at| (at, *kind, *subject)))
            .max_by_key(|(at, _, _)| *at);
        let Some((at, kind, subject)) = nearest else {
            continue;
        };
        // An excluding word closer to the number than the context word
        // means the number is a floor's, a room's, the greenery's.
        let excluded = EXCLUDING_WORDS
            .iter()
            .filter_map(|w| before.rfind(w))
            .any(|ex| ex > at);
        if excluded {
            continue;
        }
        let mut f = fact(kind, page, &area.printed);
        f.number = Some(area.m2);
        if !subject.is_empty() {
            f.subject = Some(subject.to_string());
        }
        out.push(f);
    }
}

/// The «Նախատեսում եմ» line, item by item.
fn planned_facts(page: &PageInput<'_>, planned: &str, out: &mut Vec<Fact>) {
    for item in planned.split(',') {
        for area in printed_areas(item) {
            let lower = item.to_lowercase();
            let subject = if lower.contains("ցանկապատ") || lower.contains("պարիսպ") {
                "fence"
            } else if lower.contains("շենք") || lower.contains("տուն") || lower.contains("առանձնատ")
            {
                "building"
            } else {
                continue;
            };
            let mut f = fact(FactKind::PlannedArea, page, &area.printed);
            f.number = Some(area.m2);
            f.subject = Some(subject.into());
            out.push(f);
        }
    }
}

fn name_facts(page: &PageInput<'_>, out: &mut Vec<Fact>) {
    let mut push = |name: &str| {
        let mut f = fact(FactKind::Applicant, page, name);
        f.text = Some(person_key(name));
        out.push(f);
    };
    if has_label(page, "application_form") {
        if let Some(c) = FROM_LINE.captures(page.text) {
            push(&c[1]);
        }
        return;
    }
    if page.trust == Trust::Official {
        if let Some(c) = APPLICANT_LINE.captures(page.text) {
            push(&c[1]);
        }
        return;
    }
    for c in CLIENT_LINE.captures_iter(page.text) {
        push(&c[1]);
    }
    for c in BELONGS_LINE.captures_iter(page.text) {
        push(&c[1]);
    }
}

fn address_facts(page: &PageInput<'_>, request_address: Option<&str>, out: &mut Vec<Fact>) {
    if let Some(address) = request_address {
        let mut f = fact(FactKind::Address, page, address);
        f.text = Some(address_key(address));
        out.push(f);
        return;
    }
    if page.trust == Trust::Applicant
        && let Some(c) = ADDRESS_LINE.captures(page.text)
    {
        let printed = address_up_to_the_next_field(
            c[1].trim_matches(|ch: char| ch == '*' || ch == '\\').trim(),
        );
        let key = address_key(printed);
        // A line with a number but no street word is a date or a phone.
        if key.starts_with('|') {
            return;
        }
        let mut f = fact(FactKind::Address, page, printed);
        f.text = Some(key);
        out.push(f);
    }
}

/// The facts one page states. `request_address` and `planned` are the
/// application form's parsed slots, for that page only.
pub fn page_facts(
    page: &PageInput<'_>,
    request_address: Option<&str>,
    planned: Option<&str>,
) -> Vec<Fact> {
    let mut out = Vec::new();
    if page.text.trim().is_empty() {
        return out;
    }
    let certificate = has_label(page, "cadastral_certificate");
    let continuation = certificate && page.text.contains("ՇԻՆՈՒԹՅՈՒՆՆԵՐԻ ԲՆՈՒԹԱԳՐԵՐԸ");
    if certificate && !continuation {
        certificate_facts(page, &mut out);
    } else if continuation {
        building_rows(page, &mut out);
    } else {
        code_facts(page, &mut out);
        let mut taken: Vec<(usize, usize)> = Vec::new();
        for c in BUILDING_SUFFIX_ROW.captures_iter(page.text) {
            if let Some(area) = parse_number(&c[2]) {
                let mut f = fact(
                    FactKind::BuildingArea,
                    page,
                    &format!("{} {} քմ", &c[1], &c[2]),
                );
                f.number = Some(area);
                f.subject = Some(format!("-{}", &c[1]));
                out.push(f);
                let whole = c.get(0).expect("match");
                taken.push((whole.start(), whole.end()));
            }
        }
        context_area_facts(page, &taken, &mut out);
        for c in CERT_CITATION.captures_iter(page.text) {
            let mut f = fact(FactKind::CertificateNumber, page, &c[1]);
            f.text = Some(c[1].to_string());
            out.push(f);
        }
    }
    if let Some(planned) = planned {
        planned_facts(page, planned, &mut out);
    }
    name_facts(page, &mut out);
    address_facts(page, request_address, &mut out);
    // The form's planned line is also part of the form's text: one reading
    // of each figure, not two.
    let mut seen: Vec<(FactKind, Option<String>, Option<String>, String)> = Vec::new();
    out.retain(|f| {
        let key = (f.kind, f.subject.clone(), f.text.clone(), f.printed.clone());
        if seen.contains(&key) {
            false
        } else {
            seen.push(key);
            true
        }
    });
    out
}

/// The facts a placed sheet's turning-point table implies, from the
/// `survey_parcel` feature `vector` exported for it.
pub fn survey_facts(page: &PageInput<'_>, survey: &serde_json::Value) -> Vec<Fact> {
    let props = &survey["properties"];
    let mut out = Vec::new();
    if let Some(area) = props["area_m2"].as_f64() {
        let mut f = fact(FactKind::SurveyArea, page, &format!("{area:.2} m²"));
        f.number = Some(area);
        out.push(f);
    }
    if let Some(perimeter) = props["perimeter_m"].as_f64() {
        let mut f = fact(
            FactKind::SurveyPerimeter,
            page,
            &format!("{perimeter:.2} m"),
        );
        f.number = Some(perimeter);
        out.push(f);
    }
    if let Some(points) = props["turning_points"].as_u64() {
        let mut f = fact(FactKind::TurningPoints, page, &points.to_string());
        f.number = Some(points as f64);
        out.push(f);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input<'a>(page: usize, labels: &'a [String], text: &'a str, trust: Trust) -> PageInput<'a> {
        PageInput {
            page,
            labels,
            text,
            trust,
            via: Via::EmbeddedText,
        }
    }

    fn labels(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn an_album_place_date_stamp_is_not_part_of_the_address() {
        let printed =
            address_up_to_the_next_field("ՆԵՐՔԻՆ ՉԱՐԲԱԽԻ 4-ՐԴ ՓՈՂՈՑ 63/2 ՀՈՂԱՄԱՍ ԵՐԵՎԱՆ 2026");
        assert_eq!(printed, "ՆԵՐՔԻՆ ՉԱՐԲԱԽԻ 4-ՐԴ ՓՈՂՈՑ 63/2 ՀՈՂԱՄԱՍ");
        assert_eq!(
            same_address(
                &address_key(printed),
                &address_key("Ներքին Չարբախի 4-րդ փողոց 63/2 հողամաս")
            ),
            Some(true)
        );
        // A leading city is the address's own, not a stamp: nothing follows it.
        assert_eq!(
            address_up_to_the_next_field("ք. Երևան, Մալաթիա 48"),
            "ք. Երևան, Մալաթիա 48"
        );
    }

    #[test]
    fn a_certificate_states_code_area_owner_number_and_address() {
        let text = "1. ԳՐԱՆՑՎԱԾ ԻՐԱՎՈՒՆՔԻ ՍՈՒԲՅԵԿՏ(ՆԵՐ)\nԱՆՈՒՆ ԱԶԳԱՆՈՒՆՅԱՆ ՀԱՅՐԻ\n2. ԱՆՇԱՐԺ ԳՈՒՅՔԻ ԳՏՆՎԵԼՈՒ ՎԱՅՐԸ ԵՎ ԱՆՎԱՆՈՒՄԸ\nՔ.Երևան, Շենգավիթ, Օրինակ փողոց 1/1 բնակելի տուն\n4. ՀՈՂԱՄԱՍԻ ԲՆՈՒԹԱԳՐԵՐԸ\nԿադաստրային ծածկագիրը՝ 01-011-0000-0001 Մակերեսի չափը (հա)՝ 0.02612 Նպատակային\nՎԿԱՅԱԿԱՆ N 20300101-01-0001, գաղտնաբառ՝ 0AAAA0AAA0AA";
        let l = labels(&["cadastral_certificate", "text_document"]);
        let facts = page_facts(&input(5, &l, text, Trust::Official), None, None);
        let kinds: Vec<FactKind> = facts.iter().map(|f| f.kind).collect();
        assert_eq!(
            kinds,
            [
                FactKind::CadastralCode,
                FactKind::ParcelArea,
                FactKind::CertificateNumber,
                FactKind::Owner,
                FactKind::Address
            ]
        );
        assert_eq!(facts[1].number, Some(261.2));
        assert_eq!(facts[3].text.as_deref(), Some("person:anun-azganunyan"));
        assert_eq!(facts[4].text.as_deref(), Some("շենգավի օրինակ|1/1"));
        assert!(
            facts
                .iter()
                .all(|f| f.about.as_deref() == Some("01-011-0000-0001"))
        );
    }

    #[test]
    fn building_rows_and_suffix_rows_are_read() {
        let l = labels(&["cadastral_certificate", "table"]);
        let text = "5. ՇԻՆՈՒԹՅՈՒՆՆԵՐԻ ԲՆՈՒԹԱԳՐԵՐԸ\nԲնակելի 1 01-011-0912-0146-001 11.9 քմ ՍԵՓԱԿԱՆՈՒԹՅՈՒՆ տուն\n2 01-011-0912-0146-002 ծածկ 14 քմ";
        let facts = page_facts(&input(6, &l, text, Trust::Official), None, None);
        assert_eq!(facts.len(), 2);
        assert_eq!(facts[0].subject.as_deref(), Some("01-011-0912-0146-001"));
        assert_eq!(facts[1].number, Some(14.0));

        let l = labels(&["text_document"]);
        let note = "մտնում են ՝ 001 Բնակելի տուն-40.00մ2 002 Օժանդակ շինություն-19.25մ2";
        let facts = page_facts(&input(15, &l, note, Trust::Applicant), None, None);
        let areas: Vec<(Option<&str>, Option<f64>)> = facts
            .iter()
            .map(|f| (f.subject.as_deref(), f.number))
            .collect();
        assert_eq!(
            areas,
            [(Some("-001"), Some(40.0)), (Some("-002"), Some(19.25))]
        );
    }

    #[test]
    fn context_words_classify_areas_and_exclusions_hold() {
        let l = labels(&["text_document"]);
        let text = "Հողամասի մակերեսի չափը կազմում է 261 քմ։ Խոհանոց (12.05 քմ) -Առանձնատան կառուցապատման մակերես 98 մ2 Հարկ 2 ՝105,5 քմ (ներառյալ ցանկապատ և պարիսպ )6,40քմ -Կանաչապատման մակերես 156.6 մ2";
        let facts = page_facts(&input(8, &l, text, Trust::Applicant), None, None);
        let got: Vec<(FactKind, Option<&str>, f64)> = facts
            .iter()
            .map(|f| (f.kind, f.subject.as_deref(), f.number.unwrap()))
            .collect();
        assert_eq!(
            got,
            [
                (FactKind::ParcelArea, None, 261.0),
                (FactKind::FootprintArea, None, 98.0),
                (FactKind::PlannedArea, Some("fence"), 6.4),
            ]
        );
    }

    #[test]
    fn the_form_names_the_applicant_and_the_planned_items() {
        let l = labels(&["application_form", "text_document"]);
        let text = "Ձև N 1-1\n**Նարինե Ավետիսյանից**\nԽնդրում եմ";
        let facts = page_facts(
            &input(1, &l, text, Trust::Applicant),
            Some("Երևան, Շենգավիթ Արշակունյաց պողոտա 212/6 բնակելի տուն"),
            Some(
                "Հիմնական շենք՝ 12.4x6.3մ, ցանկապատ՝ 10.2x0.2մ, Հիմնական շենք /առանձնատուն/ 98քմ, ցանկապատ 2.0քմ",
            ),
        );
        let got: Vec<(FactKind, Option<&str>)> = facts
            .iter()
            .map(|f| (f.kind, f.subject.as_deref()))
            .collect();
        assert_eq!(
            got,
            [
                (FactKind::PlannedArea, Some("building")),
                (FactKind::PlannedArea, Some("fence")),
                (FactKind::Applicant, None),
                (FactKind::Address, None),
            ]
        );
        assert_eq!(facts[2].text.as_deref(), Some("person:avetisyan-narine"));
        assert_eq!(facts[3].text.as_deref(), Some("արշակու շենգավի|212/6"));
        assert_eq!(
            same_address("արշակու շենգավի|212/6", "արշակու|212/6"),
            Some(true)
        );
        assert_eq!(same_address("արշակու|212/6", "վարդանա|47/8"), Some(false));
    }

    #[test]
    fn a_reference_letter_names_who_asked_and_a_fee_table_its_partial_code() {
        let l = labels(&["cadastral_reference", "text_document"]);
        let text = "ՏԵՂԵԿԱՆՔ\nԴիմող՝\nԱՐՄԱՆ ՀՈՎՀԱՆՆԻՍՅԱՆ\nԻ պատասխան";
        let facts = page_facts(&input(12, &l, text, Trust::Official), None, None);
        assert_eq!(facts[0].text.as_deref(), Some("person:arman-hovhannisyan"));
        let fee = "Դիմող՝ Արման Հովհաննիսյան Տրամադրված տեղեկատվությունը 19.05.2026թ. 1 Երևան Նոր Նորք 0731-0612 1 100";
        let facts = page_facts(&input(13, &l, fee, Trust::Official), None, None);
        assert!(facts.iter().any(|f| f.kind == FactKind::CadastralCode
            && f.subject.as_deref() == Some("partial")
            && f.text.as_deref() == Some("0731-0612")));
    }

    #[test]
    fn a_map_listing_codes_with_single_spaces_yields_every_code() {
        let l = labels(&["cad_drawing", "cadastral_plan"]);
        let text = "01-011-0388-0017 01-011-0388-0018 01-011-0388-0017 01-011-1643-0003 \
                    01-011-0388-0015 01-011-0388-0007";
        let facts = page_facts(&input(2, &l, text, Trust::Official), None, None);
        let mut codes: Vec<&str> = facts
            .iter()
            .filter(|f| f.kind == FactKind::CadastralCode)
            .filter_map(|f| f.text.as_deref())
            .collect();
        codes.sort_unstable();
        assert_eq!(
            codes,
            [
                "01-011-0388-0007",
                "01-011-0388-0015",
                "01-011-0388-0017",
                "01-011-0388-0018",
                "01-011-1643-0003"
            ]
        );
        assert!(facts.iter().all(|f| f.subject.as_deref() == Some("map")));
    }

    #[test]
    fn a_code_inside_a_longer_run_of_digits_is_not_read() {
        let l = labels(&["text_document"]);
        let text = "N 101-011-0388-0007 and 01-011-0388-00071 and 0388-0007-7";
        let facts = page_facts(&input(3, &l, text, Trust::Applicant), None, None);
        assert!(facts.iter().all(|f| f.kind != FactKind::CadastralCode));
        let fee = "Տրամադրված տեղեկատվությունը 0731-0612 0731-0613 01-011-0731-0614";
        let facts = page_facts(&input(4, &l, fee, Trust::Official), None, None);
        let mut partial: Vec<&str> = facts
            .iter()
            .filter(|f| f.subject.as_deref() == Some("partial"))
            .filter_map(|f| f.text.as_deref())
            .collect();
        partial.sort_unstable();
        assert_eq!(partial, ["0731-0612", "0731-0613"]);
    }
}
