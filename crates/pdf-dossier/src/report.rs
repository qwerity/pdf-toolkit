//! What the `dossier` command reports: the whole-dossier annotation.
//!
//! The shape is the annotation schema (`docs/annotation-schema.md`): one
//! entry per page carrying labels, extraction routing, QR payloads, entities
//! and group membership, plus document-level groups, a deduplicated entity
//! registry, and a graph derived from those inline facts.
//!
//! The closed vocabularies — route, relation, edge type, georeference status
//! — are Rust enums, not strings, so a typo is a compile error rather than a
//! new value in the caller-facing JSON. `labels` and `vlm.reasons` stay open
//! by design and stay strings.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use pdf_classify::{Confidence, PageClass};

pub const SCHEMA_VERSION: &str = "0.2.0";

/// How a page's text is recovered.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Route {
    /// pdfium's text layer is trustworthy.
    EmbeddedText,
    /// Only a vision model can read the page.
    Vlm,
    /// Nothing to read — pure imagery.
    #[default]
    None,
}

/// How an entity mention was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Via {
    EmbeddedText,
    Vlm,
    None,
    /// Reserved for payload-derived entities; not currently produced.
    Qr,
}

impl From<Route> for Via {
    fn from(route: Route) -> Self {
        match route {
            Route::EmbeddedText => Via::EmbeddedText,
            Route::Vlm => Via::Vlm,
            Route::None => Via::None,
        }
    }
}

/// A non-group relation between two pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Relation {
    /// The same physical document flowing onto the next page.
    Continues,
    /// The same sheet appearing again elsewhere in the dossier.
    DuplicateOf,
}

/// What an edge in the derived graph asserts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeType {
    Mentions,
    MemberOf,
    About,
    Continues,
    DuplicateOf,
    GeoreferenceSource,
}

impl From<Relation> for EdgeType {
    fn from(relation: Relation) -> Self {
        match relation {
            Relation::Continues => EdgeType::Continues,
            Relation::DuplicateOf => EdgeType::DuplicateOf,
        }
    }
}

/// Whether a placed fit applies to a drawings group's own frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GeorefStatus {
    /// A member page placed: the fit is in this group's drawing frame.
    Applied,
    /// The dossier placed elsewhere; this group shares the site, not the
    /// frame. Geometry decides, never proximity.
    SameSiteUnverified,
    None,
}

/// The whole-dossier annotation for one document.
#[derive(Debug, Serialize)]
pub struct DossierReport {
    pub schema_version: &'static str,
    /// Stem of the source file.
    pub document: String,
    /// Path as given on the command line.
    pub source: String,
    pub page_count: usize,
    /// What produced this file — names the passes so a reader can tell a
    /// deterministic-only run from one merged with vision-model answers.
    pub generated_by: String,
    pub stats: Stats,
    /// What the application form asks for — action, object, use, works —
    /// read off the form's fixed slots. Absent when no embedded-text
    /// application form states a request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request: Option<crate::request::Request>,
    pub pages: Vec<PageAnnotation>,
    pub groups: Vec<Group>,
    pub entities: Vec<Entity>,
    pub graph: Graph,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub georeference_note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Pages whose geometry extraction failed outright, verbatim from the
    /// vector pass. A page listed here has no classification, no `cad`, no
    /// georeference — and this is the record that says why, so a check
    /// that could not run does not look like a check that passed.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub warnings: Vec<String>,
    /// With `--vlm` and `--output`: the merged vision-pass answers, written as
    /// `answers/<document>.json` relative to the output directory — the file
    /// `building --answers` takes. Absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answers_file: Option<String>,
}

/// Per-dossier counts, so a caller can see the shape of the work at a glance.
#[derive(Debug, Default, Serialize)]
pub struct Stats {
    pub embedded_text: usize,
    pub vlm: usize,
    pub none: usize,
    /// Pages carrying a `vlm` block — the VLM-routed ones plus embedded-text
    /// pages whose layer is mis-encoded.
    pub vlm_flagged: usize,
    pub qr_pages: usize,
    pub handwriting_pages: usize,
    pub placed_pages: usize,
}

/// One page of the dossier.
#[derive(Debug, Serialize)]
pub struct PageAnnotation {
    pub page_number: usize,
    /// Multi-label; most specific first. Open vocabulary.
    pub labels: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Album sheet designation printed on the page (ԷՋ-03, Ճ-1, Է.02 …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sheet: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anomaly: Option<String>,
    /// The `vector` heuristic's verdict, kept verbatim — right or wrong — so
    /// the annotation doubles as an error ledger for the classifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub classification: Option<Classification>,
    /// `digital`, `scan` or `mixed`.
    pub content_kind: String,
    /// ISO 639-1 codes of the languages visible on the page.
    pub languages: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declared_scale_denominator: Option<f64>,
    pub extraction: Extraction,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub qr_codes: Vec<QrCode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handwriting: Option<Handwriting>,
    /// Ids into the top-level entity registry.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<String>,
    /// Ids into the top-level groups.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub related_pages: Vec<RelatedPage>,
    /// Verbatim from `vector`, present only when the page placed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub georeference: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cad: Option<Cad>,
}

/// `class` + `confidence` from `pdf-classify`, without the feature block.
/// The enums serialize themselves; this crate adds no second vocabulary.
#[derive(Debug, Serialize)]
pub struct Classification {
    pub class: PageClass,
    pub confidence: Confidence,
    pub is_drawing: bool,
    pub is_mappable: bool,
}

/// How the page's text was — or must be — recovered.
#[derive(Debug, Default, Serialize)]
pub struct Extraction {
    /// The `inspect` routing slugs observed on the page.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub routing_signals: Vec<String>,
    pub route: Route,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chars: Option<usize>,
    /// A text layer exists but is wrong: legacy 8-bit font encoding, CID
    /// glyph outlines, or a custom glyph mapping.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub encoding_issue: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vlm: Option<VlmBlock>,
}

/// The page's vision-model work order, and — after a merge — its result.
#[derive(Debug, Serialize)]
pub struct VlmBlock {
    /// Invariantly `true`: the block's presence is the fact. Kept in the
    /// serialized shape for callers that already branch on it.
    pub required: bool,
    /// Open list; treat unknown values as "needs a vision pass".
    pub reasons: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extracted: Option<VlmExtracted>,
}

/// What the vision pass recovered from the page.
#[derive(Debug, Serialize)]
pub struct VlmExtracted {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// One decoded (or visually confirmed but undecodable) QR code.
#[derive(Debug, Serialize)]
pub struct QrCode {
    /// The payload, or null for a code that is visible but would not decode.
    pub data: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Handwriting on the page, when there is any.
#[derive(Debug, Clone, Serialize)]
pub struct Handwriting {
    pub present: bool,
    /// `signatures`, `fields`, `notes`, or `full_page`.
    pub extent: String,
    pub script: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RelatedPage {
    pub page: usize,
    pub relation: Relation,
}

/// A reference to the page's CAD geometry export.
#[derive(Debug, Serialize)]
pub struct Cad {
    pub crs: String,
    pub features: usize,
    /// Relative path of the GeoJSON file, present when `--output` wrote one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub geojson: Option<String>,
}

/// Pages that belong together as one document section.
#[derive(Debug, Serialize)]
pub struct Group {
    pub id: String,
    /// `identity`, `ownership`, `application`, `administrative`,
    /// `financial`, `drawings`, or `technical`.
    pub kind: String,
    pub title: String,
    pub pages: Vec<usize>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<String>,
    /// Present on `drawings` groups.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub georeference: Option<GroupGeoreference>,
}

#[derive(Debug, Serialize)]
pub struct GroupGeoreference {
    pub status: GeorefStatus,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub source_pages: Vec<usize>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub applies_to_pages: Vec<usize>,
}

/// A real-world identifier seen somewhere in the dossier.
#[derive(Debug, Serialize)]
pub struct Entity {
    /// `<type>:<value>`, e.g. `cadastral_code:10-001-0123-0789`.
    pub id: String,
    #[serde(rename = "type")]
    pub entity_type: String,
    pub value: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<String>,
    pub sources: Vec<EntitySource>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EntitySource {
    pub page: usize,
    pub via: Via,
}

/// Derived mechanically from the inline facts; repeats no independent
/// information.
#[derive(Debug, Serialize)]
pub struct Graph {
    pub edges: Vec<Edge>,
}

#[derive(Debug, Serialize)]
pub struct Edge {
    pub from: String,
    pub to: String,
    #[serde(rename = "type")]
    pub edge_type: EdgeType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via: Option<Via>,
}

impl DossierReport {
    /// Internal-consistency problems a reviewer must not have to hunt for:
    /// dangling group/entity references, out-of-range related pages, pages
    /// with no labels.
    pub fn validate(&self) -> Vec<String> {
        let mut problems = Vec::new();
        let group_ids: std::collections::HashSet<&str> =
            self.groups.iter().map(|g| g.id.as_str()).collect();
        let entity_ids: std::collections::HashSet<&str> =
            self.entities.iter().map(|e| e.id.as_str()).collect();

        if self.pages.len() != self.page_count {
            problems.push(format!(
                "pages {} != page_count {}",
                self.pages.len(),
                self.page_count
            ));
        }
        for page in &self.pages {
            let n = page.page_number;
            for group in &page.groups {
                if !group_ids.contains(group.as_str()) {
                    problems.push(format!("p{n}: unknown group {group}"));
                }
            }
            for entity in &page.entities {
                if !entity_ids.contains(entity.as_str()) {
                    problems.push(format!("p{n}: unknown entity {entity}"));
                }
            }
            for related in &page.related_pages {
                if related.page < 1 || related.page > self.page_count {
                    problems.push(format!("p{n}: related page {} out of range", related.page));
                }
            }
            if page.labels.is_empty() {
                problems.push(format!("p{n}: no labels"));
            }
        }
        problems
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The enum spellings are the contract; a rename on the Rust side must
    /// not silently rename a JSON value callers branch on.
    #[test]
    fn closed_vocabularies_serialize_to_their_contract_strings() {
        assert_eq!(
            serde_json::to_value(Route::EmbeddedText).unwrap(),
            "embedded_text"
        );
        assert_eq!(serde_json::to_value(Route::Vlm).unwrap(), "vlm");
        assert_eq!(serde_json::to_value(Route::None).unwrap(), "none");
        assert_eq!(serde_json::to_value(Via::from(Route::Vlm)).unwrap(), "vlm");
        assert_eq!(
            serde_json::to_value(Relation::DuplicateOf).unwrap(),
            "duplicate_of"
        );
        assert_eq!(
            serde_json::to_value(EdgeType::from(Relation::Continues)).unwrap(),
            "continues"
        );
        assert_eq!(
            serde_json::to_value(EdgeType::GeoreferenceSource).unwrap(),
            "georeference_source"
        );
        assert_eq!(
            serde_json::to_value(GeorefStatus::SameSiteUnverified).unwrap(),
            "same_site_unverified"
        );
    }

    fn minimal_page(number: usize) -> PageAnnotation {
        PageAnnotation {
            page_number: number,
            labels: vec!["text_document".into()],
            title: None,
            sheet: None,
            notes: None,
            anomaly: None,
            classification: None,
            content_kind: "digital".into(),
            languages: Vec::new(),
            declared_scale_denominator: None,
            extraction: Extraction::default(),
            qr_codes: Vec::new(),
            handwriting: None,
            entities: Vec::new(),
            groups: Vec::new(),
            related_pages: Vec::new(),
            georeference: None,
            cad: None,
        }
    }

    fn minimal_report(pages: Vec<PageAnnotation>) -> DossierReport {
        DossierReport {
            schema_version: SCHEMA_VERSION,
            document: "t".into(),
            source: "t.pdf".into(),
            page_count: pages.len(),
            generated_by: String::new(),
            stats: Stats::default(),
            request: None,
            pages,
            groups: Vec::new(),
            entities: Vec::new(),
            graph: Graph { edges: Vec::new() },
            georeference_note: None,
            note: None,
            warnings: Vec::new(),
            answers_file: None,
        }
    }

    #[test]
    fn a_consistent_report_validates_clean() {
        assert!(minimal_report(vec![minimal_page(1)]).validate().is_empty());
    }

    /// Each inconsistency a reviewer must not have to hunt for is named:
    /// dangling references, out-of-range relations, unlabelled pages, and a
    /// page list that disagrees with the count.
    #[test]
    fn every_inconsistency_is_reported_by_name() {
        let mut page = minimal_page(1);
        page.groups = vec!["nowhere".into()];
        page.entities = vec!["person:nobody".into()];
        page.related_pages = vec![RelatedPage {
            page: 99,
            relation: Relation::Continues,
        }];
        page.labels = Vec::new();
        let mut report = minimal_report(vec![page]);
        report.page_count = 2;

        let problems = report.validate().join("\n");
        for expected in [
            "unknown group nowhere",
            "unknown entity person:nobody",
            "related page 99 out of range",
            "no labels",
            "pages 1 != page_count 2",
        ] {
            assert!(
                problems.contains(expected),
                "missing '{expected}' in: {problems}"
            );
        }
    }
}
