//! What the dossier asks for, read off the Ձև N 1-1 application form.
//!
//! The form has one fixed slot for the request — the line after «Խնդրում եմ
//! տալ նախագծման թույլտվություն» — and the applicant fills it from a closed
//! list the form itself prints on the next line: կառուցում, վերակառուցում,
//! վերականգնում, ուժեղացում, գործառնական նշանակության փոփոխում. That makes
//! the *action* a lookup, not a reading. The object description, the site
//! address and the «Նախատեսում եմ» parameters line are carried verbatim;
//! the intended *use*, secondary *works* and *storeys* are keyword facts
//! over those verbatim strings, so a reviewer can always see what they were
//! derived from. Where the form names no use, the other embedded-text pages
//! are consulted — an album title block saying ԲՆԱԿԵԼԻ ՏՈՒՆ — but never the
//! cadastre documents, whose «բնակելի կառուցապատման» is the land's
//! designation rather than the applicant's request.

use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;

use crate::report::Via;

/// What the applicant asks permission for, in the form's own vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// կառուցում
    NewConstruction,
    /// վերակառուցում
    Reconstruction,
    /// վերականգնում
    Restoration,
    /// ուժեղացում
    Strengthening,
    /// գործառնական նշանակության փոփոխում
    ChangeOfUse,
    /// The slot is filled with something outside the form's list.
    Other,
}

/// The intended use of what is built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Use {
    Residential,
    Commercial,
    Industrial,
    /// Both residential and commercial/industrial keywords.
    Mixed,
}

/// Secondary works the request names beside the main structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Work {
    Demolition,
    Annex,
    Extension,
    Fence,
    Garage,
    Basement,
    Attic,
    Canopy,
    SummerKitchen,
    Replanning,
}

/// The request block of the dossier report.
#[derive(Debug, Serialize)]
pub struct Request {
    /// The application-form page the request was read from.
    pub page: usize,
    pub via: Via,
    pub action: Action,
    /// The object description filled in beside the action, verbatim.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object: Option<String>,
    #[serde(rename = "use", skip_serializing_if = "Option::is_none")]
    pub intended_use: Option<Use>,
    /// The page whose text named the use — the form itself, or the first
    /// other embedded-text page that did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub use_page: Option<usize>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub works: Vec<Work>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storeys: Option<u64>,
    /// The site address as the form states it, with the cadastral code
    /// removed (it is an entity of its own).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    /// The «1. Նախատեսում եմ» parameters line, verbatim: dimensions, areas,
    /// whatever the applicant typed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub planned: Option<String>,
}

/// One page's contribution to the search: its number, labels and embedded
/// text.
pub struct PageText<'a> {
    pub number: usize,
    pub labels: &'a [String],
    pub text: &'a str,
}

const REQUEST_HEADER: &str = "Խնդրում եմ տալ նախագծման թույլտվություն";
const ADDRESS_HEADER: &str = "Նախագծման համար";
const ADDRESS_FOOTER: &str = "գտնվող հողամասում";
const PLANNED_HEADER: &str = "Նախատեսում եմ";

static CADASTRAL_CODE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\d{2}-\s*\d{3}-\s*\d{4}-\s*\d{4}(?:-\d{3})?").expect("static regex")
});
static STOREYS_DIGITS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d+)\s*-?\s*հարկ").expect("static regex"));

/// Reads the request from the first application form that states one.
pub fn detect(pages: &[PageText<'_>]) -> Option<Request> {
    let form = pages
        .iter()
        .filter(|p| p.labels.iter().any(|l| l == "application_form"))
        .find_map(|p| form_fields(p.text).map(|fields| (p.number, fields)));
    let (page, fields) = form?;

    let evidence = format!(
        "{} {}",
        fields.object.as_deref().unwrap_or_default(),
        fields.planned.as_deref().unwrap_or_default()
    )
    .to_lowercase();
    let (intended_use, use_page) = match use_in(&evidence) {
        Some(found) => (Some(found), Some(page)),
        None => pages
            .iter()
            .filter(|p| p.number != page && !is_cadastre_document(p.labels))
            .find_map(|p| use_in(&p.text.to_lowercase()).map(|found| (Some(found), Some(p.number))))
            .unwrap_or((None, None)),
    };

    Some(Request {
        page,
        via: Via::EmbeddedText,
        action: fields.action,
        object: fields.object,
        intended_use,
        use_page,
        works: works_in(&evidence),
        storeys: storeys_in(&evidence),
        address: fields.address,
        planned: fields.planned,
    })
}

fn is_cadastre_document(labels: &[String]) -> bool {
    labels
        .iter()
        .any(|l| l == "cadastral_certificate" || l == "cadastral_reference")
}

struct FormFields {
    action: Action,
    object: Option<String>,
    address: Option<String>,
    planned: Option<String>,
}

/// The form's fixed slots, or `None` when the request slot is empty.
fn form_fields(text: &str) -> Option<FormFields> {
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let statement = slot_after(&lines, REQUEST_HEADER, |l| l.starts_with("(կառուցում"))?;
    let (action, object) = split_statement(&statement);

    let address = slot_after(&lines, ADDRESS_HEADER, |l| l.starts_with('('))
        .map(|slot| clean_address(&slot))
        .filter(|a| !a.is_empty());
    let planned = slot_after(&lines, PLANNED_HEADER, |l| {
        l.starts_with('(') || l.starts_with("2.")
    })
    .map(|slot| clean_planned(&slot))
    .filter(|p| !p.is_empty());

    Some(FormFields {
        action,
        object,
        address,
        planned,
    })
}

/// The text of one slot: whatever follows `header` on its own line plus
/// every following line up to the legend that closes it, joined on spaces.
/// A parenthesised remark right after the header — the request header's
/// own «(Ճարտարապետահատակագծային առաջադրանք)» — is the form's, not the
/// applicant's. `None` when the slot holds nothing.
fn slot_after(lines: &[&str], header: &str, closes: impl Fn(&str) -> bool) -> Option<String> {
    let start = lines.iter().position(|l| l.contains(header))?;
    let mut parts: Vec<&str> = Vec::new();
    let inline = lines[start]
        .split_once(header)
        .map_or("", |(_, rest)| rest.trim());
    if !inline.is_empty() && !(inline.starts_with('(') && inline.ends_with(')')) {
        parts.push(inline);
    }
    for line in lines.iter().skip(start + 1) {
        if line.is_empty() || closes(line) {
            break;
        }
        parts.push(line);
    }
    let joined = parts.join(" ").trim().to_string();
    (!joined.is_empty()).then_some(joined)
}

/// The action word the applicant chose, and what follows it verbatim.
fn split_statement(statement: &str) -> (Action, Option<String>) {
    let lower = statement.to_lowercase();
    let action = if lower.starts_with("վերակառուցում") {
        Action::Reconstruction
    } else if lower.starts_with("վերականգնում") {
        Action::Restoration
    } else if lower.starts_with("ուժեղացում") {
        Action::Strengthening
    } else if lower.starts_with("գործառնական") {
        Action::ChangeOfUse
    } else if lower.starts_with("կառուցում") {
        Action::NewConstruction
    } else {
        Action::Other
    };
    let remainder = if action == Action::Other {
        statement.trim()
    } else {
        let word_end = statement
            .find(char::is_whitespace)
            .unwrap_or(statement.len());
        statement[word_end..].trim()
    };
    let object = remainder
        .strip_prefix('(')
        .and_then(|r| r.strip_suffix(')'))
        .filter(|inner| !inner.contains(')'))
        .unwrap_or(remainder)
        .trim();
    (action, (!object.is_empty()).then(|| object.to_string()))
}

fn clean_address(slot: &str) -> String {
    let without_footer = slot.split(ADDRESS_FOOTER).next().unwrap_or(slot);
    let without_footer = without_footer
        .trim_end()
        .strip_suffix("գտնվող")
        .unwrap_or(without_footer);
    let without_code = CADASTRAL_CODE.replace_all(without_footer, "");
    collapse(&without_code)
        .trim_matches(|c: char| c == ',' || c.is_whitespace())
        .to_string()
}

fn clean_planned(slot: &str) -> String {
    collapse(slot)
        .trim_matches(|c: char| c == ',' || c.is_whitespace())
        .to_string()
}

/// Whitespace-collapsed copy, with a comma that a line break separated from
/// its word pulled back.
fn collapse(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace(" ,", ",")
}

const RESIDENTIAL: &[&str] = &["բնակելի", "առանձնատ"];
const COMMERCIAL: &[&str] = &[
    "առևտր",
    "խանութ",
    "գրասենյակ",
    "հասարակական",
    "սրճարան",
    "ռեստորան",
    "հյուրանոց",
    "սպասարկ",
    "կոմերցիոն",
    "օֆիս",
];
const INDUSTRIAL: &[&str] = &["արտադր", "արդյունաբեր", "պահեստ", "գործարան"];

/// The use the (lowercased) text names, if any.
fn use_in(lower: &str) -> Option<Use> {
    let any = |keys: &[&str]| keys.iter().any(|k| lower.contains(k));
    match (any(RESIDENTIAL), any(COMMERCIAL) || any(INDUSTRIAL)) {
        (true, true) => Some(Use::Mixed),
        (true, false) => Some(Use::Residential),
        (false, true) => Some(if any(COMMERCIAL) {
            Use::Commercial
        } else {
            Use::Industrial
        }),
        (false, false) => None,
    }
}

/// Secondary works the (lowercased) text names. «առանց նկուղի» — *without*
/// a basement — is the one negation the forms actually use.
fn works_in(lower: &str) -> Vec<Work> {
    let has = |key: &str| lower.contains(key);
    let basement = lower.match_indices("նկուղ").any(|(at, _)| {
        let before = &lower[..at];
        !before.trim_end().ends_with("առանց")
    });
    let mut works = Vec::new();
    for (present, work) in [
        (has("քանդ"), Work::Demolition),
        (has("կցակառույց"), Work::Annex),
        (has("ընդլայն"), Work::Extension),
        (
            has("պարիսպ") || has("պարսպ") || has("ցանկապատ"),
            Work::Fence,
        ),
        (has("ավտոտնակ"), Work::Garage),
        (basement, Work::Basement),
        (has("ձեղնահարկ"), Work::Attic),
        (has("ծածկոց"), Work::Canopy),
        (has("ամառային խոհանոց"), Work::SummerKitchen),
        (has("վերահատակագծ"), Work::Replanning),
    ] {
        if present {
            works.push(work);
        }
    }
    works
}

/// The storey count the (lowercased) text states — as a digit or as the
/// Armenian compound (երկհարկանի, միահարկ …).
fn storeys_in(lower: &str) -> Option<u64> {
    let collapsed = collapse(lower);
    if let Some(capture) = STOREYS_DIGITS.captures(&collapsed) {
        return capture[1].parse().ok();
    }
    const WORDS: &[(&str, u64)] = &[
        ("միահարկ", 1),
        ("մեկ հարկ", 1),
        ("մեկհարկ", 1),
        ("երկհարկ", 2),
        ("երկու հարկ", 2),
        ("եռահարկ", 3),
        ("երեք հարկ", 3),
        ("քառահարկ", 4),
        ("չորս հարկ", 4),
        ("հնգահարկ", 5),
        ("հինգ հարկ", 5),
    ];
    WORDS
        .iter()
        .find(|(word, _)| collapsed.contains(word))
        .map(|(_, n)| *n)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic Ձև N 1-1 form in the corpus's layout: each filled slot
    /// is followed by the form's own parenthesised legend.
    fn form(statement: &str, planned: &str) -> String {
        format!(
            "Ձև N 1-1\nՀ Ա Յ Տ\nԾՀԱ/01/00001-30\n\
             Խնդրում եմ տալ նախագծման թույլտվություն (Ճարտարապետահատակագծային առաջադրանք)\n\
             {statement}\n\
             (կառուցում, վերակառուցում, վերականգնում, ուժեղացում, գործառնական նշանակության փոփոխում, օբյեկտի անվանումը, հակիրճ բնորոշումը)\n\
             Նախագծման համար Երևան, Օրինակ վարչական շրջան Նմուշի 1-ին փողոց 2/3 հողամաս,\n\
             10-001-0456-0123 գտնվող հողամասում\n\
             (մարզի, համայնքի, փողոցի անվանումները, շենքի համարը, հողամասի ծածկագիրը)\n\
             1. Նախատեսում եմ {planned}\n\
             (օբյեկտի գործառնական նշանակությունը /այդ թվում՝ փոփոխվող, նախատեսվող)\n\
             2. Կից ներկայացնում եմ՝"
        )
    }

    fn detect_form(statement: &str, planned: &str) -> Request {
        let text = form(statement, planned);
        let labels = vec!["application_form".to_string(), "text_document".to_string()];
        detect(&[PageText {
            number: 1,
            labels: &labels,
            text: &text,
        }])
        .expect("a filled form states a request")
    }

    #[test]
    fn a_new_house_with_a_fence_is_read_off_the_form() {
        let request = detect_form(
            "Կառուցում (Երկհարկանի բնակելի տուն և պարիսպ)",
            ", 9,72 մ x7.62մ, 119,72 մ2",
        );
        assert_eq!(request.action, Action::NewConstruction);
        assert_eq!(
            request.object.as_deref(),
            Some("Երկհարկանի բնակելի տուն և պարիսպ")
        );
        assert_eq!(request.intended_use, Some(Use::Residential));
        assert_eq!(request.use_page, Some(1));
        assert_eq!(request.works, [Work::Fence]);
        assert_eq!(request.storeys, Some(2));
        assert_eq!(
            request.address.as_deref(),
            Some("Երևան, Օրինակ վարչական շրջան Նմուշի 1-ին փողոց 2/3 հողամաս")
        );
        assert_eq!(request.planned.as_deref(), Some("9,72 մ x7.62մ, 119,72 մ2"));
        assert_eq!(request.page, 1);
    }

    #[test]
    fn reconstruction_with_demolition_of_an_annex_names_both_works() {
        let request = detect_form(
            "Վերակառուցում (Օժանդակ կցակառույցի քանդում և նորի կառուցում, մասնակի վերահատակագծում)",
            ", Օժանդակ կցակառույց 5.22x4.54",
        );
        assert_eq!(request.action, Action::Reconstruction);
        assert_eq!(
            request.works,
            [Work::Demolition, Work::Annex, Work::Replanning]
        );
        assert!(
            request.intended_use.is_none(),
            "an auxiliary annex names no use"
        );
        assert!(request.storeys.is_none());
    }

    #[test]
    fn without_a_basement_is_not_a_basement_and_digits_count_storeys() {
        let request = detect_form(
            "Կառուցում (Երկհարկանի առանց նկուղի)",
            ", տունը բաղկացած է 2\nհարկից",
        );
        assert!(!request.works.contains(&Work::Basement));
        assert_eq!(request.storeys, Some(2));

        let with = detect_form(
            "Կառուցում (Երկու հարկանի բնակելի տուն, նկուղային հարկով)",
            "",
        );
        assert_eq!(with.works, [Work::Basement]);
        assert_eq!(with.storeys, Some(2));
        let semi = detect_form(
            "Վերակառուցում (միահարկ առանձնատուն ձեղնահարկով և կիսանկուղային հարկուվ, ծածկոց)",
            "",
        );
        assert_eq!(semi.works, [Work::Basement, Work::Attic, Work::Canopy]);
        assert_eq!(semi.storeys, Some(1));
        assert_eq!(semi.intended_use, Some(Use::Residential));
    }

    #[test]
    fn the_use_falls_back_to_other_pages_but_never_to_cadastre_documents() {
        let text = form("Կառուցում (Երկհարկանի առանց նկուղի)", ", 9x7 մ");
        let form_labels = vec!["application_form".to_string()];
        let certificate_labels = vec!["cadastral_certificate".to_string()];
        let album_labels = vec!["cad_drawing".to_string(), "floor_plan".to_string()];
        let pages = [
            PageText {
                number: 1,
                labels: &form_labels,
                text: &text,
            },
            PageText {
                number: 4,
                labels: &certificate_labels,
                text: "Գործառնական նշանակությունը՝ բնակելի կառուցապատման",
            },
            PageText {
                number: 12,
                labels: &album_labels,
                text: "ԲՆԱԿԵԼԻ ՏՈՒՆ 1-ԻՆ ՀԱՐԿԻ ՀԱՏԱԿԱԳԻԾ",
            },
        ];
        let request = detect(&pages).expect("request");
        assert_eq!(request.intended_use, Some(Use::Residential));
        assert_eq!(
            request.use_page,
            Some(12),
            "the certificate's land use is not the request"
        );
    }

    #[test]
    fn mixed_and_other_are_told_apart_and_an_empty_slot_is_no_request() {
        let mixed = detect_form("Կառուցում (բնակելի և առևտրային շենք)", "");
        assert_eq!(mixed.intended_use, Some(Use::Mixed));
        let other = detect_form("Բարեկարգում (բակի սալարկում)", "");
        assert_eq!(other.action, Action::Other);
        assert_eq!(
            other.object.as_deref(),
            Some("Բարեկարգում (բակի սալարկում)")
        );

        let blank = form("", "");
        let labels = vec!["application_form".to_string()];
        assert!(
            detect(&[PageText {
                number: 1,
                labels: &labels,
                text: &blank
            }])
            .is_none()
        );
        assert!(detect(&[]).is_none());
    }

    #[test]
    fn enums_serialize_to_their_contract_strings() {
        assert_eq!(
            serde_json::to_value(Action::ChangeOfUse).unwrap(),
            "change_of_use"
        );
        assert_eq!(
            serde_json::to_value(Use::Residential).unwrap(),
            "residential"
        );
        assert_eq!(
            serde_json::to_value(Work::SummerKitchen).unwrap(),
            "summer_kitchen"
        );
    }
}
