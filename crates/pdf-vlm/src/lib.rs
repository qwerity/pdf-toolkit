//! Hosted-vision passes over dossier pages.
//!
//! Three jobs, one discipline. [`VlmClient`] annotates the pages `dossier`
//! flags, returning answers in the dossier answers format
//! (`pdf_dossier::answers`) so the merge path is exactly the one a reviewed
//! answers file takes. [`cad`] asks a model to refine recovered CAD geometry,
//! and [`building`] asks it to read a building's massing out of the drawings
//! — in both, the model only ever *proposes* operations against geometry the
//! toolkit extracted; coordinates never come from the model.
//!
//! **These are the only networked corners of the toolkit.** Everything else
//! is offline and deterministic; using a model here sends page images — which
//! in this domain include identity documents — to the configured provider.
//! That is why every networked path is opt-in per invocation and never a
//! default. Transport, provider routing and keys live in `llm-providers`;
//! the model spec's shape picks the provider (`gemini-*`, `gpt-*`, or
//! `vendor/model`).

use std::io::Cursor;
use std::num::NonZeroU16;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use indexmap::IndexMap;
use pdfium_render::prelude::Pdfium;
use serde::Deserialize;

use llm_providers::{Client, Request};
use pdf_core::page::{open_document, page_index};
use pdf_dossier::answers::{Answers, HandwritingAnswer, PageAnswer, PersonMeta};
use pdf_dossier::entities::entity_id;
use pdf_render::{PngSize, render_page, write_png};

pub mod building;
pub mod cad;

pub use llm_providers::ASTRA_MODEL;

/// The model used when `--vlm` is given without a value: the corpus
/// benchmark's winner on identifier reading, JSON compliance and price
/// (`docs/annotation-findings.md`).
pub const DEFAULT_MODEL: &str = "gemini-3.8-flash";

/// Width a document page is rendered at for annotation. The corpus benchmark
/// ran at this width; identity documents and handwriting stay legible.
pub const RENDER_WIDTH_PX: u32 = 1400;

/// Width a *drawing* is rendered at for the CAD and building passes.
/// Dimension chains and level marks print small; 1400 px loses them.
pub const DRAWING_RENDER_WIDTH_PX: u32 = 2000;

/// A configured connection to one vision model, answering in the dossier
/// answers format.
pub struct VlmClient {
    client: Client,
}

impl VlmClient {
    /// Builds a client from a model spec, reading the key from the
    /// environment (a `.env` in the working directory is honoured).
    ///
    /// The spec's shape picks the provider: `gemini-*` goes to the Gemini
    /// API, `gpt-*` to OpenAI, and a `vendor/model` id to OpenRouter.
    pub fn from_env(model_spec: &str) -> Result<Self> {
        Ok(Self {
            client: Client::from_env(model_spec)?,
        })
    }

    fn model(&self) -> &str {
        self.client.model()
    }

    /// Asks the model to annotate one rendered page. Returns the page answer
    /// plus display metadata for any person/org entities it read.
    pub fn annotate_page(&self, png: &[u8]) -> Result<(PageAnswer, IndexMap<String, PersonMeta>)> {
        let mut request = Request::new(PAGE_PROMPT);
        request.images_png.push(png.to_vec());
        let reply = self
            .client
            .ask(&request)
            .with_context(|| format!("vision request to '{}' failed", self.model()))?;
        parse_reply(&reply.text)
            .with_context(|| format!("could not parse '{}' reply as a page answer", self.model()))
    }

    /// Renders and annotates the given pages of a document, assembling the
    /// result as an [`Answers`] set ready for the dossier merge. Progress
    /// goes to stderr; stdout stays reserved for the JSON contract.
    ///
    /// A page that fails does not fail the run: an unrenderable page, an
    /// exhausted retry or an unparseable reply is returned as a warning,
    /// the page keeps its deterministic facts and its standing `vlm` work
    /// order, and every already-paid answer is kept.
    pub fn annotate_pages(
        &self,
        pdfium: &Pdfium,
        pdf_path: &Path,
        page_numbers: &[usize],
    ) -> Result<(Answers, Vec<String>)> {
        let document = open_document(pdfium, pdf_path).map_err(anyhow::Error::from)?;
        let pages = document.pages();
        let mut answers = Answers::default();
        let mut warnings = Vec::new();

        for (done, &number) in page_numbers.iter().enumerate() {
            eprintln!(
                "vlm: page {number} ({}/{} flagged) -> {}",
                done + 1,
                page_numbers.len(),
                self.model()
            );
            let annotated = render_page_png(pages, number, RENDER_WIDTH_PX)
                .and_then(|png| self.annotate_page(&png));
            match annotated {
                Ok((answer, people)) => {
                    answers.pages.insert(number, answer);
                    for (id, meta) in people {
                        answers.people.entry(id).or_insert(meta);
                    }
                }
                Err(error) => warnings.push(format!(
                    "vision pass failed on page {number}; its work order stands: {error:#}"
                )),
            }
        }
        Ok((answers, warnings))
    }
}

/// Renders one page of an open document to PNG bytes at the given width.
pub(crate) fn render_page_png(
    pages: &pdfium_render::prelude::PdfPages<'_>,
    number: usize,
    width_px: u32,
) -> Result<Vec<u8>> {
    let page_number = NonZeroU16::new(u16::try_from(number).unwrap_or(0))
        .ok_or_else(|| anyhow!("page number {number} out of range"))?;
    let page = pages
        .get(page_index(page_number))
        .map_err(|error| anyhow!("cannot load page {number}: {error}"))?;
    let bitmap = render_page(&page, PngSize::WidthPx(width_px)).map_err(anyhow::Error::from)?;
    let mut cursor = Cursor::new(Vec::new());
    write_png(&bitmap, &mut cursor, Path::new("<memory>")).map_err(anyhow::Error::from)?;
    Ok(cursor.into_inner())
}

/// What the model is asked, and the shape it must answer in.
///
/// Deliberately does **not** ask for groups: grouping is derived
/// deterministically from labels and entities, and per-page group guesses
/// from a model would fight that derivation.
const PAGE_PROMPT: &str = r#"You are annotating one page of an Armenian building-permit dossier.
Look at the page image and return ONLY a JSON object (no markdown fences) with:
{
  "labels": [..],        // multi-label, most specific first, from this vocabulary (extensible):
                         // application_form, cadastral_certificate, cadastral_reference, passport,
                         // id_card, social_card, power_of_attorney, consent_statement, official_letter,
                         // decision, permit_document, receipt, contract, license, technical_conditions,
                         // expert_conclusion, photo_montage, cover_page, table_of_contents, text_document,
                         // cad_drawing, site_plan, general_plan, topographic_survey, cadastral_plan,
                         // floor_plan, elevation_or_section, technical_detail, roof_plan, 3d_view,
                         // scanned_drawing, handwritten, signature, e_signature, stamp, qr, photo,
                         // map, table, low_quality_scan
  "kind": "digital" | "scan" | "mixed",   // born-digital page, scanned/photographed paper, or both
  "lang": [..],          // ISO 639-1 of languages visible on the page (hy, en, ru)
  "title": "...",        // one line, in Armenian, saying what the page is
  "handwriting": {"present": true/false, "extent": "signatures"|"fields"|"notes"|"full_page"},
  "entities": [          // every real-world identifier you can actually read on the page
    {"type": "cadastral_code"|"passport_number"|"id_number"|"psn"|"certificate_number"|
             "reference_number"|"person"|"org"|"address"|"date"|"phone"|"email", "value": "..."}
  ]
}
Rules: report only what is visible; transcribe numbers exactly; do not guess
unreadable characters (omit the entity instead); person values keep the original
Armenian/Latin spelling from the page."#;

/// The reply shape the prompt asks for.
#[derive(Debug, Deserialize)]
struct RawAnswer {
    labels: Vec<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    lang: Vec<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    handwriting: Option<RawHandwriting>,
    #[serde(default)]
    entities: Vec<RawEntity>,
}

#[derive(Debug, Deserialize)]
struct RawHandwriting {
    #[serde(default)]
    present: bool,
    #[serde(default)]
    extent: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawEntity {
    #[serde(rename = "type")]
    entity_type: String,
    value: String,
}

/// Parses a model reply — tolerant of markdown fences and prose around the
/// JSON — into the answers format.
fn parse_reply(reply: &str) -> Result<(PageAnswer, IndexMap<String, PersonMeta>)> {
    let raw: RawAnswer = serde_json::from_str(extract_json(reply)?)
        .with_context(|| format!("reply was: {}", reply.chars().take(400).collect::<String>()))?;
    if raw.labels.is_empty() {
        bail!("reply carries no labels");
    }

    let mut people = IndexMap::new();
    let mut entity_ids = Vec::new();
    for entity in &raw.entities {
        let id = entity_id(&entity.entity_type, &entity.value);

        if !entity_ids.contains(&id) {
            if matches!(entity.entity_type.as_str(), "person" | "org") {
                people.entry(id.clone()).or_insert(PersonMeta {
                    value: entity.value.clone(),
                    roles: Vec::new(),
                });
            }
            entity_ids.push(id);
        }
    }

    let answer = PageAnswer {
        labels: raw.labels,
        kind: raw.kind,
        lang: raw.lang,
        title: raw.title,
        handwriting: raw.handwriting.and_then(|h| {
            h.present.then(|| HandwritingAnswer {
                extent: h.extent.unwrap_or_else(|| "fields".to_string()),
                script: Vec::new(),
            })
        }),
        entities: entity_ids,
        ..Default::default()
    };
    Ok((answer, people))
}

/// The JSON object inside a possibly-fenced, possibly-chatty reply.
fn extract_json(reply: &str) -> Result<&str> {
    let start = reply
        .find('{')
        .ok_or_else(|| anyhow!("no JSON object in reply"))?;
    let end = reply
        .rfind('}')
        .ok_or_else(|| anyhow!("unterminated JSON object in reply"))?;
    if end < start {
        bail!("malformed reply");
    }
    Ok(&reply[start..=end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fenced_reply_parses_into_a_page_answer() {
        let reply = "Here is the annotation:\n```json\n{
            \"labels\": [\"passport\", \"photo\"],
            \"kind\": \"scan\",
            \"lang\": [\"hy\", \"en\"],
            \"title\": \"Անձնագիր\",
            \"handwriting\": {\"present\": false},
            \"entities\": [
                {\"type\": \"passport_number\", \"value\": \"AB1234567\"},
                {\"type\": \"person\", \"value\": \"Մկրտչյան Արամ\"}
            ]
        }\n```";
        let (answer, people) = parse_reply(reply).expect("parses");
        assert_eq!(answer.labels, ["passport", "photo"]);
        assert_eq!(answer.kind.as_deref(), Some("scan"));
        assert_eq!(
            answer.entities,
            ["passport_number:AB1234567", "person:mkrtchyan-aram"]
        );
        assert!(answer.handwriting.is_none(), "absent when not present");
        assert_eq!(people["person:mkrtchyan-aram"].value, "Մկրտչյան Արամ");
    }

    #[test]
    fn handwriting_defaults_its_extent_and_duplicates_collapse() {
        let reply = r#"{
            "labels": ["consent_statement", "handwritten"],
            "handwriting": {"present": true},
            "entities": [
                {"type": "cadastral_code", "value": "10-001-0456-0123"},
                {"type": "cadastral_code", "value": "10-001-0456-0123"}
            ]
        }"#;
        let (answer, _) = parse_reply(reply).expect("parses");
        assert_eq!(
            answer.handwriting.as_ref().expect("present").extent,
            "fields"
        );
        assert_eq!(answer.entities, ["cadastral_code:10-001-0456-0123"]);
    }

    #[test]
    fn a_reply_without_labels_is_refused() {
        assert!(parse_reply(r#"{"labels": []}"#).is_err());
        assert!(parse_reply("the model rambled with no JSON at all").is_err());
    }

    /// The vocabulary the prompt teaches must stay aligned with the schema
    /// doc; spot-check the labels a reviewer would miss first.
    #[test]
    fn the_prompt_teaches_the_annotation_vocabulary() {
        for label in [
            "cadastral_certificate",
            "photo_montage",
            "elevation_or_section",
            "general_plan",
            "3d_view",
            "qr",
        ] {
            assert!(PAGE_PROMPT.contains(label), "prompt must teach {label}");
        }
        assert!(PAGE_PROMPT.contains("ONLY a JSON object"));
    }
}
