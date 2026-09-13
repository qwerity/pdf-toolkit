//! The vision-model answer file a `--answers` merge consumes.
//!
//! The shape is what the VLM stage (or a human reviewer) returns for the
//! pages the deterministic pass flagged: labels, a one-line title, entities,
//! group membership — the semantic layer a binary cannot read off a scan.
//! A page's answer is authoritative: where present, it replaces the
//! deterministic guess rather than being merged field-by-field, because the
//! answer was produced by something that actually looked at the page.
//!
//! Closed vocabularies (`relation`) deserialize into the report's enums, so
//! a typo in an answers file is a named parse error at load time instead of
//! a new edge type in the caller-facing graph.
//!
//! Maps use [`IndexMap`] because group order in the answer file is the order
//! groups appear in the final report, and a reviewer's ordering is worth
//! preserving.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::report::Relation;

/// Everything the vision pass answered for one document. Serializable too:
/// a live `--vlm` pass writes what it answered so a later `building
/// --answers` can reuse it instead of asking again.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Answers {
    /// Keyed by one-based page number.
    #[serde(default)]
    pub pages: HashMap<usize, PageAnswer>,
    /// Group metadata, keyed by the group ids the pages reference.
    #[serde(default)]
    pub groups: IndexMap<String, GroupMeta>,
    /// Display values and roles for person/org entities, keyed by entity id.
    #[serde(default)]
    pub people: IndexMap<String, PersonMeta>,
    #[serde(default)]
    pub georef_note: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

/// What the vision pass said about one page.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct PageAnswer {
    pub labels: Vec<String>,
    /// `digital`, `scan`, or `mixed`.
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub lang: Vec<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub sheet: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub anomaly: Option<String>,
    #[serde(default)]
    pub declared_scale: Option<f64>,
    #[serde(default)]
    pub entities: Vec<String>,
    #[serde(default)]
    pub groups: Vec<String>,
    #[serde(default)]
    pub related: Vec<RelatedAnswer>,
    #[serde(default)]
    pub handwriting: Option<HandwritingAnswer>,
    /// Reasons this page needed (or still needs) a vision pass.
    #[serde(default)]
    pub vlm: Vec<String>,
    #[serde(default)]
    pub encoding_issue: bool,
    /// Figures the model read off the page, for the cross-page checks —
    /// what a scanned certificate states about the parcel.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub facts: Vec<FactAnswer>,
}

/// One figure a vision answer states: `kind` is a `FactKind` name
/// (`parcel_area`, `building_area`, `owner`, `applicant`, `address`,
/// `cadastral_code`, `certificate_number`), `value` as printed, `unit` for
/// an area (`ha` or `m2`), `subject` a building code or planned item.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FactAnswer {
    pub kind: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RelatedAnswer {
    pub page: usize,
    pub relation: Relation,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct HandwritingAnswer {
    pub extent: String,
    #[serde(default)]
    pub script: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct GroupMeta {
    pub kind: String,
    pub title: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PersonMeta {
    pub value: String,
    #[serde(default)]
    pub roles: Vec<String>,
}

impl Answers {
    pub fn load(path: &Path) -> Result<Self> {
        let body = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read answers file '{}'", path.display()))?;
        serde_json::from_str(&body)
            .with_context(|| format!("answers file '{}' is not valid", path.display()))
    }

    pub fn page(&self, page_number: usize) -> Option<&PageAnswer> {
        self.pages.get(&page_number)
    }

    /// Folds `other` in without overriding anything already present. Used to
    /// combine a reviewed answers file with a live vision pass: the file was
    /// reviewed, so per page it wins.
    pub fn absorb(&mut self, other: Answers) {
        for (page, answer) in other.pages {
            self.pages.entry(page).or_insert(answer);
        }
        for (id, meta) in other.groups {
            self.groups.entry(id).or_insert(meta);
        }
        for (id, meta) in other.people {
            self.people.entry(id).or_insert(meta);
        }
        if self.georef_note.is_none() {
            self.georef_note = other.georef_note;
        }
        if self.note.is_none() {
            self.note = other.note;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_names_the_file_when_it_is_missing_or_invalid() {
        let missing = Answers::load(std::path::Path::new("/no/such/answers.json"));
        assert!(missing.is_err());
        assert!(format!("{:#}", missing.unwrap_err()).contains("/no/such/answers.json"));

        let dir = std::env::temp_dir().join("pdf-dossier-answers-test");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("broken.json");
        std::fs::write(&path, "not json at all").expect("write");
        let invalid = Answers::load(&path);
        assert!(invalid.is_err());
        assert!(format!("{:#}", invalid.unwrap_err()).contains("broken.json"));
    }

    /// A reviewer's typo in a closed vocabulary must fail at load, naming
    /// the field — never flow into the graph as a new edge type.
    #[test]
    fn an_unknown_relation_is_a_parse_error() {
        let bad: Result<Answers, _> = serde_json::from_str(
            r#"{"pages": {"1": {"labels": ["x"], "related": [{"page": 2, "relation": "resembles"}]}}}"#,
        );
        assert!(bad.is_err());

        let good: Answers = serde_json::from_str(
            r#"{"pages": {"1": {"labels": ["x"], "related": [{"page": 2, "relation": "duplicate_of"}]}}}"#,
        )
        .expect("parses");
        assert_eq!(
            good.page(1).expect("page").related[0].relation,
            Relation::DuplicateOf
        );
    }

    #[test]
    fn absorb_never_overrides_what_the_file_already_answered() {
        let mut reviewed: Answers = serde_json::from_str(
            r#"{"pages": {"1": {"labels": ["passport"]}},
                "people": {"person:a": {"value": "reviewed"}}}"#,
        )
        .expect("parses");
        let live: Answers = serde_json::from_str(
            r#"{"pages": {"1": {"labels": ["photo"]}, "2": {"labels": ["consent_statement"]}},
                "people": {"person:a": {"value": "live"}, "person:b": {"value": "new"}}}"#,
        )
        .expect("parses");

        reviewed.absorb(live);
        assert_eq!(reviewed.page(1).expect("page 1").labels, ["passport"]);
        assert_eq!(
            reviewed.page(2).expect("page 2").labels,
            ["consent_statement"]
        );
        assert_eq!(reviewed.people["person:a"].value, "reviewed");
        assert_eq!(reviewed.people["person:b"].value, "new");
    }
}
