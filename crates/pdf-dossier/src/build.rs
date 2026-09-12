//! Assembling the dossier annotation for one document.
//!
//! The build is split at its natural seam: [`analyze`] does everything that
//! reads the PDF — text extraction, routing signals, QR renders, the whole
//! `pdf-vector` pass — and [`assemble`] turns one analysis plus options into
//! a report. The split exists for the vision-pass merge: answering flagged
//! pages changes no fact about the document, so the merge re-assembles over
//! the same analysis instead of re-reading and re-rendering everything.
//!
//! Two sources feed every page: the deterministic passes, and — when the
//! caller supplies them — vision-pass [`Answers`]. An answered page takes
//! the answer wholesale rather than merging field-by-field: the answer came
//! from something that looked at the page, and the deterministic guess is
//! exactly the thing it was asked to replace.
//!
//! Groups, the entity registry and the graph are derived at the end from the
//! per-page facts, never authored, so they cannot disagree with the pages.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::num::NonZeroU16;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use indexmap::IndexMap;
use pdfium_render::prelude::Pdfium;

use pdf_classify::PageClass;
use pdf_core::page::{open_document, page_count_u16, page_index};
use pdf_core::text_quality::is_probably_unreadable_text;
use pdf_inspect::LlmReason;
use pdf_vector::{DocumentVector, PageVector, VectorOptions};

use crate::answers::{Answers, PageAnswer};
use crate::entities::{self, Found, Template};
use crate::report::*;
use crate::{qr, text_signals};

/// Whether a label marks a page whose geometry export is worth referencing:
/// the classifier's own drawing classes, plus the labels this schema adds on
/// top of them.
fn is_drawing_label(label: &str) -> bool {
    PageClass::parse(label).is_some_and(PageClass::is_drawing)
        || matches!(label, "cad_drawing" | "roof_plan")
}

/// Labels that are pure imagery: nothing on the page reads as text at all.
const PURE_IMAGE_LABELS: &[&str] = &["photo_montage", "photo"];

/// Below this many characters an embedded text layer is presence, not
/// content — the same threshold the ground-truth corpus was built with.
const MIN_EMBEDDED_CHARS: usize = 50;

#[derive(Debug, Default)]
pub struct DossierOptions {
    /// Vision-pass answers to merge. Without them, VLM-routed pages carry
    /// their work order and the semantic layer stays pending.
    pub answers: Option<Answers>,
    /// Directory to write per-page GeoJSON under (`geojson/<doc>/…`),
    /// mirroring the layout the annotation's `cad.geojson` paths use.
    pub output_dir: Option<PathBuf>,
}

/// Everything a dossier build reads from the PDF, collected once.
pub struct DossierAnalysis {
    /// Path as given on the command line, echoed as the report's `source`.
    source: String,
    vector: DocumentVector,
    inputs: Vec<PageInputs>,
}

/// Everything the deterministic passes know about one page.
struct PageInputs {
    number: usize,
    text: String,
    /// `text.chars().count()`, computed once at collection.
    chars: usize,
    reasons: Vec<LlmReason>,
    image_coverage: f32,
    qr_payloads: Vec<String>,
}

impl PageInputs {
    fn embedded(&self) -> bool {
        self.chars >= MIN_EMBEDDED_CHARS
    }

    /// Whether `inspect` says the page's content lives in pixels.
    fn scan_signal(&self) -> bool {
        self.reasons.iter().any(|r| {
            matches!(
                r,
                LlmReason::NoTextLayer | LlmReason::ImageHeavy | LlmReason::UnreadableText
            )
        })
    }
}

/// The resolved facts a page contributes to the report and the derivations.
#[derive(Default)]
struct PageFacts {
    labels: Vec<String>,
    title: Option<String>,
    sheet: Option<String>,
    notes: Option<String>,
    anomaly: Option<String>,
    content_kind: String,
    languages: Vec<String>,
    declared_scale: Option<f64>,
    route: Route,
    vlm_reasons: Vec<String>,
    encoding_issue: bool,
    handwriting: Option<Handwriting>,
    /// (id, printed value) pairs, in mention order.
    entities: Vec<(String, String)>,
    groups: Vec<String>,
    related: Vec<RelatedPage>,
}

/// Builds the whole-dossier annotation in one call.
pub fn build_dossier(
    pdfium: &Pdfium,
    path: &Path,
    options: &DossierOptions,
) -> Result<DossierReport> {
    assemble(&analyze(pdfium, path)?, options)
}

/// The expensive half: reads text, routing signals, QR payloads and the
/// vector geometry for every page.
pub fn analyze(pdfium: &Pdfium, path: &Path) -> Result<DossierAnalysis> {
    let vector =
        pdf_vector::extract_document(path, &[], &VectorOptions::default()).with_context(|| {
            format!(
                "failed to extract vector geometry from '{}'",
                path.display()
            )
        })?;
    let inputs = collect_page_inputs(pdfium, path)
        .with_context(|| format!("failed to analyze '{}'", path.display()))?;
    Ok(DossierAnalysis {
        source: path.display().to_string(),
        vector,
        inputs,
    })
}

/// The cheap half: resolves per-page facts (deterministic or answered),
/// derives groups, the entity registry and the graph, and writes GeoJSON
/// when an output directory is given.
pub fn assemble(analysis: &DossierAnalysis, options: &DossierOptions) -> Result<DossierReport> {
    let answers = options.answers.as_ref();
    let vector_pages: HashMap<usize, &PageVector> = analysis
        .vector
        .pages
        .iter()
        .map(|p| (p.page_number, p))
        .collect();

    let mut facts: Vec<PageFacts> = analysis
        .inputs
        .iter()
        .map(|input| {
            let vector_page = vector_pages.get(&input.number).copied();
            match answers.and_then(|a| a.page(input.number)) {
                Some(answer) => answered_facts(input, answer),
                None => deterministic_facts(input, vector_page),
            }
        })
        .collect();

    let certificate_numbers: Vec<Option<String>> = facts.iter().map(certificate_number).collect();
    derive_groups(&mut facts, &analysis.inputs, answers, &certificate_numbers);

    let subject = subject_parcel(&facts);
    let group_meta = group_metadata(&facts, &certificate_numbers, answers, subject.as_deref());

    emit_report(analysis, facts, group_meta, answers, options, &vector_pages)
}

/// The pages a vision pass should be asked about: flagged by the report, and
/// not already covered by the given answers.
pub fn pages_awaiting_vision(report: &DossierReport, answers: Option<&Answers>) -> Vec<usize> {
    report
        .pages
        .iter()
        .filter(|page| page.extraction.vlm.is_some())
        .filter(|page| answers.is_none_or(|a| a.page(page.page_number).is_none()))
        .map(|page| page.page_number)
        .collect()
}

/// Carries phase-1 flag reasons into live answers that arrived without any,
/// so the merged annotation still says why each page went to a model.
pub fn seed_vision_reasons(report: &DossierReport, live: &mut Answers) {
    for page in &report.pages {
        if let Some(vlm) = &page.extraction.vlm
            && let Some(answer) = live.pages.get_mut(&page.page_number)
            && answer.vlm.is_empty()
        {
            answer.vlm = vlm.reasons.clone();
        }
    }
}

/// Reads text, routing signals and QR payloads for every page, in one pass
/// over one open document — the text is extracted once and feeds both the
/// routing signals and the report.
fn collect_page_inputs(pdfium: &Pdfium, path: &Path) -> Result<Vec<PageInputs>> {
    let document = open_document(pdfium, path).map_err(anyhow::Error::from)?;
    let pages = document.pages();
    let page_count = page_count_u16(pages).map_err(anyhow::Error::from)?;
    let mut out = Vec::with_capacity(usize::from(page_count));

    for page_number in (1..=page_count).filter_map(NonZeroU16::new) {
        let number = usize::from(page_number.get());
        let page = pages
            .get(page_index(page_number))
            .map_err(|error| anyhow::anyhow!("cannot load page {number}: {error}"))?;

        let (info, sanitized) = pdf_inspect::page_info_and_text(page_number.get(), &page);
        // Line endings normalized and both ends trimmed, so `chars` and
        // template offsets do not depend on the CRLF or leading whitespace
        // pdfium emits.
        let normalized = sanitized
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .trim()
            .to_string();
        // A garbled layer is worse than none: it reads as content but is not.
        // The `unreadable_text` signal carries the fact; the text is dropped.
        let text = if is_probably_unreadable_text(&normalized) {
            String::new()
        } else {
            normalized
        };

        let mut input = PageInputs {
            number,
            chars: text.chars().count(),
            text,
            reasons: info.llm_reasons,
            image_coverage: info.image_coverage,
            qr_payloads: Vec::new(),
        };

        input.qr_payloads = qr::decode_at(&page, qr::BASE_WIDTH_PX)
            .map_err(anyhow::Error::from)
            .with_context(|| format!("QR pass failed on page {number}"))?;
        // The expensive high-resolution retry pays off only where a code can
        // hide: scans can print one too small for the base render, and
        // e-cadastre documents always carry one. CAD sheets never do.
        if input.qr_payloads.is_empty()
            && (input.scan_signal() || input.text.contains("e-cadastre.am"))
        {
            input.qr_payloads =
                qr::decode_at(&page, qr::RETRY_WIDTH_PX).map_err(anyhow::Error::from)?;
        }

        out.push(input);
    }
    Ok(out)
}

/// Whether the page's text layer is present but wrong — a legacy 8-bit font
/// encoding, a CID glyph map, or whatever the answer's reviewer saw.
fn has_encoding_issue(input: &PageInputs, answer: Option<&PageAnswer>) -> bool {
    answer.is_some_and(|a| a.encoding_issue)
        || text_signals::is_legacy_encoded(&input.text)
        || input.reasons.contains(&LlmReason::UnreadableText)
}

/// The one routing rule, for both fact sources. The answer contributes
/// evidence (its reasons, handwriting, labels, kind); it does not get a
/// parallel ladder of its own.
fn resolve_route(
    input: &PageInputs,
    answer: Option<&PageAnswer>,
    encoding_issue: bool,
    vlm_reasons: &mut Vec<String>,
) -> Route {
    if input.embedded() {
        // A mis-encoded layer still needs a vision model to actually read
        // the page, even though the raw text is worth keeping.
        if encoding_issue && !vlm_reasons.iter().any(|r| r == "legacy_encoding") {
            vlm_reasons.push("legacy_encoding".to_string());
        }
        return Route::EmbeddedText;
    }
    // An answer whose labels say pure imagery settles the route before any
    // flag reasons do: a live pass seeds the phase-1 reasons into its
    // answers, and the same answer replayed from a file must not produce a
    // different route. Handwriting still wins — a photo with handwritten
    // notes has something to read.
    let pure_imagery = answer.is_some_and(|a| {
        !a.labels.is_empty()
            && a.labels
                .iter()
                .all(|l| PURE_IMAGE_LABELS.contains(&l.as_str()))
    });
    if pure_imagery && !answer.is_some_and(|a| a.handwriting.is_some()) {
        vlm_reasons.clear();
        return Route::None;
    }
    if !vlm_reasons.is_empty() || answer.is_some_and(|a| a.handwriting.is_some()) {
        return Route::Vlm;
    }
    if input.scan_signal() || answer.is_some_and(|a| a.kind.as_deref() == Some("scan")) {
        vlm_reasons.push("scanned_or_unreadable_text".to_string());
        return Route::Vlm;
    }
    Route::None
}

/// Facts for a page the vision pass answered: the answer is authoritative.
fn answered_facts(input: &PageInputs, answer: &PageAnswer) -> PageFacts {
    let encoding_issue = has_encoding_issue(input, Some(answer));
    let mut vlm_reasons = answer.vlm.clone();
    let route = resolve_route(input, Some(answer), encoding_issue, &mut vlm_reasons);

    PageFacts {
        labels: answer.labels.clone(),
        title: answer.title.clone(),
        sheet: answer.sheet.clone(),
        notes: answer.notes.clone(),
        anomaly: answer.anomaly.clone(),
        content_kind: answer.kind.clone().unwrap_or_else(|| "digital".to_string()),
        languages: answer.lang.clone(),
        declared_scale: answer.declared_scale,
        route,
        vlm_reasons,
        encoding_issue,
        handwriting: answer.handwriting.as_ref().map(|h| Handwriting {
            present: true,
            extent: h.extent.clone(),
            script: h.script.clone(),
        }),
        entities: answer
            .entities
            .iter()
            .map(|id| (id.clone(), fallback_value(id)))
            .collect(),
        groups: answer.groups.clone(),
        related: answer
            .related
            .iter()
            .map(|r| RelatedPage {
                page: r.page,
                relation: r.relation,
            })
            .collect(),
    }
}

/// Facts for a page only the deterministic passes have seen.
fn deterministic_facts(input: &PageInputs, vector_page: Option<&PageVector>) -> PageFacts {
    let embedded = input.embedded();
    let template = embedded
        .then(|| entities::detect_template(&input.text))
        .flatten();
    let encoding_issue = has_encoding_issue(input, None);
    let mut vlm_reasons = Vec::new();
    let route = resolve_route(input, None, encoding_issue, &mut vlm_reasons);

    let mut found: Vec<Found> = Vec::new();
    if embedded {
        found = entities::extract(&input.text);
        if matches!(
            template,
            Some(Template::Certificate {
                continuation: false
            })
        ) {
            found.extend(entities::certificate_owners(&input.text));
        }
    }

    PageFacts {
        labels: deterministic_labels(template, vector_page, &input.qr_payloads, route),
        sheet: embedded
            .then(|| entities::sheet_designation(&input.text))
            .flatten(),
        content_kind: deterministic_content_kind(embedded, input.image_coverage),
        languages: text_signals::languages(&input.text),
        declared_scale: vector_page
            .and_then(|v| v.classification.features.declared_scales.first().copied()),
        route,
        vlm_reasons,
        encoding_issue,
        entities: found.into_iter().map(|f| (f.id, f.value)).collect(),
        ..Default::default()
    }
}

fn deterministic_labels(
    template: Option<Template>,
    vector_page: Option<&PageVector>,
    qr_payloads: &[String],
    route: Route,
) -> Vec<String> {
    let mut labels: Vec<String> = match template {
        Some(Template::Certificate { continuation }) => {
            let mut l = vec!["cadastral_certificate".into(), "text_document".into()];
            if continuation {
                l.push("table".into());
            }
            l
        }
        Some(Template::Reference) => vec!["cadastral_reference".into(), "text_document".into()],
        Some(Template::ApplicationForm) => vec!["application_form".into(), "text_document".into()],
        None => match vector_page {
            Some(v) if v.classification.is_drawing => {
                let class = v.classification.class;
                if class == PageClass::ScannedDrawing {
                    vec![class.as_str().to_string()]
                } else {
                    vec!["cad_drawing".into(), class.as_str().to_string()]
                }
            }
            _ if route == Route::EmbeddedText => vec!["text_document".into()],
            _ => vec!["unknown".into()],
        },
    };
    if !qr_payloads.is_empty() && !labels.iter().any(|l| l == "qr") {
        // Right after the primary label: the code is part of what the page is.
        labels.insert(labels.len().min(1), "qr".into());
    }
    labels
}

fn deterministic_content_kind(embedded: bool, image_coverage: f32) -> String {
    if embedded && image_coverage < 0.3 {
        "digital"
    } else if embedded {
        "mixed"
    } else if image_coverage > 0.5 {
        "scan"
    } else {
        "digital"
    }
    .to_string()
}

/// Deterministic grouping over the whole document, written into the facts.
///
/// Grouping is where single pages become a dossier: an application, one group
/// per certificate (its continuation pages chained with `continues`), the
/// cadastre references, and the drawings. An answer that names groups is
/// authoritative; one that names none — every live vision answer, since
/// the pass never asks for groups — gets the same derivation an unanswered
/// page gets. Chaining likewise applies wherever the source page's answer
/// did not supply relations itself.
fn derive_groups(
    facts: &mut [PageFacts],
    inputs: &[PageInputs],
    answers: Option<&Answers>,
    certificate_numbers: &[Option<String>],
) {
    for (index, fact) in facts.iter_mut().enumerate() {
        let answered_groups = answers
            .and_then(|a| a.page(inputs[index].number))
            .is_some_and(|a| !a.groups.is_empty());
        if answered_groups {
            continue;
        }
        fact.groups = deterministic_groups_for(fact, certificate_numbers[index].as_deref());
    }
    chain_certificate_pages(facts, inputs, certificate_numbers, answers);
}

fn deterministic_groups_for(fact: &PageFacts, certificate: Option<&str>) -> Vec<String> {
    let mut groups = Vec::new();
    if fact.labels.iter().any(|l| l == "application_form") {
        groups.push("application".to_string());
    }
    if let Some(number) = certificate {
        groups.push(format!("certificate:{number}"));
    }
    if fact.labels.iter().any(|l| l == "cadastral_reference") {
        groups.push("cadastre_references".to_string());
    }
    if fact.labels.iter().any(|l| is_drawing_label(l)) {
        groups.push("drawings".to_string());
    }
    groups
}

fn certificate_number(fact: &PageFacts) -> Option<String> {
    if !fact.labels.iter().any(|l| l == "cadastral_certificate") {
        return None;
    }
    fact.entities
        .iter()
        .map(|(id, _)| entities::split_id(id))
        .find(|(entity_type, _)| *entity_type == "certificate_number")
        .map(|(_, value)| value.to_string())
}

/// A certificate's continuation page follows it: chain consecutive pages of
/// the same certificate with `continues`.
fn chain_certificate_pages(
    facts: &mut [PageFacts],
    inputs: &[PageInputs],
    certificate_numbers: &[Option<String>],
    answers: Option<&Answers>,
) {
    for index in 0..facts.len().saturating_sub(1) {
        // A page whose answer exists supplies its own relations; the chain
        // fills in only for pages no reviewer or vision pass touched.
        if answers.and_then(|a| a.page(inputs[index].number)).is_some() {
            continue;
        }
        let Some(current) = &certificate_numbers[index] else {
            continue;
        };
        if certificate_numbers[index + 1].as_ref() == Some(current)
            && inputs[index + 1].number == inputs[index].number + 1
        {
            facts[index].related.push(RelatedPage {
                page: inputs[index + 1].number,
                relation: Relation::Continues,
            });
        }
    }
}

/// The subject parcel: the first cadastral code the application form names.
fn subject_parcel(facts: &[PageFacts]) -> Option<String> {
    facts
        .iter()
        .find(|f| f.labels.iter().any(|l| l == "application_form"))
        .and_then(|f| {
            f.entities
                .iter()
                .find(|(id, _)| entities::split_id(id).0 == "cadastral_code")
                .map(|(id, _)| id.clone())
        })
}

/// Group metadata in final order: the answers' ordering first, deterministic
/// ids after, in first-reference page order.
fn group_metadata(
    facts: &[PageFacts],
    certificate_numbers: &[Option<String>],
    answers: Option<&Answers>,
    subject: Option<&str>,
) -> IndexMap<String, (String, String)> {
    let mut meta = IndexMap::new();
    if let Some(answers) = answers {
        for (id, group) in &answers.groups {
            meta.insert(id.clone(), (group.kind.clone(), group.title.clone()));
        }
    }
    for fact in facts {
        for id in &fact.groups {
            if !meta.contains_key(id) {
                let value = deterministic_group_meta(id, subject, facts, certificate_numbers);
                meta.insert(id.clone(), value);
            }
        }
    }
    meta
}

fn deterministic_group_meta(
    id: &str,
    subject: Option<&str>,
    facts: &[PageFacts],
    certificate_numbers: &[Option<String>],
) -> (String, String) {
    if id == "application" {
        return ("application".into(), "Design-permission application".into());
    }
    if id == "cadastre_references" {
        return ("ownership".into(), "Cadastre committee references".into());
    }
    if id == "drawings" {
        return ("drawings".into(), "Drawings and plans".into());
    }
    if let Some(number) = id.strip_prefix("certificate:") {
        // The certificate for the parcel under application is the ownership
        // chain; any other certificate belongs to somebody else's parcel.
        let is_subject = subject.is_some_and(|parcel| {
            facts.iter().zip(certificate_numbers).any(|(f, cert)| {
                cert.as_deref() == Some(number) && f.entities.iter().any(|(e, _)| e == parcel)
            })
        });
        return if is_subject {
            (
                "ownership".into(),
                format!("Ownership certificate {number}"),
            )
        } else {
            ("administrative".into(), format!("Certificate {number}"))
        };
    }
    ("administrative".into(), id.to_string())
}

/// Per-group accumulation while the pages are walked.
#[derive(Default)]
struct GroupAccumulator {
    pages: BTreeSet<usize>,
    entities: BTreeSet<String>,
}

/// Per-entity accumulation: the first printed value seen, and every mention.
#[derive(Default)]
struct EntityAccumulator {
    value: Option<String>,
    sources: Vec<EntitySource>,
}

fn emit_report(
    analysis: &DossierAnalysis,
    facts: Vec<PageFacts>,
    group_meta: IndexMap<String, (String, String)>,
    answers: Option<&Answers>,
    options: &DossierOptions,
    vector_pages: &HashMap<usize, &PageVector>,
) -> Result<DossierReport> {
    let document = analysis.vector.document.clone();
    let mut stats = Stats::default();
    let mut pages_out = Vec::with_capacity(facts.len());

    // Registries the derivations fill as pages are walked, in page order —
    // the graph's edge order is the reader's reading order. The entity
    // registry is a BTreeMap because the serialized list is sorted by id.
    let mut entity_registry: BTreeMap<String, EntityAccumulator> = BTreeMap::new();
    let mut group_accumulators: HashMap<String, GroupAccumulator> = HashMap::new();
    let mut edges: Vec<Edge> = Vec::new();
    let mut placed: BTreeSet<usize> = BTreeSet::new();
    let mut geojson_dir_ready = false;

    for (input, fact) in analysis.inputs.iter().zip(facts) {
        let n = input.number;
        let vector_page = vector_pages.get(&n).copied();

        match fact.route {
            Route::EmbeddedText => stats.embedded_text += 1,
            Route::Vlm => stats.vlm += 1,
            Route::None => stats.none += 1,
        }

        let mut extraction = Extraction {
            routing_signals: input.reasons.iter().map(|r| r.slug().to_string()).collect(),
            route: fact.route,
            encoding_issue: fact.encoding_issue,
            ..Default::default()
        };
        if input.embedded() {
            extraction.engine = Some("pdfium");
            extraction.text = Some(input.text.clone());
            extraction.chars = Some(input.chars);
        }
        if !fact.vlm_reasons.is_empty() {
            stats.vlm_flagged += 1;
            let extracted = match (&fact.title, &fact.notes) {
                (None, None) => None,
                (title, notes) => Some(VlmExtracted {
                    summary: title.clone(),
                    notes: notes.clone(),
                }),
            };
            extraction.vlm = Some(VlmBlock {
                required: true,
                reasons: fact.vlm_reasons.clone(),
                extracted,
            });
        }

        let mut qr_codes: Vec<QrCode> = input
            .qr_payloads
            .iter()
            .map(|data| QrCode {
                data: Some(data.clone()),
                note: None,
            })
            .collect();
        if qr_codes.is_empty() && fact.labels.iter().any(|l| l == "qr") {
            qr_codes.push(QrCode {
                data: None,
                note: Some("qr_visible_not_decoded".to_string()),
            });
        }
        if !qr_codes.is_empty() {
            stats.qr_pages += 1;
        }
        if fact.handwriting.is_some() {
            stats.handwriting_pages += 1;
        }

        // How the values on this page were actually obtained: the route.
        // A legacy-encoded page is VLM-flagged for a future pass, but its
        // regex-read identifiers came from the embedded layer all the same.
        let via = Via::from(fact.route);
        for (id, value) in &fact.entities {
            let entry = entity_registry.entry(id.clone()).or_default();
            entry.value.get_or_insert_with(|| value.clone());
            entry.sources.push(EntitySource { page: n, via });
            edges.push(Edge {
                from: format!("page:{n}"),
                to: format!("entity:{id}"),
                edge_type: EdgeType::Mentions,
                via: Some(via),
            });
        }
        for group in &fact.groups {
            let accumulator = group_accumulators.entry(group.clone()).or_default();
            accumulator.pages.insert(n);
            accumulator
                .entities
                .extend(fact.entities.iter().map(|(id, _)| id.clone()));
            edges.push(Edge {
                from: format!("page:{n}"),
                to: format!("group:{group}"),
                edge_type: EdgeType::MemberOf,
                via: None,
            });
        }
        for related in &fact.related {
            edges.push(Edge {
                from: format!("page:{n}"),
                to: format!("page:{}", related.page),
                edge_type: related.relation.into(),
                via: None,
            });
        }

        let georeference = vector_page
            .and_then(|v| v.georeference.as_ref())
            .filter(|g| g.placed)
            .map(|g| serde_json::to_value(g).expect("georeference serializes"));
        if georeference.is_some() {
            stats.placed_pages += 1;
            placed.insert(n);
        }

        let cad = build_cad(
            &document,
            n,
            &fact.labels,
            vector_page,
            options,
            &mut geojson_dir_ready,
        )?;

        pages_out.push(PageAnnotation {
            page_number: n,
            labels: fact.labels,
            title: fact.title,
            sheet: fact.sheet,
            notes: fact.notes,
            anomaly: fact.anomaly,
            classification: vector_page.map(|v| Classification {
                class: v.classification.class,
                confidence: v.classification.confidence,
                is_drawing: v.classification.is_drawing,
                is_mappable: v.classification.is_mappable,
            }),
            content_kind: fact.content_kind,
            languages: fact.languages,
            declared_scale_denominator: fact.declared_scale,
            extraction,
            qr_codes,
            handwriting: fact.handwriting,
            entities: fact.entities.into_iter().map(|(id, _)| id).collect(),
            groups: fact.groups,
            related_pages: fact.related,
            georeference,
            cad,
        });
    }

    let mut groups_out = Vec::new();
    for (id, (kind, title)) in &group_meta {
        let Some(accumulator) = group_accumulators.get(id) else {
            continue; // metadata for a group no page references
        };
        let georeference = (kind == "drawings").then(|| {
            let member_placed: Vec<usize> = accumulator
                .pages
                .iter()
                .copied()
                .filter(|p| placed.contains(p))
                .collect();
            if !member_placed.is_empty() {
                GroupGeoreference {
                    status: GeorefStatus::Applied,
                    source_pages: member_placed.clone(),
                    applies_to_pages: member_placed,
                }
            } else if !placed.is_empty() {
                // A placed page elsewhere fixes the site, not this group's
                // drawing frame — geometry decides, not proximity.
                GroupGeoreference {
                    status: GeorefStatus::SameSiteUnverified,
                    source_pages: placed.iter().copied().collect(),
                    applies_to_pages: Vec::new(),
                }
            } else {
                GroupGeoreference {
                    status: GeorefStatus::None,
                    source_pages: Vec::new(),
                    applies_to_pages: Vec::new(),
                }
            }
        });
        if let Some(geo) = &georeference {
            for source in &geo.source_pages {
                edges.push(Edge {
                    from: format!("page:{source}"),
                    to: format!("group:{id}"),
                    edge_type: EdgeType::GeoreferenceSource,
                    via: None,
                });
            }
        }
        groups_out.push(Group {
            id: id.clone(),
            kind: kind.clone(),
            title: title.clone(),
            pages: accumulator.pages.iter().copied().collect(),
            entities: accumulator.entities.iter().cloned().collect(),
            georeference,
        });
    }

    let mut entities_out = Vec::new();
    for (id, accumulator) in &entity_registry {
        let (value, roles) = describe_entity(id, answers, accumulator.value.as_deref());
        entities_out.push(Entity {
            id: id.clone(),
            entity_type: entities::split_id(id).0.to_string(),
            value,
            roles,
            sources: accumulator.sources.clone(),
        });
    }
    if let Some(answers) = answers {
        for (id, meta) in &answers.people {
            if !entity_registry.contains_key(id) {
                entities_out.push(Entity {
                    id: id.clone(),
                    entity_type: entities::split_id(id).0.to_string(),
                    value: meta.value.clone(),
                    roles: meta.roles.clone(),
                    sources: Vec::new(),
                });
            }
        }
    }

    for group in &groups_out {
        for entity in &group.entities {
            if entities::split_id(entity).0 == "cadastral_code" {
                edges.push(Edge {
                    from: format!("group:{}", group.id),
                    to: format!("entity:{entity}"),
                    edge_type: EdgeType::About,
                    via: None,
                });
            }
        }
    }

    Ok(DossierReport {
        schema_version: SCHEMA_VERSION,
        document,
        source: analysis.source.clone(),
        page_count: analysis.vector.page_count,
        generated_by: generated_by(answers.is_some()),
        stats,
        pages: pages_out,
        groups: groups_out,
        entities: entities_out,
        graph: Graph { edges },
        georeference_note: answers.and_then(|a| a.georef_note.clone()),
        note: answers.and_then(|a| a.note.clone()),
        warnings: analysis.vector.warnings.clone(),
    })
}

/// The value and roles an entity is listed with: the answers' people map is
/// authoritative, the first printed value second, the id's own value segment
/// last.
fn describe_entity(
    id: &str,
    answers: Option<&Answers>,
    printed: Option<&str>,
) -> (String, Vec<String>) {
    if let Some(meta) = answers.and_then(|a| a.people.get(id)) {
        return (meta.value.clone(), meta.roles.clone());
    }
    let value = printed
        .map(str::to_string)
        .unwrap_or_else(|| fallback_value(id));
    (value, Vec::new())
}

pub(crate) fn fallback_value(id: &str) -> String {
    entities::split_id(id).1.to_string()
}

/// The CAD reference for a drawing page, writing the GeoJSON beside the
/// report when an output directory was given.
fn build_cad(
    document: &str,
    page_number: usize,
    labels: &[String],
    vector_page: Option<&PageVector>,
    options: &DossierOptions,
    geojson_dir_ready: &mut bool,
) -> Result<Option<Cad>> {
    let is_drawing_page = labels.iter().any(|l| is_drawing_label(l));
    let (Some(vector_page), true) = (vector_page, is_drawing_page) else {
        return Ok(None);
    };

    let mut geojson_path = None;
    if let Some(dir) = &options.output_dir {
        let file_name = pdf_vector::page_geojson_file_name(document, page_number);
        let relative = format!("geojson/{document}/{file_name}");
        let full = dir.join(&relative);
        if !*geojson_dir_ready {
            std::fs::create_dir_all(full.parent().expect("path has a parent"))
                .with_context(|| format!("cannot create '{}'", full.display()))?;
            *geojson_dir_ready = true;
        }
        pdf_vector::write_page_geojson(&full, &vector_page.geojson)?;
        geojson_path = Some(relative);
    }

    Ok(Some(Cad {
        crs: vector_page.crs.clone(),
        features: vector_page.summary.features,
        geojson: geojson_path,
    }))
}

fn generated_by(merged: bool) -> String {
    let base = format!(
        "pdf-toolkit {} dossier: deterministic extraction (pdfium text, templates, rxing QR, vector classification and georeferencing)",
        env!("CARGO_PKG_VERSION")
    );
    if merged {
        format!("{base} + vision-pass answers")
    } else {
        format!("{base}; vision-pass answers pending")
    }
}
