//! The credentials a Committee document prints on itself.
//!
//! An ownership certificate (ՎԿԱՅԱԿԱՆ) and a reference letter (ՏԵՂԵԿԱՆՔ)
//! each print, in their header, a document number and a passcode
//! («գաղտնաբառ»). Together they are what the Cadastre Committee asks for
//! before it will show its own copy, so a reviewer holding both can check
//! the filed document against the register's.
//!
//! This module reads them off the page and stops there. **It does not build
//! links and does not fetch anything** — what to do with the pair is the
//! caller's business, and the toolkit stays a reader of PDFs.

use regex::Regex;
use serde::Serialize;
use std::sync::LazyLock;

/// Which Committee document the credentials belong to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessKind {
    Certificate,
    Reference,
}

/// A document's number and passcode, as printed, and the page they are on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DocumentAccess {
    pub kind: AccessKind,
    /// As printed — a reference keeps its `ԱՏ-` prefix, a certificate has none.
    pub number: String,
    pub passcode: String,
    pub page: usize,
}

/// The passcode as printed: Latin capitals and digits, so it survives the
/// legacy Armenian decoding untouched. Stated once — both headers end in it.
const PASSCODE: &str = r"գաղտնաբառ[՝:]?\s*([A-Z0-9]{8,12})\b";

static CERTIFICATE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"ՎԿԱՅԱԿԱՆ N\s*(\d{{8}}-\d{{2}}-\d{{4}}),?\s*{PASSCODE}"
    ))
    .expect("static regex")
});
static REFERENCE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?:Փաստաթղթի (?:համար|ծածկագիր)՝|N)\s*([Ա-Ֆ]{{0,3}}-?\d{{8}}-[\d-]*\d)\s*(?:Փաստաթղթի\s+)?{PASSCODE}"
    ))
    .expect("static regex")
});

/// Records one document once: a certificate repeats its header on the
/// continuation page, and the same number twice is the same document, not two.
/// The single authority on that invariant — both readers below go through it.
fn push_access(
    out: &mut Vec<DocumentAccess>,
    page: usize,
    kind: AccessKind,
    number: &str,
    passcode: &str,
) {
    if number.is_empty() || passcode.is_empty() || out.iter().any(|a| a.number == number) {
        return;
    }
    out.push(DocumentAccess {
        kind,
        number: number.to_string(),
        passcode: passcode.to_string(),
        page,
    });
}

/// The credentials a page prints, certificate first, each number once.
///
/// The label is tested before the regex, so a page that is neither kind of
/// Committee document costs nothing.
pub fn page_access(page: usize, labels: &[String], text: &str) -> Vec<DocumentAccess> {
    let mut out: Vec<DocumentAccess> = Vec::new();
    for (label, kind, pattern) in [
        (
            "cadastral_certificate",
            AccessKind::Certificate,
            &*CERTIFICATE,
        ),
        ("cadastral_reference", AccessKind::Reference, &*REFERENCE),
    ] {
        if labels.iter().any(|l| l == label) {
            for c in pattern.captures_iter(text) {
                push_access(&mut out, page, kind, &c[1], &c[2]);
            }
        }
    }
    out
}

/// The credentials a vision answer read off a scanned page. An answer whose
/// kind is neither `certificate` nor `reference`, or whose fields are blank,
/// is dropped — the model is told to omit what it cannot read, not to guess.
pub fn answered_access(
    page: usize,
    answers: &[crate::answers::AccessAnswer],
) -> Vec<DocumentAccess> {
    let mut out: Vec<DocumentAccess> = Vec::new();
    for a in answers {
        let kind = match a.kind.as_str() {
            "certificate" => AccessKind::Certificate,
            "reference" => AccessKind::Reference,
            _ => continue,
        };
        push_access(&mut out, page, kind, a.number.trim(), a.passcode.trim());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_certificate_header_gives_its_number_and_passcode() {
        let text = "ՎԿԱՅԱԿԱՆ N 20300101-01-0001, գաղտնաբառ՝ 0AAAA0AAA0AA Փաստաթղթի իսկությունը\n";
        let found = page_access(5, &labels(&["cadastral_certificate"]), text);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, AccessKind::Certificate);
        assert_eq!(found[0].number, "20300101-01-0001");
        assert_eq!(found[0].passcode, "0AAAA0AAA0AA");
        assert_eq!(found[0].page, 5);
    }

    #[test]
    fn a_reference_letter_is_read_as_a_reference() {
        let text =
            "Փաստաթղթի համար՝ ԱՏ-20300101-01-0002 Փաստաթղթի գաղտնաբառ՝ 0BBBB0BBB0BB Դիմող` ...";
        let found = page_access(2, &labels(&["cadastral_reference", "qr"]), text);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, AccessKind::Reference);
        assert_eq!(found[0].number, "ԱՏ-20300101-01-0002");
        assert_eq!(found[0].passcode, "0BBBB0BBB0BB");
    }

    #[test]
    fn the_older_reference_layouts_are_read_too() {
        let coded = "Փաստաթղթի ծածկագիր՝ 20300101-0003 Փաստաթղթի գաղտնաբառ՝ 0CCCC0CC Տեղեկանք";
        let found = page_access(1, &labels(&["cadastral_reference"]), coded);
        assert_eq!(found[0].number, "20300101-0003");
        assert_eq!(found[0].passcode, "0CCCC0CC");
        let bare = "ՎԵՐԱԲԵՐՅԱԼ 01/01/2030թ. N ԱՏ-20300101-01-0004 գաղտնաբառ՝ 0DDDD0DDD0DD Դիմող`";
        let found = page_access(1, &labels(&["cadastral_reference"]), bare);
        assert_eq!(found[0].number, "ԱՏ-20300101-01-0004");
        assert_eq!(found[0].passcode, "0DDDD0DDD0DD");
    }

    #[test]
    fn an_answered_scan_gives_the_same_record_and_a_guess_is_dropped() {
        use crate::answers::AccessAnswer;
        let answers = [
            AccessAnswer {
                kind: "certificate".into(),
                number: "20300101-01-0001".into(),
                passcode: " 0AAAA0AAA0AA ".into(),
            },
            AccessAnswer {
                kind: "certificate".into(),
                number: "20300101-01-0009".into(),
                passcode: String::new(),
            },
            AccessAnswer {
                kind: "passport".into(),
                number: "AB1234567".into(),
                passcode: "X".into(),
            },
        ];
        let found = answered_access(7, &answers);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].passcode, "0AAAA0AAA0AA");
        assert_eq!(found[0].page, 7);
    }

    #[test]
    fn a_page_without_the_label_or_the_passcode_gives_nothing() {
        let text = "ՎԿԱՅԱԿԱՆ N 20300101-01-0001, գաղտնաբառ՝ 0AAAA0AAA0AA";
        assert!(page_access(1, &labels(&["text_document"]), text).is_empty());
        let no_passcode = "ՎԿԱՅԱԿԱՆ N 20300101-01-0001 Փաստաթղթի իսկությունը";
        assert!(page_access(1, &labels(&["cadastral_certificate"]), no_passcode).is_empty());
    }

    #[test]
    fn the_same_number_on_a_continuation_page_is_kept_once() {
        let text = "ՎԿԱՅԱԿԱՆ N 20300101-01-0001, գաղտնաբառ՝ 0AAAA0AAA0AA Էջ 1/2 ... ՎԿԱՅԱԿԱՆ N 20300101-01-0001, գաղտնաբառ՝ 0AAAA0AAA0AA";
        assert_eq!(
            page_access(1, &labels(&["cadastral_certificate"]), text).len(),
            1
        );
    }
}
