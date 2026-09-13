//! Who vouches for a page.
//!
//! A permit dossier mixes pages the Cadastre Committee issued — the ownership
//! certificate, its map extracts with the turning-point table, the reference
//! letter and its fee table — with pages the applicant or their architect
//! wrote. The first kind is the record of what the parcel is; the second is
//! a claim about it. Every cross-page check treats them that way: an
//! official figure is the reference, and an applicant's copy of it is what
//! gets checked.

use serde::Serialize;

/// Which side of that line a page (or a group of pages) is on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Trust {
    /// Issued by the Cadastre Committee: the parcel's record.
    Official,
    /// Written by the applicant or their designer: a claim about the parcel.
    Applicant,
    /// Identity documents: about a person, not the parcel.
    Identity,
    /// A page nothing could read yet — a scan awaiting the vision pass.
    #[default]
    Unknown,
}

/// Labels that only the Cadastre Committee's own documents carry.
const OFFICIAL_LABELS: &[&str] = &["cadastral_certificate", "cadastral_reference"];
const IDENTITY_LABELS: &[&str] = &["id_card", "passport", "social_card"];

/// Phrases printed on the Committee's map extracts and fee tables (matched
/// on the legacy-decoded, lower-cased text).
const OFFICIAL_MARKERS: &[&str] = &[
    "կադաստրի կոմիտե",
    "կադաստրային քարտեզից",
    "e-cadastre.am",
    "տրամադրված տեղեկատվությունը",
];

/// The trust of a page from its labels and (decoded) text. `readable` is
/// false for a page nothing has read yet; such a page is `Unknown` unless a
/// vision answer labelled it. `extract` says the page looks like one of the
/// Committee's map extracts (see [`looks_like_extract`]), whose heading is
/// usually unreadable to a binary.
pub fn page_trust(labels: &[String], text: &str, readable: bool, extract: bool) -> Trust {
    let has = |set: &[&str]| labels.iter().any(|l| set.contains(&l.as_str()));
    if has(IDENTITY_LABELS) {
        return Trust::Identity;
    }
    if has(OFFICIAL_LABELS) || extract {
        return Trust::Official;
    }
    let lower = text.to_lowercase();
    if OFFICIAL_MARKERS.iter().any(|m| lower.contains(m)) {
        return Trust::Official;
    }
    if !readable && labels.iter().all(|l| l == "unknown") {
        return Trust::Unknown;
    }
    Trust::Applicant
}

/// Whether a map page is one of the Committee's extracts rather than the
/// architect's plan of the same site: it carries a turning-point table or
/// several parcel codes, and no sheet designation or drawing title — the
/// extract's own heading is set in a font a binary cannot read, so the
/// absence of the architect's marks is what identifies it.
pub fn looks_like_extract(
    is_map_class: bool,
    has_control_table: bool,
    parcel_codes: usize,
    sheet: Option<&str>,
    text: &str,
) -> bool {
    let titled = text.to_lowercase().contains("հատակագիծ");
    is_map_class && sheet.is_none() && !titled && (has_control_table || parcel_codes >= 2)
}

/// A group's trust: official if any of its pages is, else identity if any
/// is, else applicant — a group is one document, and one official page in
/// it makes the document the Committee's.
pub fn group_trust(page_trusts: impl IntoIterator<Item = Trust>) -> Trust {
    let mut seen_identity = false;
    let mut seen_applicant = false;
    for trust in page_trusts {
        match trust {
            Trust::Official => return Trust::Official,
            Trust::Identity => seen_identity = true,
            Trust::Applicant => seen_applicant = true,
            Trust::Unknown => {}
        }
    }
    if seen_identity && !seen_applicant {
        Trust::Identity
    } else if seen_applicant || seen_identity {
        Trust::Applicant
    } else {
        Trust::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn labels_and_markers_decide() {
        assert_eq!(
            page_trust(&labels(&["cadastral_certificate"]), "", true, false),
            Trust::Official
        );
        assert_eq!(
            page_trust(
                &labels(&["cad_drawing", "topographic_survey"]),
                "Հատված կադաստրային քարտեզից",
                true,
                false
            ),
            Trust::Official
        );
        assert_eq!(
            page_trust(
                &labels(&["cad_drawing", "site_plan"]),
                "Գլխավոր հատակագիծ",
                true,
                false
            ),
            Trust::Applicant
        );
        assert_eq!(
            page_trust(
                &labels(&["cad_drawing", "cadastral_plan"]),
                "0013 0014",
                true,
                true
            ),
            Trust::Official
        );
        assert_eq!(
            page_trust(&labels(&["id_card", "photo"]), "", false, false),
            Trust::Identity
        );
        assert_eq!(
            page_trust(&labels(&["unknown"]), "", false, false),
            Trust::Unknown
        );
    }

    #[test]
    fn an_extract_has_a_table_or_codes_and_no_title() {
        assert!(looks_like_extract(
            true,
            true,
            0,
            None,
            "Հ/հ Y X Հեռավորությունը"
        ));
        assert!(looks_like_extract(
            true,
            false,
            3,
            None,
            "01-011-0388-0017 01-011-0388-0018"
        ));
        assert!(!looks_like_extract(
            true,
            true,
            3,
            Some("Է.03"),
            "Գլխավոր հատակագիծ"
        ));
        assert!(!looks_like_extract(
            true,
            false,
            3,
            None,
            "Իրավիճակային գլխավոր հատակագիծ Մ 1։2000"
        ));
        assert!(!looks_like_extract(false, false, 0, None, ""));
    }

    #[test]
    fn one_official_page_makes_the_group_official() {
        assert_eq!(
            group_trust([Trust::Applicant, Trust::Official]),
            Trust::Official
        );
        assert_eq!(group_trust([Trust::Identity]), Trust::Identity);
        assert_eq!(group_trust([Trust::Unknown]), Trust::Unknown);
    }
}
