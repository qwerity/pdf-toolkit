//! Hosted-model transport for the toolkit's opt-in networked commands.
//!
//! One [`Client`] over three APIs. The model spec's shape picks the provider,
//! so a caller names a model and nothing else:
//!
//! - `gemini-*` — Google's Gemini API (`GEMINI_API_KEY`, with
//!   `GOOGLE_API_KEY` accepted as a fallback)
//! - `gpt-*` — the OpenAI API (`OPENAI_API_KEY`), including
//!   [`ASTRA_MODEL`]
//! - `vendor/model` — OpenRouter (`OPENROUTER_API_KEY`)
//!
//! Every request can carry page images and tool definitions on every
//! provider, and a reply then holds the model's tool calls — issued in
//! parallel, so one request over several pages comes back as one call per
//! finding instead of one round-trip each.
//!
//! API keys travel in headers, never in URLs, so they cannot leak through a
//! proxy log or a malformed-URL error message. A provider's error body is
//! surfaced in the failure, because "429" without Gemini's quota message is
//! not actionable.
//!
//! Every reply's token usage is logged to stderr with an estimated cost,
//! priced from the bundled [`models.json`](../models.json) catalog — so a
//! run's spend is visible call by call without any provider dashboard.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use serde::Serialize;
use serde_json::{Value, json};

/// OpenAI's GPT-6 Astra: the strongest model available here for reading
/// construction drawings and carrying out multi-step tool work, at roughly
/// ten times the price of the default `gemini-3.8-flash`.
pub const ASTRA_MODEL: &str = "gpt-6-astra";

/// A reconstruction over seven sheets can think for forty thousand tokens
/// before its first tool call; five minutes cut such replies off.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(600);

/// Which API a model spec is served by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    /// Google's Generative Language API.
    Gemini,
    /// The OpenAI API.
    OpenAi,
    /// OpenRouter's OpenAI-compatible API.
    OpenRouter,
}

impl Provider {
    /// The provider a model spec's shape names.
    ///
    /// A `vendor/model` id is OpenRouter's namespace; `gpt-*` is OpenAI's;
    /// anything else goes to Gemini, which is where the bare `gemini-*` ids
    /// live.
    pub fn for_spec(spec: &str) -> Self {
        if spec.contains('/') {
            Provider::OpenRouter
        } else if spec.starts_with("gpt-") {
            Provider::OpenAi
        } else {
            Provider::Gemini
        }
    }

    /// The provider's name as reports print it.
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::Gemini => "gemini",
            Provider::OpenAi => "openai",
            Provider::OpenRouter => "openrouter",
        }
    }

    /// The environment variable holding this provider's key, plus an
    /// accepted fallback name.
    fn key_lookup(self) -> (&'static str, Option<&'static str>) {
        match self {
            Provider::Gemini => ("GEMINI_API_KEY", Some("GOOGLE_API_KEY")),
            Provider::OpenAi => ("OPENAI_API_KEY", None),
            Provider::OpenRouter => ("OPENROUTER_API_KEY", None),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Provider::Gemini => "Gemini",
            Provider::OpenAi => "OpenAI",
            Provider::OpenRouter => "OpenRouter",
        }
    }
}

/// How hard a reasoning model should think.
///
/// GPT-6 Astra accepts the full range; Gemini folds it to its `low` / `high`
/// thinking levels; OpenRouter models ignore it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effort {
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl Effort {
    /// Gemini 3 takes a thinking level rather than a budget; `low` and
    /// `high` are the two every model in the family accepts.
    fn gemini_level(self) -> &'static str {
        match self {
            Effort::Low => "low",
            _ => "high",
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::XHigh => "xhigh",
            Effort::Max => "max",
        }
    }
}

/// One callable function offered to the model, in the OpenAI function shape.
#[derive(Debug, Clone)]
pub struct Tool {
    pub name: String,
    pub description: String,
    /// JSON Schema of the arguments object.
    pub parameters: Value,
}

/// One prompt, any number of page images, and optionally the tools the model
/// may call.
#[derive(Debug, Clone)]
pub struct Request {
    /// What this call is for, as the usage report names it (`building`,
    /// `vlm page 7`). Empty when the caller did not say.
    pub label: String,
    pub prompt: String,
    /// PNG-encoded images, sent before the prompt in page order.
    pub images_png: Vec<Vec<u8>>,
    pub max_output_tokens: u32,
    /// Reasoning effort, for providers that take one.
    pub effort: Option<Effort>,
    pub tools: Vec<Tool>,
}

impl Request {
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            label: String::new(),
            prompt: prompt.into(),
            images_png: Vec::new(),
            max_output_tokens: 4096,
            effort: None,
            tools: Vec::new(),
        }
    }
}

/// One function call the model issued. Calls arrive in the order the model
/// made them; a model calling in parallel returns several per reply.
#[derive(Debug, Clone)]
pub struct ToolCall {
    pub name: String,
    /// The parsed arguments object. When a model emits arguments that are not
    /// valid JSON, the raw text is carried as a string so one malformed call
    /// does not cost the caller the rest of the reply.
    pub arguments: Value,
}

/// Token usage one reply reported. `input_tokens` includes the cached
/// share, the way every provider reports it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    /// Includes reasoning/thinking tokens — providers bill them as output.
    pub output_tokens: u64,
}

/// USD per million tokens for one model, from `models.json`.
#[derive(Debug, Clone, Copy)]
pub struct ModelPrice {
    pub input: f64,
    pub output: f64,
    /// Cache-read rate; cache writes are not modelled.
    pub cached_input: f64,
}

impl Usage {
    /// Estimated USD cost of this reply at the given rates.
    pub fn cost(&self, price: &ModelPrice) -> f64 {
        let fresh = self.input_tokens.saturating_sub(self.cached_input_tokens);
        (fresh as f64 * price.input
            + self.cached_input_tokens as f64 * price.cached_input
            + self.output_tokens as f64 * price.output)
            / 1e6
    }
}

/// The bundled price catalog. Every model the toolkit has used is listed;
/// update `models.json` when a provider changes its rates (the file records
/// where each number came from and when).
pub fn model_price(model: &str) -> Option<ModelPrice> {
    static CATALOG: OnceLock<HashMap<String, ModelPrice>> = OnceLock::new();
    CATALOG
        .get_or_init(|| {
            let raw: Value = serde_json::from_str(include_str!("../models.json"))
                .expect("models.json is valid JSON");
            raw["models"]
                .as_object()
                .expect("models.json has a models table")
                .iter()
                .map(|(id, price)| {
                    (
                        id.clone(),
                        ModelPrice {
                            input: price["input"].as_f64().unwrap_or_default(),
                            output: price["output"].as_f64().unwrap_or_default(),
                            cached_input: price["cached_input"].as_f64().unwrap_or_default(),
                        },
                    )
                })
                .collect()
        })
        .get(model)
        .copied()
}

/// One hosted call's token usage and estimated cost, as every networked
/// command reports it (`llm_usage[]` in `docs/json-api.md`).
#[derive(Debug, Clone, Serialize)]
pub struct CallUsage {
    /// What the call was for, from [`Request::label`].
    pub label: String,
    pub model: String,
    /// `gemini`, `openai` or `openrouter`.
    pub provider: &'static str,
    /// Whether the provider reported token counts; the counts are zero when
    /// it did not, and `cost_usd` is absent.
    pub usage_reported: bool,
    /// Prompt tokens, the cached share included.
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    /// Reasoning tokens included, as providers bill them.
    pub output_tokens: u64,
    /// Estimated from `models.json` rates. Absent when the model is not
    /// catalogued or the provider reported no usage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    pub elapsed_ms: u64,
}

/// What the model answered: prose, tool calls, or both.
#[derive(Debug, Clone, Default)]
pub struct Reply {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    /// Token usage the provider reported, when it reported any.
    pub usage: Option<Usage>,
}

/// A configured connection to one hosted model.
pub struct Client {
    provider: Provider,
    model: String,
    api_key: String,
    agent: ureq::Agent,
    /// Every call made through this client, for the report.
    calls: std::sync::Mutex<Vec<CallUsage>>,
}

impl Client {
    /// Builds a client from a model spec, reading the key from the
    /// environment (a `.env` in the working directory is honoured).
    pub fn from_env(model_spec: &str) -> Result<Self> {
        // Keys live in the environment; a developer's .env is a convenience,
        // never a requirement, and absence of the file is not an error.
        let _ = dotenvy::dotenv();

        let provider = Provider::for_spec(model_spec);
        let (variable, fallback) = provider.key_lookup();
        let api_key = std::env::var(variable)
            .or_else(|_| {
                fallback
                    .map(std::env::var)
                    .unwrap_or(Err(std::env::VarError::NotPresent))
            })
            .ok()
            .filter(|key| !key.is_empty())
            .ok_or_else(|| {
                anyhow!("model '{model_spec}' needs {variable} in the environment (or .env)")
            })?;

        // Statuses are read manually so a provider's error body reaches the
        // failure message instead of being dropped with the response.
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(REQUEST_TIMEOUT))
            .http_status_as_error(false)
            .build();
        Ok(Self {
            provider,
            model: model_spec.to_string(),
            api_key,
            agent: config.into(),
            calls: std::sync::Mutex::new(Vec::new()),
        })
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn provider(&self) -> Provider {
        self.provider
    }

    /// Sends one request and returns the model's reply. The reply's token
    /// usage and estimated cost are logged to stderr (`llm: …`) and kept
    /// for [`Client::calls`].
    pub fn ask(&self, request: &Request) -> Result<Reply> {
        let started = std::time::Instant::now();
        let reply = self.send(request)?;
        log_usage(&self.model, reply.usage.as_ref());
        let usage = reply.usage.unwrap_or_default();
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(CallUsage {
                label: request.label.clone(),
                model: self.model.clone(),
                provider: self.provider.as_str(),
                usage_reported: reply.usage.is_some(),
                input_tokens: usage.input_tokens,
                cached_input_tokens: usage.cached_input_tokens,
                output_tokens: usage.output_tokens,
                cost_usd: reply
                    .usage
                    .and_then(|u| model_price(&self.model).map(|price| round6(u.cost(&price)))),
                elapsed_ms: started.elapsed().as_millis() as u64,
            });
        Ok(reply)
    }

    /// Every call this client has made, in order — what a command's
    /// `llm_usage` reports.
    pub fn calls(&self) -> Vec<CallUsage> {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn send(&self, request: &Request) -> Result<Reply> {
        match self.provider {
            Provider::Gemini => {
                let url = format!(
                    "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent",
                    self.model
                );
                let reply = self.post(&url, "x-goog-api-key", &gemini_body(request))?;
                gemini_reply(&reply)
            }
            Provider::OpenAi => {
                let reply = self.post(
                    "https://api.openai.com/v1/responses",
                    "Authorization",
                    &responses_body(&self.model, request),
                )?;
                responses_reply(&reply)
            }
            Provider::OpenRouter => {
                let reply = self.post(
                    "https://openrouter.ai/api/v1/chat/completions",
                    "Authorization",
                    &chat_body(&self.model, request),
                )?;
                chat_reply(&reply)
            }
        }
    }

    fn post(&self, url: &str, key_header: &str, body: &Value) -> Result<Value> {
        let key_value = if key_header == "Authorization" {
            format!("Bearer {}", self.api_key)
        } else {
            self.api_key.clone()
        };
        // One bounded retry on throttling/overload: a 503 during a long
        // multi-image request is routine and rarely repeats.
        for attempt in 0..2 {
            let mut response = self
                .agent
                .post(url)
                .header(key_header, &key_value)
                .send_json(body)
                .with_context(|| format!("{} request failed", self.provider.name()))?;
            let status = response.status();
            let text = response
                .body_mut()
                .read_to_string()
                .with_context(|| format!("{} response unreadable", self.provider.name()))?;
            if status.is_success() {
                return serde_json::from_str(&text)
                    .with_context(|| format!("{} reply is not JSON", self.provider.name()));
            }
            let retryable = matches!(status.as_u16(), 429 | 500 | 502 | 503 | 529);
            if retryable && attempt == 0 {
                eprintln!(
                    "llm: {} replied {status}; retrying in 15s",
                    self.provider.name()
                );
                std::thread::sleep(Duration::from_secs(15));
                continue;
            }
            bail!(
                "{} replied {status}: {}",
                self.provider.name(),
                error_detail(&text)
            );
        }
        unreachable!("the retry loop always returns or bails")
    }
}

/// The message a provider's error body carries, or a truncated raw body when
/// there is none. All three providers use `{"error": {"message": ...}}`.
fn error_detail(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| body.chars().take(300).collect())
}

/// The Gemini `generateContent` request.
fn gemini_body(request: &Request) -> Value {
    let mut parts: Vec<Value> = request
        .images_png
        .iter()
        .map(|png| {
            json!({"inline_data": {
                "mime_type": "image/png",
                "data": base64::engine::general_purpose::STANDARD.encode(png),
            }})
        })
        .collect();
    parts.push(json!({"text": request.prompt}));
    let mut body = json!({
        "contents": [{"parts": parts}],
        "generationConfig": {"temperature": 0, "maxOutputTokens": request.max_output_tokens},
    });
    if let Some(effort) = request.effort {
        body["generationConfig"]["thinkingConfig"] =
            json!({ "thinkingLevel": effort.gemini_level() });
    }
    if !request.tools.is_empty() {
        let declarations: Vec<Value> = request
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.parameters,
                })
            })
            .collect();
        body["tools"] = json!([{"functionDeclarations": declarations}]);
        body["toolConfig"] = json!({"functionCallingConfig": {"mode": "AUTO"}});
    }
    body
}

/// The text and function calls of a Gemini reply, in the order the model
/// produced them. Thought parts are not content and are skipped.
fn gemini_reply(reply: &Value) -> Result<Reply> {
    let parts = reply["candidates"][0]["content"]["parts"]
        .as_array()
        .ok_or_else(|| anyhow!("no content parts in Gemini reply: {reply}"))?;
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    for part in parts {
        if part["thought"].as_bool().unwrap_or(false) {
            continue;
        }
        if let Some(chunk) = part["text"].as_str() {
            text.push_str(chunk);
        }
        if let Some(call) = part.get("functionCall")
            && let Some(name) = call["name"].as_str()
        {
            tool_calls.push(ToolCall {
                name: name.to_string(),
                arguments: call.get("args").cloned().unwrap_or(Value::Null),
            });
        }
    }
    if text.is_empty() && tool_calls.is_empty() {
        bail!("no text or function call in Gemini reply: {reply}");
    }
    Ok(Reply {
        text,
        tool_calls,
        usage: gemini_usage(reply),
    })
}

/// Gemini's `usageMetadata`. Thinking tokens are billed as output, so they
/// are folded into `output_tokens`.
fn gemini_usage(reply: &Value) -> Option<Usage> {
    let meta = reply.get("usageMetadata")?;
    Some(Usage {
        input_tokens: meta["promptTokenCount"].as_u64().unwrap_or_default(),
        cached_input_tokens: meta["cachedContentTokenCount"].as_u64().unwrap_or_default(),
        output_tokens: meta["candidatesTokenCount"].as_u64().unwrap_or_default()
            + meta["thoughtsTokenCount"].as_u64().unwrap_or_default(),
    })
}

/// The OpenAI Responses API request. Function tools alongside reasoning
/// live on `/v1/responses` — Chat Completions refuses the combination for
/// GPT-6 Astra — so the OpenAI dialect uses Responses for everything.
fn responses_body(model: &str, request: &Request) -> Value {
    let mut content: Vec<Value> = request
        .images_png
        .iter()
        .map(|png| {
            json!({"type": "input_image", "image_url": format!(
                "data:image/png;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(png)
            )})
        })
        .collect();
    content.push(json!({"type": "input_text", "text": request.prompt}));

    let mut body = json!({
        "model": model,
        "input": [{"role": "user", "content": content}],
        "max_output_tokens": request.max_output_tokens,
    });
    if let Some(effort) = request.effort {
        body["reasoning"] = json!({"effort": effort.as_str()});
    }
    if !request.tools.is_empty() {
        body["tools"] = request
            .tools
            .iter()
            .map(|tool| {
                json!({"type": "function",
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.parameters,
                })
            })
            .collect();
        body["tool_choice"] = "auto".into();
        body["parallel_tool_calls"] = true.into();
    }
    body
}

/// The output items of a Responses API reply: message text and
/// function calls, in the order the model produced them.
fn responses_reply(reply: &Value) -> Result<Reply> {
    let output = reply["output"]
        .as_array()
        .ok_or_else(|| anyhow!("no output in Responses reply: {reply}"))?;
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    for item in output {
        match item["type"].as_str() {
            Some("message") => {
                if let Some(parts) = item["content"].as_array() {
                    text.extend(parts.iter().filter_map(|p| p["text"].as_str()));
                }
            }
            Some("function_call") => {
                let Some(name) = item["name"].as_str() else {
                    continue;
                };
                let raw = item["arguments"].as_str().unwrap_or_default();
                let arguments =
                    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string()));
                tool_calls.push(ToolCall {
                    name: name.to_string(),
                    arguments,
                });
            }
            _ => {}
        }
    }
    if text.is_empty() && tool_calls.is_empty() {
        bail!("neither content nor tool calls in Responses reply: {reply}");
    }
    let usage = reply.get("usage").map(|u| Usage {
        input_tokens: u["input_tokens"].as_u64().unwrap_or_default(),
        cached_input_tokens: u["input_tokens_details"]["cached_tokens"]
            .as_u64()
            .unwrap_or_default(),
        output_tokens: u["output_tokens"].as_u64().unwrap_or_default(),
    });
    Ok(Reply {
        text,
        tool_calls,
        usage,
    })
}

/// The OpenAI-compatible chat request OpenRouter takes.
fn chat_body(model: &str, request: &Request) -> Value {
    let mut content: Vec<Value> = request
        .images_png
        .iter()
        .map(|png| {
            json!({"type": "image_url", "image_url": {"url": format!(
                "data:image/png;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(png)
            )}})
        })
        .collect();
    content.push(json!({"type": "text", "text": request.prompt}));

    let mut body = json!({
        "model": model,
        "messages": [{"role": "user", "content": content}],
        "max_tokens": request.max_output_tokens,
        "temperature": 0,
    });
    if !request.tools.is_empty() {
        body["tools"] = request
            .tools
            .iter()
            .map(|tool| {
                json!({"type": "function", "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.parameters,
                }})
            })
            .collect();
        body["tool_choice"] = "auto".into();
        body["parallel_tool_calls"] = true.into();
    }
    body
}

/// The message content and tool calls of an OpenAI-format reply.
fn chat_reply(reply: &Value) -> Result<Reply> {
    let message = &reply["choices"][0]["message"];
    let text = message["content"].as_str().unwrap_or_default().to_string();
    let tool_calls: Vec<ToolCall> = message["tool_calls"]
        .as_array()
        .map(|calls| {
            calls
                .iter()
                .filter_map(|call| {
                    let name = call["function"]["name"].as_str()?.to_string();
                    let raw = call["function"]["arguments"].as_str().unwrap_or_default();
                    let arguments = serde_json::from_str(raw)
                        .unwrap_or_else(|_| Value::String(raw.to_string()));
                    Some(ToolCall { name, arguments })
                })
                .collect()
        })
        .unwrap_or_default();
    if text.is_empty() && tool_calls.is_empty() {
        bail!("neither content nor tool calls in reply: {reply}");
    }
    let usage = reply.get("usage").map(|u| Usage {
        input_tokens: u["prompt_tokens"].as_u64().unwrap_or_default(),
        cached_input_tokens: u["prompt_tokens_details"]["cached_tokens"]
            .as_u64()
            .unwrap_or_default(),
        output_tokens: u["completion_tokens"].as_u64().unwrap_or_default(),
    });
    Ok(Reply {
        text,
        tool_calls,
        usage,
    })
}

fn round6(v: f64) -> f64 {
    (v * 1e6).round() / 1e6
}

/// One stderr line per call: tokens and, where the catalog prices the
/// model, the estimated spend. Stdout stays reserved for JSON contracts.
fn log_usage(model: &str, usage: Option<&Usage>) {
    match usage {
        Some(u) => match model_price(model) {
            Some(price) => eprintln!(
                "llm: {model}: {} in ({} cached) + {} out \u{2248} ${:.4}",
                u.input_tokens,
                u.cached_input_tokens,
                u.output_tokens,
                u.cost(&price)
            ),
            None => eprintln!(
                "llm: {model}: {} in ({} cached) + {} out (not in models.json; cost unknown)",
                u.input_tokens, u.cached_input_tokens, u.output_tokens
            ),
        },
        None => eprintln!("llm: {model}: provider reported no token usage"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_model_spec_shape_picks_the_provider() {
        assert_eq!(Provider::for_spec("gemini-3.8-flash"), Provider::Gemini);
        assert_eq!(Provider::for_spec(ASTRA_MODEL), Provider::OpenAi);
        assert_eq!(Provider::for_spec("gpt-4o"), Provider::OpenAi);
        assert_eq!(
            Provider::for_spec("qwen/qwen3-vl-8b-instruct"),
            Provider::OpenRouter
        );
        // An unknown bare name goes to Gemini, whose error then names the
        // key variable — the shape rule stays total rather than guessing.
        assert_eq!(Provider::for_spec("no-such-model"), Provider::Gemini);
    }

    #[test]
    fn a_missing_or_empty_key_is_an_error_naming_the_variable() {
        for (spec, variable) in [
            ("no-such-model-name", "GEMINI_API_KEY"),
            ("gpt-no-such-model", "OPENAI_API_KEY"),
            ("vendor/no-such-model", "OPENROUTER_API_KEY"),
        ] {
            match Client::from_env(spec) {
                Ok(client) => assert!(
                    !client.api_key.is_empty(),
                    "a key from the environment is fine too"
                ),
                Err(error) => assert!(
                    error.to_string().contains(variable),
                    "error must name {variable}: {error}"
                ),
            }
        }
    }

    #[test]
    fn request_bodies_carry_the_images_and_the_prompt() {
        let mut request = Request::new("annotate this");
        request.images_png.push(b"ABC".to_vec());

        let gemini = gemini_body(&request);
        assert_eq!(
            gemini["contents"][0]["parts"][0]["inline_data"]["data"],
            "QUJD"
        );
        assert_eq!(gemini["contents"][0]["parts"][1]["text"], "annotate this");
        assert_eq!(gemini["generationConfig"]["temperature"], 0);

        let router = chat_body("vendor/model", &request);
        assert_eq!(router["model"], "vendor/model");
        let content = &router["messages"][0]["content"];
        assert_eq!(content[0]["image_url"]["url"], "data:image/png;base64,QUJD");
        assert_eq!(content[1]["text"], "annotate this");
        assert_eq!(router["max_tokens"], 4096);
        assert_eq!(router["temperature"], 0);
    }

    #[test]
    fn the_openai_dialect_is_the_responses_api_with_reasoning_and_flat_tools() {
        let mut request = Request::new("fix this drawing");
        request.images_png.push(b"ABC".to_vec());
        request.effort = Some(Effort::High);
        request.tools.push(Tool {
            name: "set_layer".into(),
            description: "assign features to a layer".into(),
            parameters: json!({"type": "object"}),
        });

        let body = responses_body(ASTRA_MODEL, &request);
        assert_eq!(body["model"], ASTRA_MODEL);
        assert_eq!(body["max_output_tokens"], 4096);
        assert_eq!(body["reasoning"]["effort"], "high");
        let content = &body["input"][0]["content"];
        assert_eq!(content[0]["type"], "input_image");
        assert_eq!(content[0]["image_url"], "data:image/png;base64,QUJD");
        assert_eq!(
            content[1]["input_text"].as_str(),
            None,
            "text key is 'text'"
        );
        assert_eq!(content[1]["text"], "fix this drawing");
        assert!(
            body.get("temperature").is_none(),
            "reasoning models refuse a pinned temperature"
        );
        // Responses tools are flat, not nested under "function" — Chat
        // Completions refuses function tools with reasoning for Astra.
        assert_eq!(body["tools"][0]["name"], "set_layer");
        assert_eq!(body["parallel_tool_calls"], true);
    }

    #[test]
    fn a_responses_reply_yields_text_and_parallel_function_calls_in_order() {
        let reply = json!({"output": [
            {"type": "reasoning", "summary": []},
            {"type": "message", "content": [{"type": "output_text", "text": "done"}]},
            {"type": "function_call", "name": "set_layer",
             "arguments": "{\"layer\": \"walls\", \"ids\": [1]}"},
            {"type": "function_call", "name": "drop_features",
             "arguments": "not json"},
        ]});
        let parsed = responses_reply(&reply).expect("parses");
        assert_eq!(parsed.text, "done");
        assert_eq!(parsed.tool_calls.len(), 2);
        assert_eq!(parsed.tool_calls[0].arguments["layer"], "walls");
        assert_eq!(
            parsed.tool_calls[1].arguments,
            Value::String("not json".into()),
            "a malformed call is carried raw, not fatal"
        );
        assert!(responses_reply(&json!({"output": []})).is_err());
    }

    #[test]
    fn replies_are_extracted_and_empty_replies_are_refused() {
        let gemini = json!({"candidates": [{"content": {"parts": [
            {"text": "hel"}, {"text": "lo"}]}}]});
        assert_eq!(gemini_reply(&gemini).expect("text").text, "hello");
        assert!(gemini_reply(&json!({"candidates": []})).is_err());
        assert!(
            gemini_reply(&json!({"candidates": [{"content": {"parts": []}}]})).is_err(),
            "empty text is a refusal, not an answer"
        );

        let chat = json!({"choices": [{"message": {"content": "reply"}}]});
        assert_eq!(chat_reply(&chat).expect("text").text, "reply");
        assert!(chat_reply(&json!({"error": {"message": "x"}})).is_err());
    }

    #[test]
    fn parallel_tool_calls_come_back_in_order_and_survive_bad_arguments() {
        let reply = json!({"choices": [{"message": {
            "content": null,
            "tool_calls": [
                {"function": {"name": "set_layer",
                              "arguments": "{\"layer\": \"walls\", \"ids\": [1, 2]}"}},
                {"function": {"name": "drop_features",
                              "arguments": "not json at all"}},
            ],
        }}]});
        let parsed = chat_reply(&reply).expect("tool calls");
        assert_eq!(parsed.text, "");
        assert_eq!(parsed.tool_calls.len(), 2);
        assert_eq!(parsed.tool_calls[0].name, "set_layer");
        assert_eq!(parsed.tool_calls[0].arguments["layer"], "walls");
        assert_eq!(
            parsed.tool_calls[1].arguments,
            Value::String("not json at all".into()),
            "a malformed call is carried raw, not fatal"
        );
    }

    #[test]
    fn the_gemini_dialect_declares_functions_and_returns_parallel_calls() {
        let mut request = Request::new("reconstruct");
        request.effort = Some(Effort::High);
        request.tools.push(Tool {
            name: "add_part".into(),
            description: "declare a part".into(),
            parameters: json!({"type": "object", "properties": {"name": {"type": "string"}}}),
        });
        let body = gemini_body(&request);
        assert_eq!(
            body["tools"][0]["functionDeclarations"][0]["name"],
            "add_part"
        );
        assert_eq!(body["toolConfig"]["functionCallingConfig"]["mode"], "AUTO");
        assert_eq!(
            body["generationConfig"]["thinkingConfig"]["thinkingLevel"],
            "high"
        );
        assert!(
            gemini_body(&Request::new("plain")).get("tools").is_none(),
            "no tools, no declarations"
        );

        let reply = json!({"candidates": [{"content": {"parts": [
            {"thought": true, "text": "planning…"},
            {"functionCall": {"name": "add_part", "args": {"name": "house A"}}},
            {"functionCall": {"name": "add_part", "args": {"name": "house B"}}},
            {"functionCall": {"name": "set_facts", "args": {"floors": 2}}},
            {"text": "done"},
        ]}}]});
        let parsed = gemini_reply(&reply).expect("parses");
        assert_eq!(parsed.text, "done", "thought parts are not content");
        assert_eq!(parsed.tool_calls.len(), 3);
        assert_eq!(parsed.tool_calls[1].arguments["name"], "house B");
        assert_eq!(parsed.tool_calls[2].name, "set_facts");

        let calls_only = json!({"candidates": [{"content": {"parts": [
            {"functionCall": {"name": "set_facts", "args": {}}}]}}]});
        assert!(
            gemini_reply(&calls_only).is_ok(),
            "a call with no prose is a reply"
        );
        let empty =
            json!({"candidates": [{"content": {"parts": [{"thought": true, "text": "…"}]}}]});
        assert!(gemini_reply(&empty).is_err());
    }

    #[test]
    fn the_catalog_prices_every_model_the_toolkit_has_used() {
        for model in [
            ASTRA_MODEL,
            "gemini-3.8-flash",
            "gemini-pro-latest",
            "openai/gpt-4o",
            "anthropic/claude-sonnet-4.6",
            "qwen/qwen3-vl-8b-instruct",
        ] {
            let price = model_price(model).unwrap_or_else(|| panic!("{model} missing"));
            assert!(price.input > 0.0 && price.output > price.input);
        }
        assert!(model_price("made-up-model").is_none());
    }

    #[test]
    fn cost_bills_cached_input_at_the_cache_rate() {
        let price = ModelPrice {
            input: 10.0,
            output: 50.0,
            cached_input: 1.0,
        };
        let usage = Usage {
            input_tokens: 1_000_000,
            cached_input_tokens: 400_000,
            output_tokens: 100_000,
        };
        // 600k fresh at $10 + 400k cached at $1 + 100k out at $50.
        assert!((usage.cost(&price) - (6.0 + 0.4 + 5.0)).abs() < 1e-9);
    }

    #[test]
    fn each_dialect_reports_its_usage() {
        let gemini = json!({
            "candidates": [{"content": {"parts": [{"text": "ok"}]}}],
            "usageMetadata": {"promptTokenCount": 1200, "candidatesTokenCount": 300,
                              "thoughtsTokenCount": 50, "cachedContentTokenCount": 200},
        });
        assert_eq!(
            gemini_reply(&gemini).expect("parses").usage,
            Some(Usage {
                input_tokens: 1200,
                cached_input_tokens: 200,
                output_tokens: 350
            }),
            "thinking tokens bill as output"
        );

        let responses = json!({
            "output": [{"type": "message", "content": [{"type": "output_text", "text": "ok"}]}],
            "usage": {"input_tokens": 3400, "output_tokens": 900,
                      "input_tokens_details": {"cached_tokens": 100}},
        });
        assert_eq!(
            responses_reply(&responses).expect("parses").usage,
            Some(Usage {
                input_tokens: 3400,
                cached_input_tokens: 100,
                output_tokens: 900
            })
        );

        let chat = json!({
            "choices": [{"message": {"content": "ok"}}],
            "usage": {"prompt_tokens": 800, "completion_tokens": 120},
        });
        assert_eq!(
            chat_reply(&chat).expect("parses").usage,
            Some(Usage {
                input_tokens: 800,
                cached_input_tokens: 0,
                output_tokens: 120
            })
        );
    }

    #[test]
    fn provider_error_bodies_reach_the_failure_message() {
        assert_eq!(
            error_detail(r#"{"error": {"message": "quota exceeded for tier"}}"#),
            "quota exceeded for tier"
        );
        assert_eq!(
            error_detail("<html>gateway timeout</html>"),
            "<html>gateway timeout</html>"
        );
    }
}
