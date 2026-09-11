//! Vision-model page annotation for the dossier pipeline.
//!
//! `dossier` routes what it cannot read — scans, handwriting, mis-encoded
//! text layers — to a vision pass. This crate *is* that pass, when the caller
//! opts in: it renders a flagged page, sends it to a configured hosted model,
//! and parses the reply into the dossier answers format
//! (`pdf_dossier::answers`), so the merge path is exactly the one a reviewed
//! answers file takes.
//!
//! **This is the only networked corner of the toolkit.** Everything else is
//! offline and deterministic; using a model here sends page images — which in
//! this domain include identity documents — to the configured provider. That
//! is why it is opt-in per invocation (`--vlm <model>`) and never a default.
//!
//! Two providers cover every model worth using: Google's Gemini API
//! (`GEMINI_API_KEY`) for `gemini-*` model ids, and OpenRouter
//! (`OPENROUTER_API_KEY`) for everything in `vendor/model` form. The model
//! spec picks the provider by shape: `gemini-3.8-flash` vs
//! `qwen/qwen2.5-vl-72b-instruct`.

use std::io::Cursor;
use std::num::NonZeroU16;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use indexmap::IndexMap;
use pdfium_render::prelude::Pdfium;
use serde::Deserialize;
use serde_json::{Value, json};

use pdf_core::page::{open_document, page_index};
use pdf_dossier::answers::{Answers, HandwritingAnswer, PageAnswer, PersonMeta};
use pdf_dossier::entities::entity_id;
use pdf_render::{PngSize, render_page, write_png};

/// The model used when `--vlm` is given without a value: the corpus
/// benchmark's winner on identifier reading, JSON compliance and price
/// (`docs/annotation-findings.md`).
pub const DEFAULT_MODEL: &str = "gemini-3.8-flash";

/// Width the page is rendered at for the model. The corpus benchmark ran at
/// this width; identity documents and handwriting stay legible.
pub const RENDER_WIDTH_PX: u32 = 1400;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(180);

/// Which API a model id is served by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    /// Google's Generative Language API; key from `GEMINI_API_KEY`.
    Gemini,
    /// OpenRouter's OpenAI-compatible API; key from `OPENROUTER_API_KEY`.
    OpenRouter,
}

/// A configured connection to one vision model.
pub struct VlmClient {
    provider: Provider,
    model: String,
    api_key: String,
    agent: ureq::Agent,
}

impl VlmClient {
    /// Builds a client from a model spec, reading the key from the
    /// environment (a `.env` in the working directory is honoured).
    ///
    /// The spec's shape picks the provider: a `vendor/model` id goes to
    /// OpenRouter, anything else to the Gemini API.
    pub fn from_env(model_spec: &str) -> Result<Self> {
        // Keys live in the environment; a developer's .env is a convenience,
        // never a requirement, and absence of the file is not an error.
        let _ = dotenvy::dotenv();

        let provider = if model_spec.contains('/') {
            Provider::OpenRouter
        } else {
            Provider::Gemini
        };
        let (variable, fallback) = match provider {
            Provider::Gemini => ("GEMINI_API_KEY", Some("GOOGLE_API_KEY")),
            Provider::OpenRouter => ("OPENROUTER_API_KEY", None),
        };
        let api_key = std::env::var(variable)
            .or_else(|_| {
                fallback
                    .map(std::env::var)
                    .unwrap_or(Err(std::env::VarError::NotPresent))
            })
            .map_err(|_| {
                anyhow!("model '{model_spec}' needs {variable} in the environment (or .env)")
            })?;

        let config = ureq::Agent::config_builder()
            .timeout_global(Some(REQUEST_TIMEOUT))
            .build();
        Ok(Self {
            provider,
            model: model_spec.to_string(),
            api_key,
            agent: config.into(),
        })
    }

    /// Asks the model to annotate one rendered page. Returns the page answer
    /// plus display metadata for any person/org entities it read.
    pub fn annotate_page(&self, png: &[u8]) -> Result<(PageAnswer, IndexMap<String, PersonMeta>)> {
        let reply = self
            .request(png)
            .with_context(|| format!("vision request to '{}' failed", self.model))?;
        parse_reply(&reply)
            .with_context(|| format!("could not parse '{}' reply as a page answer", self.model))
    }

    /// Renders and annotates the given pages of a document, assembling the
    /// result as an [`Answers`] set ready for the dossier merge. Progress
    /// goes to stderr; stdout stays reserved for the JSON contract.
    pub fn annotate_pages(
        &self,
        pdfium: &Pdfium,
        pdf_path: &Path,
        page_numbers: &[usize],
    ) -> Result<Answers> {
        let document = open_document(pdfium, pdf_path).map_err(anyhow::Error::from)?;
        let pages = document.pages();
        let mut answers = Answers::default();

        for (done, &number) in page_numbers.iter().enumerate() {
            eprintln!(
                "vlm: page {number} ({}/{} flagged) -> {}",
                done + 1,
                page_numbers.len(),
                self.model
            );
            let page_number = NonZeroU16::new(u16::try_from(number).unwrap_or(0))
                .ok_or_else(|| anyhow!("page number {number} out of range"))?;
            let page = pages
                .get(page_index(page_number))
                .map_err(|error| anyhow!("cannot load page {number}: {error}"))?;
            let png = render_png(&page)?;

            let (answer, people) = self
                .annotate_page(&png)
                .with_context(|| format!("page {number}"))?;
            answers.pages.insert(number, answer);
            for (id, meta) in people {
                answers.people.entry(id).or_insert(meta);
            }
        }
        Ok(answers)
    }

    fn request(&self, png: &[u8]) -> Result<String> {
        let encoded = base64::engine::general_purpose::STANDARD.encode(png);
        match self.provider {
            Provider::Gemini => self.request_gemini(&encoded),
            Provider::OpenRouter => self.request_openrouter(&encoded),
        }
    }

    fn request_gemini(&self, png_base64: &str) -> Result<String> {
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
            self.model, self.api_key
        );
        let reply: Value = self
            .agent
            .post(&url)
            .send_json(gemini_body(png_base64))?
            .body_mut()
            .read_json()?;
        gemini_text(&reply)
    }

    fn request_openrouter(&self, png_base64: &str) -> Result<String> {
        let reply: Value = self
            .agent
            .post("https://openrouter.ai/api/v1/chat/completions")
            .header("Authorization", &format!("Bearer {}", self.api_key))
            .send_json(openrouter_body(&self.model, png_base64))?
            .body_mut()
            .read_json()?;
        openrouter_text(&reply)
    }
}

/// The Gemini `generateContent` request for one page image.
fn gemini_body(png_base64: &str) -> Value {
    json!({
        "contents": [{"parts": [
            {"inline_data": {"mime_type": "image/png", "data": png_base64}},
            {"text": PAGE_PROMPT},
        ]}],
        "generationConfig": {"temperature": 0, "maxOutputTokens": 4096},
    })
}

/// The concatenated text parts of a Gemini reply.
fn gemini_text(reply: &Value) -> Result<String> {
    reply["candidates"][0]["content"]["parts"]
        .as_array()
        .map(|parts| {
            parts
                .iter()
                .filter_map(|p| p["text"].as_str())
                .collect::<String>()
        })
        .filter(|text| !text.is_empty())
        .ok_or_else(|| anyhow!("no text candidate in Gemini reply: {reply}"))
}

/// The OpenAI-compatible chat request OpenRouter takes.
fn openrouter_body(model: &str, png_base64: &str) -> Value {
    json!({
        "model": model,
        "temperature": 0,
        "max_tokens": 4096,
        "messages": [{"role": "user", "content": [
            {"type": "image_url",
             "image_url": {"url": format!("data:image/png;base64,{png_base64}")}},
            {"type": "text", "text": PAGE_PROMPT},
        ]}],
    })
}

/// The message content of an OpenRouter reply.
fn openrouter_text(reply: &Value) -> Result<String> {
    reply["choices"][0]["message"]["content"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow!("no message content in OpenRouter reply: {reply}"))
}

/// Renders one page to PNG bytes at [`RENDER_WIDTH_PX`].
fn render_png(page: &pdfium_render::prelude::PdfPage<'_>) -> Result<Vec<u8>> {
    let bitmap =
        render_page(page, PngSize::WidthPx(RENDER_WIDTH_PX)).map_err(anyhow::Error::from)?;
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
                         // cad_drawing, site_plan, topographic_survey, cadastral_plan, floor_plan,
                         // elevation_or_section, technical_detail, roof_plan, scanned_drawing,
                         // handwritten, signature, e_signature, stamp, qr, photo, map, table, low_quality_scan
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
    fn the_model_spec_shape_picks_the_provider() {
        // No network is touched here: both fail on the missing key, and the
        // error names which variable the chosen provider needs.
        let gemini = VlmClient::from_env("no-such-model-name");
        let openrouter = VlmClient::from_env("vendor/no-such-model");
        for (result, expected) in [
            (gemini, "GEMINI_API_KEY"),
            (openrouter, "OPENROUTER_API_KEY"),
        ] {
            match result {
                Ok(client) => assert!(
                    !client.api_key.is_empty(),
                    "a key from the environment is fine too"
                ),
                Err(error) => assert!(
                    error.to_string().contains(expected),
                    "error must name {expected}: {error}"
                ),
            }
        }
    }

    #[test]
    fn a_fenced_reply_parses_into_a_page_answer() {
        let reply = "Here is the annotation:\n```json\n{
            \"labels\": [\"passport\", \"photo\"],
            \"kind\": \"scan\",
            \"lang\": [\"hy\", \"en\"],
            \"title\": \"Անձնագիր\",
            \"handwriting\": {\"present\": false},
            \"entities\": [
                {\"type\": \"passport_number\", \"value\": \"AC7654321\"},
                {\"type\": \"person\", \"value\": \"Բադալյան Սուրեն\"}
            ]
        }\n```";
        let (answer, people) = parse_reply(reply).expect("parses");
        assert_eq!(answer.labels, ["passport", "photo"]);
        assert_eq!(answer.kind.as_deref(), Some("scan"));
        assert_eq!(
            answer.entities,
            ["passport_number:AC7654321", "person:badalyan-suren"]
        );
        assert!(answer.handwriting.is_none(), "absent when not present");
        assert_eq!(people["person:badalyan-suren"].value, "Բադալյան Սուրեն");
    }

    #[test]
    fn handwriting_defaults_its_extent_and_duplicates_collapse() {
        let reply = r#"{
            "labels": ["consent_statement", "handwritten"],
            "handwriting": {"present": true},
            "entities": [
                {"type": "cadastral_code", "value": "01-011-0519-0062"},
                {"type": "cadastral_code", "value": "01-011-0519-0062"}
            ]
        }"#;
        let (answer, _) = parse_reply(reply).expect("parses");
        assert_eq!(
            answer.handwriting.as_ref().expect("present").extent,
            "fields"
        );
        assert_eq!(answer.entities, ["cadastral_code:01-011-0519-0062"]);
    }

    #[test]
    fn a_reply_without_labels_is_refused() {
        assert!(parse_reply(r#"{"labels": []}"#).is_err());
        assert!(parse_reply("the model rambled with no JSON at all").is_err());
    }

    #[test]
    fn request_bodies_carry_the_image_and_the_prompt() {
        let gemini = gemini_body("QUJD");
        assert_eq!(
            gemini["contents"][0]["parts"][0]["inline_data"]["data"],
            "QUJD"
        );
        assert_eq!(gemini["contents"][0]["parts"][1]["text"], PAGE_PROMPT);
        assert_eq!(gemini["generationConfig"]["temperature"], 0);

        let openrouter = openrouter_body("vendor/model", "QUJD");
        assert_eq!(openrouter["model"], "vendor/model");
        let content = &openrouter["messages"][0]["content"];
        assert_eq!(content[0]["image_url"]["url"], "data:image/png;base64,QUJD");
        assert_eq!(content[1]["text"], PAGE_PROMPT);
    }

    #[test]
    fn reply_texts_are_extracted_and_empty_replies_are_refused() {
        let gemini = serde_json::json!({"candidates": [{"content": {"parts": [
            {"text": "hel"}, {"text": "lo"}]}}]});
        assert_eq!(gemini_text(&gemini).expect("text"), "hello");
        assert!(gemini_text(&serde_json::json!({"candidates": []})).is_err());
        assert!(
            gemini_text(&serde_json::json!({"candidates": [{"content": {"parts": []}}]})).is_err(),
            "empty text is a refusal, not an answer"
        );

        let openrouter = serde_json::json!({"choices": [{"message": {"content": "reply"}}]});
        assert_eq!(openrouter_text(&openrouter).expect("text"), "reply");
        assert!(openrouter_text(&serde_json::json!({"error": {"message": "x"}})).is_err());
    }

    /// The vocabulary the prompt teaches must stay aligned with the schema
    /// doc; spot-check the labels a reviewer would miss first.
    #[test]
    fn the_prompt_teaches_the_annotation_vocabulary() {
        for label in [
            "cadastral_certificate",
            "photo_montage",
            "elevation_or_section",
            "qr",
        ] {
            assert!(PAGE_PROMPT.contains(label), "prompt must teach {label}");
        }
        assert!(PAGE_PROMPT.contains("ONLY a JSON object"));
    }
}
