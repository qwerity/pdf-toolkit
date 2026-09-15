//! Every figure the dossier states more than once, compared across the
//! pages that state it — the official page as the reference where there is
//! one, the applicant's copies checked against it, and the applicant's own
//! copies checked against each other where there is none.
//!
//! A mismatch is reported, never resolved: the check names both pages and
//! both values and says how far apart they are. The rule the toolkit keeps
//! everywhere applies here too — a disagreement between two sources is a
//! finding, not something to settle by picking a side — with one
//! qualification: when one side is the Cadastre Committee's own record,
//! the reader is told which side that is.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::access::{AccessKind, DocumentAccess};
use crate::facts::{Fact, FactKind, same_address};
use crate::trust::Trust;

/// What a check compares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckKind {
    /// Every cadastral code an applicant page prints against the subject
    /// parcel's; codes of parcels the official pages name (neighbours) are
    /// allowed, any other code is foreign.
    CadastralCode,
    /// The plot's area: certificate, notes, tables, survey polygons.
    ParcelArea,
    /// A registered building's area, by building code.
    BuildingArea,
    /// A planned item's area quoted on several applicant pages.
    PlannedArea,
    /// The applicant against the registered owner(s).
    Applicant,
    /// The site address as the certificate and the applicant state it.
    Address,
    /// The certificate number as printed and as cited.
    CertificateNumber,
    /// Whether each ownership certificate prints the number and passcode
    /// that identify it to the register.
    CertificateAccess,
    /// Official pages nothing has read yet.
    OfficialReadability,
}

/// The outcome of one comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    /// Every copy agrees with the reference within tolerance.
    Agree,
    /// A copy differs by little: rounding, or one wrong digit — a probable typo.
    NearMiss,
    /// A copy differs by more than rounding explains.
    Disagree,
    /// The figure appears once; nothing to compare it with.
    SingleSource,
    /// The pages that would settle it have not been read.
    Unreadable,
}

/// One page's statement of the figure.
#[derive(Debug, Clone, Serialize)]
pub struct Witness {
    pub page: usize,
    pub trust: Trust,
    pub printed: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// How this copy compares with the reference.
    pub status: CheckStatus,
    /// `number − reference`, when both are numbers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta: Option<f64>,
}

/// One figure across the dossier.
#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub kind: CheckKind,
    /// What the figure is about when the kind needs it (a building code, a
    /// planned item).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// The worst status among the witnesses.
    pub status: CheckStatus,
    /// The page whose statement the others are measured against, official
    /// where one exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference: Option<Witness>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub witnesses: Vec<Witness>,
    /// One line, English, saying what was compared and what came of it.
    pub note: String,
}

/// An area copy within this fraction of the reference agrees; within
/// [`AREA_NEAR_FRACTION`] it is a rounding or a slip.
const AREA_AGREE_FRACTION: f64 = 0.002;
const AREA_NEAR_FRACTION: f64 = 0.02;
/// A copy printed without decimals is allowed to be the reference rounded.
const ROUNDING_M2: f64 = 0.5;

fn witness(fact: &Fact, status: CheckStatus, delta: Option<f64>) -> Witness {
    Witness {
        page: fact.page,
        trust: fact.trust,
        printed: fact.printed.clone(),
        number: fact.number,
        text: fact.text.clone(),
        status,
        delta,
    }
}

fn worst(statuses: impl IntoIterator<Item = CheckStatus>) -> CheckStatus {
    statuses.into_iter().fold(CheckStatus::Agree, |acc, s| {
        let rank = |s: CheckStatus| match s {
            CheckStatus::Disagree => 4,
            CheckStatus::NearMiss => 3,
            CheckStatus::Unreadable => 2,
            CheckStatus::SingleSource => 1,
            CheckStatus::Agree => 0,
        };
        if rank(s) > rank(acc) { s } else { acc }
    })
}

/// How a numeric copy compares with the reference.
fn compare_area(reference: f64, copy: &Fact) -> (CheckStatus, f64) {
    let value = copy.number.unwrap_or(f64::NAN);
    let delta = value - reference;
    let printed_whole = !copy.printed.contains(['.', ',', '․']);
    let agree = delta.abs() <= (AREA_AGREE_FRACTION * reference).max(0.05)
        || (printed_whole && delta.abs() <= ROUNDING_M2);
    let status = if agree {
        CheckStatus::Agree
    } else if delta.abs() <= AREA_NEAR_FRACTION * reference {
        CheckStatus::NearMiss
    } else {
        CheckStatus::Disagree
    };
    (status, delta)
}

/// Levenshtein distance, for codes and numbers that may carry one wrong digit.
pub fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur.push((prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// The subject parcel: the application form's code, else the code the
/// first official certificate is about, else the most-printed code.
fn subject_code(facts: &[Fact], application_code: Option<&str>) -> Option<String> {
    if let Some(code) = application_code {
        return Some(code.to_string());
    }
    if let Some(code) = facts
        .iter()
        .find(|f| f.kind == FactKind::CertificateNumber && f.trust == Trust::Official)
        .and_then(|f| f.about.clone())
    {
        return Some(code);
    }
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for f in facts
        .iter()
        .filter(|f| f.kind == FactKind::CadastralCode && f.subject.is_none())
    {
        if let Some(code) = f.text.as_deref() {
            *counts.entry(code).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .max_by_key(|(_, n)| *n)
        .map(|(code, _)| code.to_string())
}

/// Official facts about the subject parcel: a neighbour's certificate is
/// official too, but its owner and area are the neighbour's.
fn official_about_subject<'a>(facts: &'a [Fact], kind: FactKind, subject: &str) -> Vec<&'a Fact> {
    facts
        .iter()
        .filter(|f| f.kind == kind && f.trust == Trust::Official)
        .filter(|f| f.about.as_deref().is_none_or(|about| about == subject))
        .collect()
}

fn check_codes(facts: &[Fact], subject: &str, out: &mut Vec<Check>) {
    let known: BTreeSet<&str> = facts
        .iter()
        .filter(|f| f.kind == FactKind::CadastralCode && f.trust == Trust::Official)
        .filter_map(|f| f.text.as_deref())
        .collect();
    let subject_tail = &subject[subject.len().saturating_sub(9)..];
    let mut witnesses = Vec::new();
    for f in facts
        .iter()
        .filter(|f| f.kind == FactKind::CadastralCode && f.trust != Trust::Official)
    {
        let Some(code) = f.text.as_deref() else {
            continue;
        };
        // A map labels its neighbours and a note may name one; neighbours
        // differ from the subject by a digit or two by construction, so
        // nothing about such a code is a finding.
        if matches!(f.subject.as_deref(), Some("map" | "mention")) {
            continue;
        }
        let status = if f.subject.as_deref() == Some("partial") {
            if code == subject_tail || known.iter().any(|k| k.ends_with(code)) {
                CheckStatus::Agree
            } else {
                CheckStatus::Disagree
            }
        } else if code == subject || known.contains(code) {
            CheckStatus::Agree
        } else if edit_distance(code, subject) <= 2 {
            CheckStatus::NearMiss
        } else {
            CheckStatus::Disagree
        };
        witnesses.push(witness(f, status, None));
    }
    // Official pages other than the subject's certificate print partial
    // codes too (the fee table), and those are checked the same way.
    for f in facts.iter().filter(|f| {
        f.kind == FactKind::CadastralCode
            && f.trust == Trust::Official
            && f.subject.as_deref() == Some("partial")
    }) {
        let Some(code) = f.text.as_deref() else {
            continue;
        };
        let status = if code == subject_tail {
            CheckStatus::Agree
        } else {
            CheckStatus::Disagree
        };
        witnesses.push(witness(f, status, None));
    }
    if witnesses.is_empty() {
        return;
    }
    let status = worst(witnesses.iter().map(|w| w.status));
    let bad: Vec<String> = witnesses
        .iter()
        .filter(|w| w.status != CheckStatus::Agree)
        .map(|w| format!("page {} prints {}", w.page, w.printed))
        .collect();
    let note = match status {
        CheckStatus::Agree => format!(
            "every code on the applicant's pages is the subject parcel's ({subject}) or one the official pages name"
        ),
        _ => format!("subject parcel {subject}; {}", bad.join("; ")),
    };
    out.push(Check {
        kind: CheckKind::CadastralCode,
        subject: None,
        status,
        reference: None,
        witnesses,
        note,
    });
}

/// Numeric copies against a reference: official first, else the first copy.
fn numeric_check(
    kind: CheckKind,
    subject: Option<String>,
    official: Vec<&Fact>,
    copies: Vec<&Fact>,
    what: &str,
    out: &mut Vec<Check>,
) {
    let reference = official
        .first()
        .copied()
        .or_else(|| copies.first().copied());
    let Some(reference) = reference else { return };
    let Some(reference_value) = reference.number else {
        return;
    };
    let others: Vec<&Fact> = official
        .iter()
        .skip(1)
        .chain(copies.iter().filter(|f| !std::ptr::eq(**f, reference)))
        .copied()
        .collect();
    if others.is_empty() {
        out.push(Check {
            kind,
            subject,
            status: CheckStatus::SingleSource,
            reference: Some(witness(reference, CheckStatus::SingleSource, None)),
            witnesses: Vec::new(),
            note: format!(
                "{what} is stated once, on page {}: {}",
                reference.page, reference.printed
            ),
        });
        return;
    }
    let witnesses: Vec<Witness> = others
        .iter()
        .map(|f| {
            let (status, delta) = compare_area(reference_value, f);
            witness(f, status, Some(delta))
        })
        .collect();
    let status = worst(witnesses.iter().map(|w| w.status));
    let side = if reference.trust == Trust::Official {
        "the official"
    } else {
        "the first"
    };
    let bad: Vec<String> = witnesses
        .iter()
        .filter(|w| w.status != CheckStatus::Agree)
        .map(|w| {
            format!(
                "page {} says {} ({:+.2})",
                w.page,
                w.printed,
                w.delta.unwrap_or(0.0)
            )
        })
        .collect();
    let note = if bad.is_empty() {
        format!(
            "{what}: {} copies agree with {side} figure on page {} ({})",
            witnesses.len(),
            reference.page,
            reference.printed
        )
    } else {
        format!(
            "{what}: {side} figure on page {} is {}; {}",
            reference.page,
            reference.printed,
            bad.join("; ")
        )
    };
    out.push(Check {
        kind,
        subject,
        status,
        reference: Some(witness(reference, CheckStatus::Agree, Some(0.0))),
        witnesses,
        note,
    });
}

fn check_parcel_area(facts: &[Fact], subject: &str, out: &mut Vec<Check>) {
    let official = official_about_subject(facts, FactKind::ParcelArea, subject);
    let copies: Vec<&Fact> = facts
        .iter()
        .filter(|f| matches!(f.kind, FactKind::ParcelArea | FactKind::SurveyArea))
        .filter(|f| f.trust != Trust::Official || f.kind == FactKind::SurveyArea)
        .filter(|f| f.about.as_deref().is_none_or(|about| about == subject))
        .collect();
    if official.is_empty() && copies.is_empty() {
        return;
    }
    numeric_check(
        CheckKind::ParcelArea,
        None,
        official,
        copies,
        "parcel area",
        out,
    );
}

fn check_buildings(facts: &[Fact], subject: &str, out: &mut Vec<Check>) {
    let registered: Vec<&Fact> = facts
        .iter()
        .filter(|f| f.kind == FactKind::BuildingArea && f.trust == Trust::Official)
        .filter(|f| f.about.as_deref().is_none_or(|about| about == subject))
        .collect();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for building in &registered {
        let Some(code) = building.subject.as_deref() else {
            continue;
        };
        if !seen.insert(code.to_string()) {
            continue;
        }
        let suffix = &code[code.len() - 4..];
        let copies: Vec<&Fact> = facts
            .iter()
            .filter(|f| f.kind == FactKind::BuildingArea && f.trust != Trust::Official)
            .filter(|f| {
                f.subject
                    .as_deref()
                    .is_some_and(|s| s == code || s == suffix)
            })
            .collect();
        if copies.is_empty() {
            continue;
        }
        numeric_check(
            CheckKind::BuildingArea,
            Some(code.to_string()),
            vec![building],
            copies,
            &format!("registered building {code}"),
            out,
        );
    }
}

/// The fence, page against page: the application form's figure (else the
/// first page's) against every other page that states one. A page agrees
/// when any of its fence figures matches; a note lists several items and
/// the one that matches is the one meant. The building's area is not
/// compared this way — a note's "building area" is a footprint, a total,
/// a floor, and which one the form meant is not decidable from the words.
fn check_planned(facts: &[Fact], out: &mut Vec<Check>) {
    let fence: Vec<&Fact> = facts
        .iter()
        .filter(|f| f.kind == FactKind::PlannedArea && f.subject.as_deref() == Some("fence"))
        .collect();
    let mut by_page: BTreeMap<usize, Vec<&Fact>> = BTreeMap::new();
    for f in &fence {
        by_page.entry(f.page).or_default().push(f);
    }
    if by_page.len() < 2 {
        return;
    }
    let reference_page = fence
        .iter()
        .find(|f| {
            f.subject.is_some()
                && f.via == crate::report::Via::EmbeddedText
                && by_page.keys().next() == Some(&f.page)
        })
        .map(|f| f.page)
        .unwrap_or_else(|| *by_page.keys().next().expect("two pages"));
    let reference = by_page[&reference_page][0];
    let Some(reference_value) = reference.number else {
        return;
    };
    let mut witnesses = Vec::new();
    for (page, copies) in &by_page {
        if *page == reference_page {
            continue;
        }
        // The copy nearest the reference speaks for the page.
        let best = copies
            .iter()
            .min_by(|a, b| {
                let da = (a.number.unwrap_or(f64::NAN) - reference_value).abs();
                let db = (b.number.unwrap_or(f64::NAN) - reference_value).abs();
                da.total_cmp(&db)
            })
            .expect("non-empty");
        let (status, delta) = compare_area(reference_value, best);
        witnesses.push(witness(best, status, Some(delta)));
    }
    let status = worst(witnesses.iter().map(|w| w.status));
    let bad: Vec<String> = witnesses
        .iter()
        .filter(|w| w.status != CheckStatus::Agree)
        .map(|w| {
            format!(
                "page {} says {} ({:+.2})",
                w.page,
                w.printed,
                w.delta.unwrap_or(0.0)
            )
        })
        .collect();
    let note = if bad.is_empty() {
        format!(
            "the fence's area on page {} ({}) is repeated consistently",
            reference.page, reference.printed
        )
    } else {
        format!(
            "fence area on page {} is {}; {}",
            reference.page,
            reference.printed,
            bad.join("; ")
        )
    };
    out.push(Check {
        kind: CheckKind::PlannedArea,
        subject: Some("fence".into()),
        status,
        reference: Some(witness(reference, CheckStatus::Agree, Some(0.0))),
        witnesses,
        note,
    });
}

fn check_applicant(facts: &[Fact], subject: &str, out: &mut Vec<Check>) {
    let owners = official_about_subject(facts, FactKind::Owner, subject);
    let applicants: Vec<&Fact> = facts
        .iter()
        .filter(|f| f.kind == FactKind::Applicant)
        .collect();
    if applicants.is_empty() {
        return;
    }
    let Some(first_owner) = owners.first() else {
        out.push(Check {
            kind: CheckKind::Applicant,
            subject: None,
            status: CheckStatus::Unreadable,
            reference: None,
            witnesses: applicants
                .iter()
                .map(|f| witness(f, CheckStatus::Unreadable, None))
                .collect(),
            note:
                "no readable certificate names the registered owner to compare the applicant with"
                    .into(),
        });
        return;
    };
    let owner_keys: BTreeSet<&str> = owners.iter().filter_map(|f| f.text.as_deref()).collect();
    let witnesses: Vec<Witness> = applicants
        .iter()
        .map(|f| {
            let status = match f.text.as_deref() {
                Some(key) if owner_keys.contains(key) => CheckStatus::Agree,
                Some(key) if owner_keys.iter().any(|o| edit_distance(o, key) <= 2) => {
                    CheckStatus::NearMiss
                }
                _ => CheckStatus::Disagree,
            };
            witness(f, status, None)
        })
        .collect();
    let status = worst(witnesses.iter().map(|w| w.status));
    let bad: Vec<String> = witnesses
        .iter()
        .filter(|w| w.status != CheckStatus::Agree)
        .map(|w| format!("page {} names {}", w.page, w.printed))
        .collect();
    let owner_names: Vec<&str> = owners.iter().map(|f| f.printed.as_str()).collect();
    let note = if bad.is_empty() {
        format!(
            "the applicant is the registered owner ({})",
            owner_names.join(", ")
        )
    } else {
        format!(
            "registered owner(s) on page {}: {}; {}",
            first_owner.page,
            owner_names.join(", "),
            bad.join("; ")
        )
    };
    out.push(Check {
        kind: CheckKind::Applicant,
        subject: None,
        status,
        reference: Some(witness(first_owner, CheckStatus::Agree, None)),
        witnesses,
        note,
    });
}

fn check_address(facts: &[Fact], subject: &str, out: &mut Vec<Check>) {
    let official = official_about_subject(facts, FactKind::Address, subject);
    let copies: Vec<&Fact> = facts
        .iter()
        .filter(|f| f.kind == FactKind::Address && f.trust != Trust::Official)
        .collect();
    let Some(reference) = official
        .first()
        .copied()
        .or_else(|| copies.first().copied())
    else {
        return;
    };
    let others: Vec<&Fact> = copies
        .iter()
        .filter(|f| !std::ptr::eq(**f, reference))
        .copied()
        .collect();
    if others.is_empty() {
        return;
    }
    let key = reference.text.as_deref().unwrap_or("");
    let witnesses: Vec<Witness> = others
        .iter()
        .map(|f| {
            let status = match same_address(key, f.text.as_deref().unwrap_or("")) {
                Some(true) => CheckStatus::Agree,
                Some(false) => CheckStatus::Disagree,
                None => CheckStatus::SingleSource,
            };
            witness(f, status, None)
        })
        .collect();
    let status = worst(witnesses.iter().map(|w| w.status));
    let bad: Vec<String> = witnesses
        .iter()
        .filter(|w| w.status != CheckStatus::Agree)
        .map(|w| format!("page {} gives {}", w.page, w.printed))
        .collect();
    let note = if bad.is_empty() {
        format!(
            "the address matches page {}'s: {}",
            reference.page, reference.printed
        )
    } else {
        format!(
            "page {} gives {}; {}",
            reference.page,
            reference.printed,
            bad.join("; ")
        )
    };
    out.push(Check {
        kind: CheckKind::Address,
        subject: None,
        status,
        reference: Some(witness(reference, CheckStatus::Agree, None)),
        witnesses,
        note,
    });
}

fn check_certificate_number(facts: &[Fact], subject: &str, out: &mut Vec<Check>) {
    let official = official_about_subject(facts, FactKind::CertificateNumber, subject);
    let Some(reference) = official.first().copied() else {
        return;
    };
    let number = reference.text.as_deref().unwrap_or("");
    let witnesses: Vec<Witness> = facts
        .iter()
        .filter(|f| f.kind == FactKind::CertificateNumber && f.trust != Trust::Official)
        .map(|f| {
            let cited = f.text.as_deref().unwrap_or("");
            let status = if cited == number {
                CheckStatus::Agree
            } else if edit_distance(cited, number) <= 2 {
                CheckStatus::NearMiss
            } else {
                CheckStatus::Disagree
            };
            witness(f, status, None)
        })
        .collect();
    if witnesses.is_empty() {
        return;
    }
    let status = worst(witnesses.iter().map(|w| w.status));
    let bad: Vec<String> = witnesses
        .iter()
        .filter(|w| w.status != CheckStatus::Agree)
        .map(|w| format!("page {} cites {}", w.page, w.printed))
        .collect();
    let note = if bad.is_empty() {
        format!("every citation is the certificate's number {number}")
    } else {
        format!(
            "certificate {number} (page {}); {}",
            reference.page,
            bad.join("; ")
        )
    };
    out.push(Check {
        kind: CheckKind::CertificateNumber,
        subject: None,
        status,
        reference: Some(witness(reference, CheckStatus::Agree, None)),
        witnesses,
        note,
    });
}

/// Runs every comparison. `application_code` is the application form's
/// cadastral code; `unread_official` lists official pages routed to the
/// vision pass that no answer covers.
pub fn run(
    facts: &[Fact],
    application_code: Option<&str>,
    unread_official: &[usize],
    certificate_pages: &[(usize, Trust)],
    access: &[DocumentAccess],
) -> Vec<Check> {
    let mut out = Vec::new();
    check_certificate_access(certificate_pages, access, &mut out);
    if !unread_official.is_empty() {
        out.push(Check {
            kind: CheckKind::OfficialReadability,
            subject: None,
            status: CheckStatus::Unreadable,
            reference: None,
            witnesses: Vec::new(),
            note: format!(
                "official page(s) {} carry no readable text; their figures wait for the vision pass",
                unread_official.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", ")
            ),
        });
    }
    let Some(subject) = subject_code(facts, application_code) else {
        return out;
    };
    check_codes(facts, &subject, &mut out);
    check_parcel_area(facts, &subject, &mut out);
    check_buildings(facts, &subject, &mut out);
    check_planned(facts, &mut out);
    check_applicant(facts, &subject, &mut out);
    check_address(facts, &subject, &mut out);
    check_certificate_number(facts, &subject, &mut out);
    out
}

/// Every page that reads as an ownership certificate must print the number
/// and the passcode («գաղտնաբառ») the register asks for; a dossier with no
/// such page cannot be verified at all. Reported whenever the answer is no,
/// so the absence of a certificate is never silent.
///
/// **The passcode is never repeated here.** It is a bearer credential, and
/// `build` strips it from the page's free text for that reason; a check that
/// printed it back into `checks[]` would undo that. This records presence
/// only — which page, whose page, and whether the pair was read — and the
/// values stay in `groups[].access`.
fn check_certificate_access(
    certificate_pages: &[(usize, Trust)],
    access: &[DocumentAccess],
    out: &mut Vec<Check>,
) {
    let witnesses: Vec<Witness> = certificate_pages
        .iter()
        .map(|&(page, trust)| {
            let found = access
                .iter()
                .find(|a| a.page == page && a.kind == AccessKind::Certificate);
            Witness {
                page,
                trust,
                // The number identifies the document and is printed all over
                // it; only the passcode is withheld.
                printed: found.map_or_else(|| "—".to_string(), |a| format!("N {}", a.number)),
                number: None,
                text: found.map(|a| a.number.clone()),
                status: found.map_or(CheckStatus::Unreadable, |_| CheckStatus::Agree),
                delta: None,
            }
        })
        .collect();
    let unread: Vec<String> = witnesses
        .iter()
        .filter(|w| w.status != CheckStatus::Agree)
        .map(|w| format!("page {}", w.page))
        .collect();
    let note = if witnesses.is_empty() {
        "no page reads as a Committee ownership certificate, so it prints no number and \
         passcode to verify the register's copy against"
            .to_string()
    } else if unread.is_empty() {
        format!(
            "every certificate page prints its number and passcode ({})",
            witnesses
                .iter()
                .map(|w| format!("page {}", w.page))
                .collect::<Vec<_>>()
                .join(", ")
        )
    } else {
        format!(
            "certificate {} print no readable number and passcode",
            unread.join(", ")
        )
    };
    out.push(Check {
        kind: CheckKind::CertificateAccess,
        subject: None,
        status: if witnesses.is_empty() {
            CheckStatus::Unreadable
        } else {
            worst(witnesses.iter().map(|w| w.status))
        },
        reference: None,
        witnesses,
        note,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::Via;

    fn f(kind: FactKind, page: usize, trust: Trust, printed: &str) -> Fact {
        Fact {
            kind,
            number: None,
            text: None,
            printed: printed.into(),
            page,
            trust,
            via: Via::EmbeddedText,
            subject: None,
            about: None,
        }
    }
    fn num(kind: FactKind, page: usize, trust: Trust, printed: &str, value: f64) -> Fact {
        let mut x = f(kind, page, trust, printed);
        x.number = Some(value);
        x
    }
    fn txt(kind: FactKind, page: usize, trust: Trust, printed: &str, text: &str) -> Fact {
        let mut x = f(kind, page, trust, printed);
        x.text = Some(text.into());
        x
    }
    fn by_kind(checks: &[Check], kind: CheckKind) -> &Check {
        checks
            .iter()
            .find(|c| c.kind == kind)
            .expect("check present")
    }

    fn access(page: usize, number: &str, passcode: &str) -> DocumentAccess {
        DocumentAccess {
            kind: AccessKind::Certificate,
            number: number.into(),
            passcode: passcode.into(),
            page,
        }
    }

    #[test]
    fn a_certificate_that_prints_its_credentials_passes_and_one_that_does_not_is_reported() {
        let pages = [(5, Trust::Official), (6, Trust::Official)];
        let both = [
            access(5, "20300101-01-0001", "0AAAA0AAA0AA"),
            access(6, "20300101-01-0001", "0AAAA0AAA0AA"),
        ];
        let checks = run(&[], None, &[], &pages, &both);
        let check = by_kind(&checks, CheckKind::CertificateAccess);
        assert_eq!(check.status, CheckStatus::Agree);
        assert_eq!(check.witnesses.len(), 2);
        assert_eq!(check.witnesses[0].printed, "N 20300101-01-0001");
        // The passcode is a bearer credential: `checks[]` records that the pair was read,
        // never the secret itself. It lives once, in `groups[].access`.
        let serialised = serde_json::to_string(&checks).expect("checks serialise");
        assert!(
            !serialised.contains("0AAAA0AAA0AA"),
            "the passcode must not reach checks[]"
        );

        let checks = run(&[], None, &[], &pages, &both[..1]);
        let check = by_kind(&checks, CheckKind::CertificateAccess);
        assert_eq!(check.status, CheckStatus::Unreadable);
        assert_eq!(check.witnesses[1].status, CheckStatus::Unreadable);
        assert!(check.note.contains("page 6"), "{}", check.note);
    }

    #[test]
    fn a_dossier_without_a_certificate_page_says_so() {
        let checks = run(&[], None, &[], &[], &[]);
        let check = by_kind(&checks, CheckKind::CertificateAccess);
        assert_eq!(check.status, CheckStatus::Unreadable);
        assert!(check.witnesses.is_empty());
        assert!(check.note.contains("no page reads"), "{}", check.note);
    }

    #[test]
    fn a_rounded_copy_agrees_and_a_wrong_one_does_not() {
        let mut cert = num(
            FactKind::ParcelArea,
            5,
            Trust::Official,
            "0.02612 հա",
            261.2,
        );
        cert.about = Some("01-011-0912-0146".into());
        let facts = vec![
            cert,
            num(FactKind::ParcelArea, 8, Trust::Applicant, "261 քմ", 261.0),
            num(
                FactKind::SurveyArea,
                3,
                Trust::Official,
                "261.21 m²",
                261.21,
            ),
            num(FactKind::ParcelArea, 12, Trust::Applicant, "265 քմ", 265.0),
        ];
        let checks = run(&facts, Some("01-011-0912-0146"), &[], &[], &[]);
        let area = by_kind(&checks, CheckKind::ParcelArea);
        assert_eq!(area.status, CheckStatus::NearMiss);
        let statuses: Vec<CheckStatus> = area.witnesses.iter().map(|w| w.status).collect();
        assert_eq!(
            statuses,
            [
                CheckStatus::Agree,
                CheckStatus::Agree,
                CheckStatus::NearMiss
            ]
        );
        assert_eq!(area.reference.as_ref().unwrap().page, 5);
    }

    #[test]
    fn a_foreign_code_is_reported_and_a_neighbours_is_not() {
        let mut neighbour = txt(
            FactKind::CadastralCode,
            14,
            Trust::Official,
            "01-011-0912-0077",
            "01-011-0912-0077",
        );
        neighbour.about = Some("01-011-0912-0077".into());
        let facts = vec![
            txt(
                FactKind::CadastralCode,
                1,
                Trust::Applicant,
                "01-011-0912-0146",
                "01-011-0912-0146",
            ),
            neighbour,
            txt(
                FactKind::CadastralCode,
                9,
                Trust::Applicant,
                "01-011-0912-0077",
                "01-011-0912-0077",
            ),
            txt(
                FactKind::CadastralCode,
                27,
                Trust::Applicant,
                "04-090-0047-0083",
                "04-090-0047-0083",
            ),
            txt(
                FactKind::CadastralCode,
                11,
                Trust::Applicant,
                "01-011-0912-0141",
                "01-011-0912-0141",
            ),
        ];
        let checks = run(&facts, Some("01-011-0912-0146"), &[], &[], &[]);
        let codes = by_kind(&checks, CheckKind::CadastralCode);
        let got: Vec<(usize, CheckStatus)> =
            codes.witnesses.iter().map(|w| (w.page, w.status)).collect();
        assert_eq!(
            got,
            [
                (1, CheckStatus::Agree),
                (9, CheckStatus::Agree),
                (27, CheckStatus::Disagree),
                (11, CheckStatus::NearMiss)
            ]
        );
    }

    #[test]
    fn the_fee_tables_partial_code_is_checked_by_its_tail() {
        let mut partial = txt(
            FactKind::CadastralCode,
            13,
            Trust::Official,
            "0731-0612",
            "0731-0612",
        );
        partial.subject = Some("partial".into());
        let facts = vec![
            txt(
                FactKind::CadastralCode,
                1,
                Trust::Applicant,
                "01-008-0857-0041",
                "01-008-0857-0041",
            ),
            partial,
        ];
        let checks = run(&facts, Some("01-008-0857-0041"), &[], &[], &[]);
        assert_eq!(
            by_kind(&checks, CheckKind::CadastralCode).status,
            CheckStatus::Disagree
        );
    }

    #[test]
    fn an_applicant_who_is_not_the_owner_is_a_disagreement() {
        let mut owner = txt(
            FactKind::Owner,
            2,
            Trust::Official,
            "ԱՐՄԱՆ ԶՈՀՐԱԲՅԱՆ",
            "person:arman-zohrabyan",
        );
        owner.about = Some("01-008-0731-0612".into());
        let facts = vec![
            owner,
            txt(
                FactKind::Applicant,
                12,
                Trust::Official,
                "ԱՐՄԱՆ ՀՈՎՀԱՆՆԻՍՅԱՆ",
                "person:arman-hovhannisyan",
            ),
            txt(
                FactKind::Applicant,
                16,
                Trust::Applicant,
                "Արման Զոհրաբյան",
                "person:arman-zohrabyan",
            ),
        ];
        let checks = run(&facts, Some("01-008-0731-0612"), &[], &[], &[]);
        let who = by_kind(&checks, CheckKind::Applicant);
        assert_eq!(who.status, CheckStatus::Disagree);
        assert_eq!(who.witnesses[1].status, CheckStatus::Agree);
    }

    #[test]
    fn planned_items_are_compared_among_the_applicants_pages() {
        let mut a = num(FactKind::PlannedArea, 1, Trust::Applicant, "2.0 քմ", 2.0);
        a.subject = Some("fence".into());
        let mut b = num(FactKind::PlannedArea, 8, Trust::Applicant, "6,40 քմ", 6.4);
        b.subject = Some("fence".into());
        let facts = vec![
            txt(
                FactKind::CadastralCode,
                1,
                Trust::Applicant,
                "x",
                "01-011-0912-0146",
            ),
            a,
            b,
        ];
        let checks = run(&facts, Some("01-011-0912-0146"), &[], &[], &[]);
        let fence = by_kind(&checks, CheckKind::PlannedArea);
        assert_eq!(fence.status, CheckStatus::Disagree);
        assert_eq!(fence.witnesses[0].page, 8);
    }

    #[test]
    fn a_registered_building_quoted_wrongly_is_caught_by_its_suffix() {
        let mut reg = num(
            FactKind::BuildingArea,
            3,
            Trust::Official,
            "17.24 քմ",
            17.24,
        );
        reg.subject = Some("01-008-0857-0041-001".into());
        reg.about = Some("01-008-0857-0041".into());
        let mut note = num(
            FactKind::BuildingArea,
            15,
            Trust::Applicant,
            "001 40.00 քմ",
            40.0,
        );
        note.subject = Some("-001".into());
        let facts = vec![reg, note];
        let checks = run(&facts, Some("01-008-0857-0041"), &[], &[], &[]);
        assert_eq!(
            by_kind(&checks, CheckKind::BuildingArea).status,
            CheckStatus::Disagree
        );
    }

    #[test]
    fn edit_distance_counts_one_wrong_digit() {
        assert_eq!(edit_distance("01-011-0912-0146", "01-011-0912-0141"), 1);
        assert_eq!(edit_distance("abc", "abc"), 0);
    }
}
