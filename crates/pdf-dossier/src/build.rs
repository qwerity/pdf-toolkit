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

use crate::answers::{Answers, FactAnswer, PageAnswer};
use crate::entities::{self, Found, Template};
use crate::facts::{self, Fact, FactKind};
use crate::report::*;
use crate::trust::{Trust, group_trust, looks_like_extract, page_trust};
use crate::{access, checks, indicators, legacy, numbers, qr, request, text_signals};

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
    trust: Trust,
}

/// Builds the whole-dossier annotation in one call.
pub fn build_dossier(
    pdfium: &Pdfium,
    path: &Path,
    options: &DossierOptions,
) -> Result<DossierReport> {
    assemble(&analyze(pdfium, path, &VectorOptions::default())?, options)
}

/// The expensive half: reads text, routing signals, QR payloads and the
/// vector geometry for every page. `vector` carries what the caller knows
/// about the site — parcel polygons for sheets that print no table.
pub fn analyze(
    pdfium: &Pdfium,
    path: &Path,
    vector_options: &VectorOptions,
) -> Result<DossierAnalysis> {
    let vector = pdf_vector::extract_document(path, &[], vector_options).with_context(|| {
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
                Some(answer) => answered_facts(input, answer, vector_page),
                None => deterministic_facts(input, vector_page),
            }
        })
        .collect();

    let certificate_numbers: Vec<Option<String>> = facts.iter().map(certificate_number).collect();
    derive_groups(&mut facts, &analysis.inputs, answers, &certificate_numbers);

    let subject = subject_parcel(&facts);
    let group_meta = group_metadata(&facts, &certificate_numbers, answers, subject.as_deref());

    let page_texts: Vec<request::PageText<'_>> = analysis
        .inputs
        .iter()
        .zip(&facts)
        .filter(|(input, _)| input.embedded())
        .map(|(input, fact)| request::PageText {
            number: input.number,
            labels: &fact.labels,
            text: &input.text,
        })
        .collect();
    let request = request::detect(&page_texts);

    // The figures every page states, then the comparisons across them.
    let page_facts = document_facts(
        &analysis.inputs,
        &facts,
        answers,
        request.as_ref(),
        &vector_pages,
    );
    let application_code = subject
        .as_deref()
        .map(|id| entities::split_id(id).1.to_string());
    // The credentials the Committee's documents print, read off the
    // (legacy-decoded) text of the pages labelled as such, and the view of
    // each page the checks project their own page lists from — one pass,
    // since both are the same question asked of the same page.
    let mut access: Vec<access::DocumentAccess> = Vec::new();
    let mut page_views: Vec<checks::PageView> = Vec::new();
    for (input, fact) in analysis.inputs.iter().zip(&facts) {
        page_views.push(checks::PageView {
            number: input.number,
            trust: fact.trust,
            labels: fact.labels.clone(),
            groups: fact.groups.clone(),
        });
        let printed = access::page_access(
            input.number,
            &fact.labels,
            &legacy::decode_legacy(&input.text),
        );
        if printed.is_empty() {
            // A scanned header is the model's to read. Two readings of one
            // secret would be a misread, so the printed pair always wins.
            access.extend(
                answers
                    .and_then(|a| a.page(input.number))
                    .map(|a| access::answered_access(input.number, &a.access))
                    .unwrap_or_default(),
            );
        } else {
            access.extend(printed);
        }
    }
    let page_checks = checks::run(
        &page_facts,
        application_code.as_deref(),
        &page_views,
        &access,
    );

    emit_report(
        analysis,
        facts,
        group_meta,
        request,
        page_facts,
        page_checks,
        access,
        answers,
        options,
        &vector_pages,
    )
}

/// Every fact every page states: read deterministically off the (legacy-
/// decoded) text, taken from a vision answer where one names figures, and
/// implied by a placed sheet's turning-point table.
fn document_facts(
    inputs: &[PageInputs],
    facts: &[PageFacts],
    answers: Option<&Answers>,
    request: Option<&request::Request>,
    vector_pages: &HashMap<usize, &PageVector>,
) -> Vec<Fact> {
    let mut out = Vec::new();
    for (input, fact) in inputs.iter().zip(facts) {
        let decoded = legacy::decode_legacy(&input.text);
        let page = facts::PageInput {
            page: input.number,
            labels: &fact.labels,
            text: if input.embedded() { &decoded } else { "" },
            trust: fact.trust,
            via: Via::from(fact.route),
        };
        let on_form = request.is_some_and(|r| r.page == input.number);
        out.extend(facts::page_facts(
            &page,
            request
                .filter(|_| on_form)
                .and_then(|r| r.address.as_deref()),
            request
                .filter(|_| on_form)
                .and_then(|r| r.planned.as_deref()),
        ));
        if let Some(answer) = answers.and_then(|a| a.page(input.number)) {
            out.extend(answered_page_facts(&page, answer));
        }
        if let Some(vector_page) = vector_pages.get(&input.number) {
            for feature in &vector_page.geojson.features {
                let is_survey = feature
                    .property("kind")
                    .and_then(|k| k.as_str())
                    .is_some_and(|k| k == "survey_parcel");
                if is_survey && let Ok(value) = serde_json::to_value(feature) {
                    out.extend(facts::survey_facts(&page, &value));
                }
            }
        }
    }
    out
}

/// The words a page prints before the permit applicant's name: a Committee
/// reference's requester, a drawing set's client, the developer, a form's
/// applicant line. (`trust::APPLICANT_LABELS` is a different list: page labels.)
const APPLICANT_LINE_LABELS: [&str; 4] = ["դիմող", "պատվիրատու", "կառուցապատող", "հայտատու"];

/// The page's text, lower-cased, when it is an Armenian text layer that can
/// vouch for a name; none for a scan or a layer of stray glyphs.
fn vouching_text(page: &facts::PageInput<'_>) -> Option<String> {
    text_signals::languages(page.text)
        .iter()
        .any(|lang| lang == "hy")
        .then(|| page.text.to_lowercase())
}

/// Whether a vision-read owner or applicant is a name the page prints.
///
/// The model is asked for the label before each name, and an applicant
/// whose label is not an applicant label (a title block's designer) is not
/// one. On a page with a text layer (`vouching_text`) the name's surname or
/// given name must also be in that text: on plan10 p15 the model reported an
/// "applicant" whose name the page does not print anywhere, and a name nobody
/// printed must not become a witness. Answers recorded before labels were
/// asked carry none and meet only the second test; a scan has no text to meet
/// it and keeps what the model read.
fn printed_on_page(text: Option<&str>, fact: &FactAnswer, kind: FactKind) -> bool {
    if !matches!(kind, FactKind::Owner | FactKind::Applicant) {
        return true;
    }
    let other_label = fact.label.as_deref().is_some_and(|label| {
        let label = label.to_lowercase();
        !APPLICANT_LINE_LABELS
            .iter()
            .any(|stem| label.contains(stem))
    });
    if kind == FactKind::Applicant && other_label {
        return false;
    }
    let Some(text) = text else {
        return true;
    };
    entities::name_words(&fact.value).into_iter().any(|word| {
        let word = word.to_lowercase();
        let word = word.trim_matches(|c: char| !c.is_alphabetic());
        word.chars().count() >= 3 && text.contains(word)
    })
}

/// A vision answer's figures as facts: the model transcribed, the toolkit
/// converts and keys them the same way it keys its own readings.
fn answered_page_facts(page: &facts::PageInput<'_>, answer: &PageAnswer) -> Vec<Fact> {
    let names_a_person = |f: &FactAnswer| matches!(f.kind.as_str(), "owner" | "applicant");
    let text = if answer.facts.iter().any(names_a_person) {
        vouching_text(page)
    } else {
        None
    };
    answer
        .facts
        .iter()
        .filter_map(|f| {
            let kind = match f.kind.as_str() {
                "cadastral_code" => FactKind::CadastralCode,
                "parcel_area" => FactKind::ParcelArea,
                "footprint_area" => FactKind::FootprintArea,
                "building_area" => FactKind::BuildingArea,
                "planned_area" => FactKind::PlannedArea,
                "owner" => FactKind::Owner,
                "applicant" => FactKind::Applicant,
                "address" => FactKind::Address,
                "certificate_number" => FactKind::CertificateNumber,
                "signatory" => FactKind::Signatory,
                "identity_holder" => FactKind::IdentityHolder,
                _ => return None,
            };
            if !printed_on_page(text.as_deref(), f, kind) {
                return None;
            }
            let mut fact = Fact {
                kind,
                number: None,
                text: None,
                printed: f.value.trim().to_string(),
                page: page.page,
                trust: page.trust,
                via: Via::Vlm,
                subject: f.subject.clone(),
                about: None,
            };
            match kind {
                FactKind::ParcelArea
                | FactKind::FootprintArea
                | FactKind::BuildingArea
                | FactKind::PlannedArea => {
                    let factor = f
                        .unit
                        .as_deref()
                        .and_then(numbers::unit_factor)
                        .unwrap_or(1.0);
                    fact.number = Some(numbers::parse_number(&f.value)? * factor);
                }
                // Every person-valued fact is canonicalised the same way, so a
                // signature reads equal to the passport that names the same
                // person however either spelled or cased it.
                FactKind::Owner
                | FactKind::Applicant
                | FactKind::Signatory
                | FactKind::IdentityHolder => {
                    fact.text = Some(entities::entity_id("person", f.value.trim()));
                }
                FactKind::Address => fact.text = Some(facts::address_key(&f.value)),
                FactKind::CertificateNumber => fact.text = Some(facts::certificate_key(&f.value)),
                _ => fact.text = Some(f.value.trim().to_string()),
            }
            Some(fact)
        })
        .collect()
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
fn answered_facts(
    input: &PageInputs,
    answer: &PageAnswer,
    vector_page: Option<&PageVector>,
) -> PageFacts {
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
        trust: page_trust(
            &answer.labels,
            &legacy::decode_legacy(&input.text),
            true,
            extract_signal(input, vector_page, answer.sheet.as_deref()),
        ),
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

    let labels = deterministic_labels(template, vector_page, &input.qr_payloads, route);
    let sheet = embedded
        .then(|| entities::sheet_designation(&input.text))
        .flatten();
    let trust = page_trust(
        &labels,
        &legacy::decode_legacy(&input.text),
        embedded,
        extract_signal(input, vector_page, sheet.as_deref()),
    );
    PageFacts {
        labels,
        trust,
        sheet,
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

/// Whether the page reads as one of the Committee's map extracts.
fn extract_signal(
    input: &PageInputs,
    vector_page: Option<&PageVector>,
    sheet: Option<&str>,
) -> bool {
    let Some(v) = vector_page else { return false };
    let is_map_class = matches!(
        v.classification.class,
        PageClass::CadastralPlan | PageClass::TopographicSurvey
    );
    let codes = entities::extract(&input.text)
        .into_iter()
        .filter(|f| entities::split_id(&f.id).0 == "cadastral_code")
        .count();
    looks_like_extract(
        is_map_class,
        v.classification.features.has_control_table,
        codes,
        sheet,
        &input.text,
    )
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
            // Geometry decides: a sheet that placed drew the parcel well
            // enough to be fitted to its survey, whatever the structural
            // heuristic made of its line density. Its text layer, where it
            // has one, is still text.
            Some(v) if v.georeference.as_ref().is_some_and(|g| g.placed) => {
                if route == Route::EmbeddedText {
                    vec!["cad_drawing".into(), "text_document".into()]
                } else {
                    vec!["cad_drawing".into()]
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

/// Strikes this page's passcodes out of every free-text field of a finished
/// annotation, leaving the certificate number and everything else intact.
///
/// ONE CHOKE POINT, DELIBERATELY. The passcode is a bearer credential: it is
/// kept once, structurally, in `groups[].access`, and must appear nowhere a
/// reader would not expect a secret. Redacting field by field at each
/// assignment meant the next text-bearing field added to `PageAnnotation`
/// would leak by omission — `extraction.vlm.extracted` already did, since it
/// is built from the same title and notes before they were cleaned. Running
/// over the assembled annotation instead makes coverage the default.
///
/// `qr_codes[].data` is deliberately NOT redacted: its contract is the
/// payload as decoded, a reference letter's QR encodes the register's own
/// link, and truncating it would misreport what the page carries.
fn redact_page_passcodes(page: &mut PageAnnotation, access: &[access::DocumentAccess]) {
    let codes: Vec<&str> = access
        .iter()
        .filter(|a| a.page == page.page_number)
        .map(|a| a.passcode.as_str())
        .collect();
    if codes.is_empty() {
        return;
    }
    let strike = |text: &mut String| {
        for code in &codes {
            if text.contains(*code) {
                *text = text.replace(*code, "[գաղտնաբառ]");
            }
        }
    };
    let mut fields: Vec<&mut String> = Vec::new();
    fields.extend(page.title.as_mut());
    fields.extend(page.notes.as_mut());
    fields.extend(page.extraction.text.as_mut());
    if let Some(vlm) = page.extraction.vlm.as_mut()
        && let Some(extracted) = vlm.extracted.as_mut()
    {
        fields.extend(extracted.summary.as_mut());
        fields.extend(extracted.notes.as_mut());
    }
    for field in fields {
        strike(field);
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_report(
    analysis: &DossierAnalysis,
    facts: Vec<PageFacts>,
    group_meta: IndexMap<String, (String, String)>,
    request: Option<request::Request>,
    page_facts: Vec<Fact>,
    page_checks: Vec<checks::Check>,
    access: Vec<access::DocumentAccess>,
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
    let mut page_trusts: HashMap<usize, Trust> = HashMap::new();
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

        page_trusts.insert(n, fact.trust);
        let mut annotation = PageAnnotation {
            page_number: n,
            labels: fact.labels,
            trust: fact.trust,
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
        };
        redact_page_passcodes(&mut annotation, &access);
        pages_out.push(annotation);
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
            trust: group_trust(
                accumulator
                    .pages
                    .iter()
                    .filter_map(|p| page_trusts.get(p).copied()),
            ),
            title: title.clone(),
            pages: accumulator.pages.iter().copied().collect(),
            entities: accumulator.entities.iter().cloned().collect(),
            georeference,
            access: if kind == "ownership" || kind == "administrative" {
                access
                    .iter()
                    .filter(|a| accumulator.pages.contains(&a.page))
                    .cloned()
                    .collect()
            } else {
                Vec::new()
            },
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

    let embedded: Vec<(usize, &str)> = analysis
        .inputs
        .iter()
        .filter(|input| input.embedded())
        .map(|input| (input.number, input.text.as_str()))
        .collect();

    Ok(DossierReport {
        schema_version: SCHEMA_VERSION,
        document,
        source: analysis.source.clone(),
        llm_usage: None,
        page_count: analysis.vector.page_count,
        generated_by: generated_by(answers.is_some()),
        stats,
        request,
        indicators: indicators::read(&embedded),
        facts: page_facts,
        checks: page_checks,
        pages: pages_out,
        groups: groups_out,
        entities: entities_out,
        graph: Graph { edges },
        georeference_note: answers.and_then(|a| a.georef_note.clone()),
        note: answers.and_then(|a| a.note.clone()),
        warnings: analysis.vector.warnings.clone(),
        answers_file: None,
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
    // A placed page carries georeferenced geometry whatever it is labelled:
    // the export is the one thing a caller cannot get elsewhere.
    let placed = vector_page.is_some_and(|v| v.georeference.as_ref().is_some_and(|g| g.placed));
    let is_drawing_page = placed || labels.iter().any(|l| is_drawing_label(l));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn fact(kind: &str, value: &str, label: Option<&str>) -> FactAnswer {
        FactAnswer {
            kind: kind.to_string(),
            value: value.to_string(),
            unit: None,
            subject: None,
            label: label.map(str::to_string),
        }
    }

    /// What `answered_page_facts` hands `printed_on_page` for a page of this text.
    fn text(text: &str) -> Option<String> {
        vouching_text(&facts::PageInput {
            page: 15,
            labels: &[],
            text,
            trust: Trust::Applicant,
            via: Via::EmbeddedText,
        })
    }

    const TITLE_BLOCK: &str = "Տնօրեն Կ.Մինասյան ՆԳՃ Ս.Մելքոնյան Նախագծեց Ս.Մելքոնյան \
        ԿԱԴԱՍՏՐԱՅԻՆ ԾԱԾԿԱԳՐԻ ՀՈՂԱՄԱՍՈՒՄ ԲՆԱԿԵԼԻ ՏԱՆ ԿԱՌՈՒՑՄԱՆ ԱՇԽԱՏԱՆՔԱՅԻՆ ՆԱԽԱԳԻԾ";

    #[test]
    fn a_vision_applicant_the_page_does_not_print_is_dropped() {
        let invented = fact("applicant", "ՍՈՆԱ ՄԵԼԻՔՅԱՆ", None);
        let page = text(TITLE_BLOCK);
        assert!(!printed_on_page(
            page.as_deref(),
            &invented,
            FactKind::Applicant
        ));
    }

    #[test]
    fn a_designer_labelled_as_such_is_not_the_applicant() {
        let designer = fact("applicant", "Ս. Մելքոնյան", Some("Նախագծեց"));
        let page = text(TITLE_BLOCK);
        assert!(!printed_on_page(
            page.as_deref(),
            &designer,
            FactKind::Applicant
        ));
    }

    #[test]
    fn a_printed_applicant_after_a_client_label_is_kept() {
        let page = text("ՊԱՏՎԻՐԱՏՈՒ՝ Վահե Դանիելյան ԲՆԱԿԵԼԻ ՏԱՆ ԿԱՌՈՒՑՄԱՆ ԱՇԽԱՏԱՆՔԱՅԻՆ ՆԱԽԱԳԻԾ");
        let client = fact("applicant", "Վահե Դանիելյան", Some("ՊԱՏՎԻՐԱՏՈՒ՝"));
        assert!(printed_on_page(
            page.as_deref(),
            &client,
            FactKind::Applicant
        ));
        let recorded = fact("applicant", "ՎԱՀԵ ԴԱՆԻԵԼՅԱՆ ՍԱՐԳՍԻ", None);
        assert!(
            printed_on_page(page.as_deref(), &recorded, FactKind::Applicant),
            "case-blind, and the patronymic need not be printed"
        );
    }

    #[test]
    fn a_scan_keeps_what_the_model_read() {
        assert_eq!(text(""), None);
        let owner = fact("owner", "ՎԱՀԵ ԴԱՆԻԵԼՅԱՆ ՍԱՐԳՍԻ", None);
        assert!(printed_on_page(None, &owner, FactKind::Owner));
    }

    #[test]
    fn a_handwritten_signatory_is_never_tested_against_the_print() {
        let signatory = fact("signatory", "Ա. Բ.", None);
        let page = text(TITLE_BLOCK);
        assert!(printed_on_page(
            page.as_deref(),
            &signatory,
            FactKind::Signatory
        ));
    }
}
