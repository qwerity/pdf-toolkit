//! A hosted model proposing the `model` command's parameters.
//!
//! Detection reads the parameters off the title blocks and level marks;
//! where a drawing set prints them differently — another office's title
//! block, levels only drawn, a roof plan without a level in its name — a
//! vision model can read what the deterministic pass could not. It is shown
//! the detected parameters and the drawing sheets, and answers with one
//! `set_params` tool call. **The answer is a proposal**: `pdf-model` checks
//! it against the drawings (the pages must be plan sheets, the levels must
//! stack, the building must come out building-sized) and keeps the detected
//! parameters when it does not hold up. Nothing here draws.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use pdf_classify::PageClass;
use pdf_model::Detection;
use pdf_model::params::{FloorParams, Params};
use pdf_vector::DocumentVector;
use serde_json::{Value, json};

use llm_providers::{Client, Effort, Request, Tool};
use pdf_core::page::open_document;

use crate::DRAWING_RENDER_WIDTH_PX;

/// The most sheets shown: plans, roof, the section, elevations.
const PAGE_BUDGET: usize = 8;
/// How many times the model is asked before a reply with no `set_params`
/// call is a failure.
const ATTEMPTS: usize = 3;

/// A configured connection to one model, bound to a document.
pub struct Proposer {
    client: Client,
    pdf_path: PathBuf,
}

impl Proposer {
    pub fn from_env(model_spec: &str, pdf_path: &Path) -> Result<Self> {
        Ok(Self {
            client: Client::from_env(model_spec)?,
            pdf_path: pdf_path.to_path_buf(),
        })
    }

    /// Every call made, for the report's `llm_usage`.
    pub fn calls(&self) -> Vec<llm_providers::CallUsage> {
        self.client.calls()
    }

    /// The callback `pdf_model::run` takes.
    pub fn callback(&self) -> impl Fn(&Detection, &DocumentVector) -> Result<Params> + '_ {
        move |detection, vector| self.propose(detection, vector)
    }

    /// Renders the drawing sheets, shows them with the detected parameters,
    /// and returns the model's proposal (unchecked).
    pub fn propose(&self, detection: &Detection, vector: &DocumentVector) -> Result<Params> {
        let pages = sheet_pages(vector, &detection.params);
        if pages.is_empty() {
            bail!("no drawing sheets to show the model");
        }
        let pdfium = pdf_core::create_pdfium().context("failed to initialize pdfium runtime")?;
        let document = open_document(&pdfium, &self.pdf_path).map_err(anyhow::Error::from)?;
        let pdf_pages = document.pages();
        let mut request = Request::new(prompt(detection, vector, &pages));
        request.label = "model params".to_string();
        for &number in &pages {
            request.images_png.push(crate::render_page_png(
                pdf_pages,
                number,
                DRAWING_RENDER_WIDTH_PX,
            )?);
        }
        request.max_output_tokens = 16_000;
        request.effort = Some(Effort::High);
        request.tools = vec![set_params_tool()];
        eprintln!("model params: pages {pages:?} -> {}", self.client.model());
        let base_prompt = request.prompt.clone();
        let mut last_text = String::new();
        for attempt in 1..=ATTEMPTS {
            if attempt > 1 {
                // A reply in prose is an answer to the wrong question: ask
                // again, quoting it, insisting on the call.
                request.prompt = format!(
                    "{base_prompt}\n\nYour previous reply made no set_params call; it said: \
                     \"{}\". Reply ONLY by calling set_params once, with every field.",
                    last_text.chars().take(300).collect::<String>()
                );
                eprintln!("model params: attempt {attempt}/{ATTEMPTS}, the reply made no call");
            }
            let reply = self.client.ask(&request).with_context(|| {
                format!("parameter request to '{}' failed", self.client.model())
            })?;
            if let Some(call) = reply.tool_calls.iter().find(|c| c.name == "set_params") {
                return params_from(&call.arguments, &detection.params);
            }
            last_text = reply.text.clone();
        }
        bail!(
            "the model made no set_params call in {ATTEMPTS} attempts{}",
            if last_text.is_empty() {
                String::new()
            } else {
                format!(
                    "; last reply: {}",
                    last_text.chars().take(300).collect::<String>()
                )
            }
        )
    }
}

/// The sheets worth showing: the detected plan and roof pages first, then
/// every other titled drawing (elevations, the section), within the budget.
fn sheet_pages(vector: &DocumentVector, detected: &Params) -> Vec<usize> {
    let mut pages: Vec<usize> = detected.floors.iter().map(|f| f.page).collect();
    pages.extend(detected.roof_page);
    for page in &vector.pages {
        if pages.len() >= PAGE_BUDGET {
            break;
        }
        let drawing = matches!(
            page.classification.class,
            PageClass::FloorPlan
                | PageClass::RoofPlan
                | PageClass::ElevationOrSection
                | PageClass::TechnicalDetail
                | PageClass::Unknown
        ) && page.classification.is_drawing;
        if drawing && !pages.contains(&page.page_number) {
            pages.push(page.page_number);
        }
    }
    pages
}

fn prompt(detection: &Detection, vector: &DocumentVector, pages: &[usize]) -> String {
    let sheets: Vec<String> = pages
        .iter()
        .map(|&n| {
            let page = vector.pages.iter().find(|p| p.page_number == n);
            let title = page
                .and_then(|p| p.classification.features.sheet_title.as_deref())
                .unwrap_or("untitled");
            let class = page
                .map(|p| p.classification.class.as_str())
                .unwrap_or("unknown");
            format!(
                "image {}: page {n} ({class}; title: {title})",
                pages.iter().position(|p| *p == n).unwrap_or(0) + 1
            )
        })
        .collect();
    let detected = serde_json::to_string_pretty(&detection.params).unwrap_or_default();
    format!(
        "You are reading the parameters of a 3D reconstruction from a CAD-exported \
         building-permit drawing set. The geometry is already extracted exactly from \
         the PDF; only these numbers are needed:\n\
         - which pages are the floor plans (bottom storey first) and which is the roof plan;\n\
         - each storey's finished floor level and the level of the storey above (metres);\n\
         - the roof slab level and the parapet top (metres), from the section and facades;\n\
         - the slab thickness and the plinth depth below ±0.000;\n\
         - the plan sheets' scale denominator (75 for 1:75).\n\
         The images are, in order: {}.\n\
         Deterministic detection already read this from the title blocks and level marks:\n{}\n\
         and noted:\n- {}\n\
         Confirm or correct it by calling set_params once. Keep every detected value you \
         cannot improve on. Report levels exactly as printed (a level mark like +3,600 is \
         3.6 m). Page numbers are one-based and must be among the pages shown. Never \
         invent a storey the sheets do not draw.",
        sheets.join("; "),
        detected,
        detection.notes.join("\n- ")
    )
}

fn set_params_tool() -> Tool {
    Tool {
        name: "set_params".into(),
        description: "The reconstruction parameters, confirmed or corrected. Call once.".into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "scale_denominator": {"type": "number", "description": "75 for 1:75"},
                "floors": {"type": "array", "items": {"type": "object", "properties": {
                    "page": {"type": "integer"},
                    "name": {"type": "string"},
                    "level_m": {"type": "number"},
                    "top_m": {"type": "number"}
                }, "required": ["page", "level_m", "top_m"]}},
                "roof_page": {"type": "integer"},
                "roof_level_m": {"type": "number"},
                "parapet_level_m": {"type": "number"},
                "slab_thickness_m": {"type": "number"},
                "plinth_m": {"type": "number"},
                "notes": {"type": "string"}
            },
            "required": ["floors"]
        }),
    }
}

/// The proposal as parameters: the model's numbers over the detected ones,
/// colours and grid labels always the detected ones (they are read off the
/// fills, not something to ask a model).
fn params_from(arguments: &Value, detected: &Params) -> Result<Params> {
    let mut params = detected.clone();
    let number = |key: &str| arguments.get(key).and_then(Value::as_f64);
    if let Some(scale) = number("scale_denominator") {
        params.scale_denominator = scale;
    }
    if let Some(floors) = arguments.get("floors").and_then(Value::as_array) {
        let proposed: Vec<FloorParams> = floors
            .iter()
            .filter_map(|f| {
                Some(FloorParams {
                    page: f.get("page")?.as_u64()? as usize,
                    name: f
                        .get("name")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("Floor at {}", f["level_m"])),
                    level_m: f.get("level_m")?.as_f64()?,
                    top_m: f.get("top_m")?.as_f64()?,
                })
            })
            .collect();
        if proposed.is_empty() {
            bail!("the proposal names no usable floor");
        }
        params.floors = proposed;
    }
    if let Some(page) = arguments.get("roof_page").and_then(Value::as_u64) {
        params.roof_page = Some(page as usize);
    }
    if let Some(v) = number("roof_level_m") {
        params.roof_level_m = v;
    }
    if let Some(v) = number("parapet_level_m") {
        params.parapet_level_m = v;
    }
    if let Some(v) = number("slab_thickness_m") {
        params.slab_thickness_m = v;
    }
    if let Some(v) = number("plinth_m") {
        params.plinth_m = v;
    }
    params.validate()?;
    Ok(params)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detected() -> Params {
        let mut params = Params::archicad_template();
        params.floors = vec![
            FloorParams {
                page: 13,
                name: "ground".into(),
                level_m: 0.0,
                top_m: 3.6,
            },
            FloorParams {
                page: 14,
                name: "first".into(),
                level_m: 3.6,
                top_m: 7.2,
            },
        ];
        params.roof_page = Some(15);
        params
    }

    #[test]
    fn the_proposal_overrides_numbers_and_keeps_colours_and_grid() {
        let params = params_from(
            &json!({
                "floors": [
                    {"page": 13, "level_m": 0.0, "top_m": 3.6, "name": "G"},
                    {"page": 14, "level_m": 3.6, "top_m": 7.2}
                ],
                "parapet_level_m": 7.78, "slab_thickness_m": 0.5,
                "notes": "as printed"
            }),
            &detected(),
        )
        .unwrap();
        assert_eq!(params.floors[0].name, "G");
        assert_eq!(params.floors[1].name, "Floor at 3.6");
        assert_eq!(params.parapet_level_m, 7.78);
        assert_eq!(params.colours, Params::archicad_template().colours);
        assert_eq!(params.axis, Params::archicad_template().axis);
        assert_eq!(params.roof_page, Some(15), "kept from detection");
    }

    #[test]
    fn a_proposal_that_cannot_describe_a_building_is_refused() {
        assert!(params_from(&json!({"floors": []}), &detected()).is_err());
        assert!(
            params_from(
                &json!({"floors": [{"page": 13, "level_m": 5.0, "top_m": 3.0}]}),
                &detected()
            )
            .is_err()
        );
    }
}
