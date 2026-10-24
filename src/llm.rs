//! Vision-LLM card reader and detector, behind [`VisionBackend`]:
//! * OpenAI chat-completions: OpenRouter, LM Studio, OpenAI and most others.
//! * Ollama's native `/api/chat`: the only way to reliably switch off
//!   "thinking" (which makes Qwen3-VL ~10x slower).
//! * Anthropic's Messages API (Claude), natively.
//! * Google's Gemini `generateContent`, natively.

use std::io::Cursor;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use image::{DynamicImage, ImageFormat, imageops::FilterType};
use reqwest::blocking::{Client, RequestBuilder};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::{LlmConfig, LlmProvider};
use crate::recognize::CardHints;

/// Long side of a single-card image sent for reading.
const CARD_IMAGE_DIM: u32 = 1024;
/// Long side of a whole-table image sent for card detection.
const SCENE_IMAGE_DIM: u32 = 1920;

const READ_PROMPT: &str = "Identify this Magic: The Gathering card from the photo. \
Read the card's printed name (top left). If — and only if — you can clearly read the tiny \
bottom-left info line, also give the set code (3-5 characters, e.g. DMU) and collector number. \
The \"x/y\" numbers in the bottom-right corner are power/toughness, NOT the collector number. \
Reply with ONLY a JSON object: {\"name\": string, \"set_code\": string|null, \
\"collector_number\": string|null, \"foil\": boolean|null, \"confidence\": number}. \
Use null for anything not clearly legible; never guess the set from the artwork.";

const DETECT_PROMPT: &str = "Locate every Magic: The Gathering playing card in the image whose \
face is fully visible. Ignore cards that are mostly covered by other cards (for example in a pile \
or fanned stack). Output ONLY a JSON array like [{\"bbox_2d\": [x1, y1, x2, y2], \"label\": \
\"card\"}], one entry per card, with coordinates normalized to 0-1000 of the image width and \
height. List each card once.";

/// Generation caps for models that don't think first: enough for any real
/// answer, but a model stuck repeating itself stops quickly instead of
/// running for minutes.
const READ_MAX_TOKENS: u32 = 400;
const DETECT_MAX_TOKENS: u32 = 2048;
/// Claude and Gemini think before answering, and that counts too.
const THINKING_MAX_TOKENS: u32 = 16_000;
/// More boxes than this means the model is looping, not seeing cards.
const MAX_DETECTIONS: usize = 60;

/// The Messages API version this adapter speaks.
const ANTHROPIC_VERSION: &str = "2026-10-21";
/// Lets Claude hand a request it declines to the next model on its own.
const ANTHROPIC_FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NormBox {
    /// Normalised 0.0..=1.0 coordinates.
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
}

/// What a request asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Task {
    Read,
    Detect,
}

/// One question about one image.
struct Ask<'a> {
    task: Task,
    prompt: &'a str,
    /// The image as base64 JPEG.
    jpeg: &'a str,
}

/// One vision model API: how a request looks and where its answer is.
trait VisionBackend: Send {
    /// The request, ready to send but for the API key.
    fn request(&self, http: &Client, ask: &Ask) -> RequestBuilder;
    /// Add the API key.
    fn authorize(&self, req: RequestBuilder, key: &str) -> RequestBuilder {
        req.bearer_auth(key)
    }
    /// The model's text from a successful response.
    fn answer(&self, response: &Value) -> Result<String>;
}

/// OpenAI chat-completions (OpenRouter, LM Studio, OpenAI, ...).
struct OpenAiCompatible {
    url: String,
    model: String,
    reasoning_effort: Option<String>,
    /// OpenRouter shows where requests come from.
    openrouter: bool,
}

impl OpenAiCompatible {
    fn body(&self, ask: &Ask) -> Value {
        let mut body = json!({
            "model": self.model,
            "temperature": 0,
            "max_tokens": local_max_tokens(ask.task),
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": ask.prompt},
                    {"type": "image_url", "image_url": {"url": format!("data:image/jpeg;base64,{}", ask.jpeg)}}
                ]
            }]
        });
        if let Some(effort) = &self.reasoning_effort {
            body["reasoning_effort"] = json!(effort);
        }
        body
    }
}

impl VisionBackend for OpenAiCompatible {
    fn request(&self, http: &Client, ask: &Ask) -> RequestBuilder {
        let req = http.post(&self.url).json(&self.body(ask));
        if self.openrouter {
            req.header(
                "HTTP-Referer",
                "https://github.com/johnflarson/scan-capture-card",
            )
            .header("X-Title", "mtg-scan")
        } else {
            req
        }
    }

    fn answer(&self, response: &Value) -> Result<String> {
        text(response["choices"][0]["message"]["content"].as_str())
    }
}

/// Ollama's native chat API, with thinking off.
struct OllamaNative {
    url: String,
    model: String,
}

impl OllamaNative {
    fn body(&self, ask: &Ask) -> Value {
        json!({
            "model": self.model,
            "stream": false,
            "think": false,
            "options": {"temperature": 0, "num_predict": local_max_tokens(ask.task)},
            "messages": [{"role": "user", "content": ask.prompt, "images": [ask.jpeg]}]
        })
    }
}

impl VisionBackend for OllamaNative {
    fn request(&self, http: &Client, ask: &Ask) -> RequestBuilder {
        http.post(&self.url).json(&self.body(ask))
    }

    fn answer(&self, response: &Value) -> Result<String> {
        text(response["message"]["content"].as_str())
    }
}

/// Anthropic's Messages API (Claude).
struct AnthropicMessages {
    url: String,
    model: String,
}

impl AnthropicMessages {
    fn body(&self, ask: &Ask) -> Value {
        json!({
            "model": self.model,
            "max_tokens": THINKING_MAX_TOKENS,
            // Reading a card is a simple task: think briefly. (Sampling
            // settings such as temperature aren't accepted in this version.)
            "output_config": {"effort": "low"},
            // A declined request is retried on the next model by the API.
            "fallbacks": "default",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "image", "source": {"type": "base64", "media_type": "image/jpeg", "data": ask.jpeg}},
                    {"type": "text", "text": ask.prompt}
                ]
            }]
        })
    }
}

impl VisionBackend for AnthropicMessages {
    fn request(&self, http: &Client, ask: &Ask) -> RequestBuilder {
        http.post(&self.url)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("anthropic-beta", ANTHROPIC_FALLBACK_BETA)
            .json(&self.body(ask))
    }

    fn authorize(&self, req: RequestBuilder, key: &str) -> RequestBuilder {
        req.header("x-api-key", key)
    }

    fn answer(&self, response: &Value) -> Result<String> {
        if response["stop_reason"] == "refusal" {
            bail!(
                "Claude declined to read this image ({})",
                response["stop_details"]["category"]
                    .as_str()
                    .unwrap_or("no reason given")
            );
        }
        // Only the text blocks: thinking and fallback blocks are skipped.
        let joined: String = response["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect();
        text(Some(&joined))
    }
}

/// Google's Gemini `generateContent`.
struct Gemini {
    url: String,
}

impl Gemini {
    fn body(&self, ask: &Ask) -> Value {
        json!({
            "contents": [{
                "role": "user",
                "parts": [
                    {"inline_data": {"mime_type": "image/jpeg", "data": ask.jpeg}},
                    {"text": ask.prompt}
                ]
            }],
            "generationConfig": {
                "temperature": 0,
                "maxOutputTokens": THINKING_MAX_TOKENS,
                "responseMimeType": "application/json"
            }
        })
    }
}

impl VisionBackend for Gemini {
    fn request(&self, http: &Client, ask: &Ask) -> RequestBuilder {
        http.post(&self.url).json(&self.body(ask))
    }

    fn authorize(&self, req: RequestBuilder, key: &str) -> RequestBuilder {
        req.header("x-goog-api-key", key)
    }

    fn answer(&self, response: &Value) -> Result<String> {
        let candidate = &response["candidates"][0];
        if candidate.is_null() {
            bail!(
                "Gemini gave no answer ({})",
                response["promptFeedback"]["blockReason"]
                    .as_str()
                    .unwrap_or("no reason given")
            );
        }
        // Thought summaries are marked `thought`; the answer is the rest.
        let joined: String = candidate["content"]["parts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|p| p["thought"] != true)
            .filter_map(|p| p["text"].as_str())
            .collect();
        text(Some(&joined))
    }
}

fn local_max_tokens(task: Task) -> u32 {
    match task {
        Task::Read => READ_MAX_TOKENS,
        Task::Detect => DETECT_MAX_TOKENS,
    }
}

/// A non-empty answer.
fn text(content: Option<&str>) -> Result<String> {
    content
        .filter(|c| !c.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            anyhow!("LLM response had no answer (a thinking model may have run out of tokens)")
        })
}

/// The API's error message, whatever shape it comes in.
fn error_message(value: &Value) -> String {
    let e = &value["error"];
    e["message"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| e.to_string())
}

pub struct VisionLlm {
    http: Client,
    backend: Box<dyn VisionBackend>,
    model: String,
    /// The server's key (`.env`), if any.
    api_key: Option<String>,
    /// The key of the account being scanned for, used before the server's.
    user_key: Option<String>,
    provider: LlmProvider,
}

#[derive(Debug, Deserialize)]
struct LlmAnswer {
    name: Option<String>,
    set_code: Option<String>,
    collector_number: Option<serde_json::Value>,
    foil: Option<bool>,
    confidence: Option<f32>,
}

impl VisionLlm {
    /// Returns `Ok(None)` when no provider is configured.
    pub fn from_config(cfg: &LlmConfig) -> Result<Option<Self>> {
        let Some(base) = cfg.resolved_base_url() else {
            return Ok(None);
        };
        // A cloud provider without a server key works with each account's
        // own key (Settings); a scan without either fails with a message.
        let api_key = cfg.resolved_api_key();
        let base = base.trim_end_matches('/');
        let model = cfg.resolved_model();
        let backend: Box<dyn VisionBackend> = match cfg.provider {
            LlmProvider::Ollama => Box::new(OllamaNative {
                url: format!("{base}/api/chat"),
                model: model.clone(),
            }),
            LlmProvider::Anthropic => Box::new(AnthropicMessages {
                url: format!("{base}/v1/messages"),
                model: model.clone(),
            }),
            LlmProvider::Gemini => Box::new(Gemini {
                url: format!("{base}/v1beta/models/{model}:generateContent"),
            }),
            _ => Box::new(OpenAiCompatible {
                url: format!("{base}/chat/completions"),
                model: model.clone(),
                reasoning_effort: cfg.resolved_reasoning_effort(),
                openrouter: cfg.provider == LlmProvider::Openrouter,
            }),
        };
        crate::tls::init();
        let http = Client::builder()
            .timeout(Duration::from_secs(cfg.timeout_secs))
            .build()?;
        Ok(Some(Self {
            http,
            backend,
            model,
            api_key,
            user_key: None,
            provider: cfg.provider,
        }))
    }

    pub fn describe(&self) -> String {
        format!("{:?} {}", self.provider, self.model)
    }

    /// A cloud service that needs an API key (not a local model server).
    pub fn needs_key(&self) -> bool {
        self.provider.needs_key()
    }

    /// Use this account's key, before the server's (`None`: the server's).
    pub fn set_user_key(&mut self, key: Option<String>) {
        self.user_key = key;
    }

    /// Read name / set / number from a photo of a single card.
    pub fn read_card(&self, img: &DynamicImage) -> Result<(CardHints, Option<f32>)> {
        let content = self.ask(Task::Read, READ_PROMPT, img, CARD_IMAGE_DIM)?;
        parse_answer(&content)
    }

    /// Find every card in a photo of a table.
    pub fn detect_cards(&self, img: &DynamicImage) -> Result<Vec<NormBox>> {
        let content = self.ask(Task::Detect, DETECT_PROMPT, img, SCENE_IMAGE_DIM)?;
        let aspect = img.width() as f32 / img.height().max(1) as f32;
        Ok(plausible_cards(parse_boxes(&content)?, aspect))
    }

    fn ask(&self, task: Task, prompt: &str, img: &DynamicImage, max_dim: u32) -> Result<String> {
        let jpeg = encode_jpeg(img, max_dim)?;
        let ask = Ask {
            task,
            prompt,
            jpeg: &jpeg,
        };
        let mut req = self.backend.request(&self.http, &ask);
        match self.user_key.as_ref().or(self.api_key.as_ref()) {
            Some(key) => req = self.backend.authorize(req, key),
            None if self.needs_key() => bail!(
                "no {:?} API key: add yours on the Settings page (or the admin sets one for \
                 the server in .env)",
                self.provider
            ),
            None => {}
        }
        let resp = req
            .send()
            .with_context(|| format!("LLM request to {:?} failed", self.provider))?;
        let status = resp.status();
        let value: Value = resp.json().context("LLM returned non-JSON")?;
        if !status.is_success() {
            bail!("LLM error {status}: {}", error_message(&value));
        }
        self.backend.answer(&value)
    }
}

fn encode_jpeg(img: &DynamicImage, max_dim: u32) -> Result<String> {
    let img = if img.width().max(img.height()) > max_dim {
        img.resize(max_dim, max_dim, FilterType::Triangle)
    } else {
        img.clone()
    };
    let mut buf = Cursor::new(Vec::new());
    img.to_rgb8().write_to(&mut buf, ImageFormat::Jpeg)?;
    Ok(base64::engine::general_purpose::STANDARD.encode(buf.into_inner()))
}

/// Slice out the outermost JSON value delimited by `open`/`close` (models
/// often wrap JSON in a ```json fence or add a sentence).
fn json_slice(content: &str, open: char, close: char) -> Result<&str> {
    let start = content
        .find(open)
        .ok_or_else(|| anyhow!("no JSON in LLM reply: {content}"))?;
    let end = content
        .rfind(close)
        .ok_or_else(|| anyhow!("no JSON in LLM reply: {content}"))?;
    if end < start {
        bail!("malformed JSON in LLM reply: {content}");
    }
    Ok(&content[start..=end])
}

fn parse_answer(content: &str) -> Result<(CardHints, Option<f32>)> {
    let answer: LlmAnswer = serde_json::from_str(json_slice(content, '{', '}')?)
        .with_context(|| format!("could not parse LLM reply: {content}"))?;
    let clean = |s: Option<String>| {
        s.map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty() && !v.eq_ignore_ascii_case("null"))
    };
    let number = match answer.collector_number {
        Some(serde_json::Value::String(s)) => Some(s),
        Some(serde_json::Value::Number(n)) => Some(n.to_string()),
        _ => None,
    };
    let hints = CardHints {
        name: clean(answer.name),
        set_code: clean(answer.set_code).map(|s| s.to_lowercase()),
        // "4/5" style answers are power/toughness, not a collector number.
        collector_number: clean(number)
            .filter(|n| !n.contains('/'))
            .map(|n| crate::recognize::normalize_collector_number(&n)),
        foil: answer.foil,
    };
    Ok((hints, answer.confidence))
}

/// Every `"bbox_2d": [x1, y1, x2, y2]` in the reply. Matching entries one by
/// one (rather than parsing the whole array) still recovers the boxes when a
/// reply is cut off mid-JSON.
fn parse_boxes(content: &str) -> Result<Vec<NormBox>> {
    static BOX: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r#""bbox_2d"\s*:\s*\[\s*(-?[\d.]+)\s*,\s*(-?[\d.]+)\s*,\s*(-?[\d.]+)\s*,\s*(-?[\d.]+)\s*\]"#,
        )
        .expect("valid regex")
    });
    let boxes: Vec<NormBox> = BOX
        .captures_iter(content)
        .filter_map(|c| {
            let v: Vec<f32> = (1..=4)
                .map(|i| c[i].parse::<f32>().map(|n| (n / 1000.0).clamp(0.0, 1.0)))
                .collect::<Result<_, _>>()
                .ok()?;
            Some(NormBox {
                x1: v[0].min(v[2]),
                y1: v[1].min(v[3]),
                x2: v[0].max(v[2]),
                y2: v[1].max(v[3]),
            })
        })
        .filter(|b| b.x2 - b.x1 > 0.01 && b.y2 - b.y1 > 0.01)
        .collect();
    if boxes.is_empty() && !content.contains('[') {
        bail!("no detections in LLM reply: {content}");
    }
    Ok(boxes)
}

/// Keep boxes shaped and sized like a card (63x88 mm, upright or sideways)
/// in an image `aspect` wide per unit of height. Drops the slivers a looping
/// model produces, and gives up entirely if it produced an absurd number.
fn plausible_cards(boxes: Vec<NormBox>, aspect: f32) -> Vec<NormBox> {
    if boxes.len() > MAX_DETECTIONS {
        return Vec::new();
    }
    boxes
        .into_iter()
        .filter(|b| {
            let (w, h) = (b.x2 - b.x1, b.y2 - b.y1);
            // Width / height in pixels: about 0.72 upright, 1.4 sideways.
            let shape = w / h * aspect;
            let card_shaped = (0.5..=0.95).contains(&shape) || (1.05..=2.0).contains(&shape);
            card_shaped && (0.002..=0.6).contains(&(w * h))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ASK: Ask = Ask {
        task: Task::Read,
        prompt: "read it",
        jpeg: "AAAA",
    };

    #[test]
    fn anthropic_requests_and_answers() {
        let b = AnthropicMessages {
            url: "https://api.anthropic.com/v1/messages".into(),
            model: "claude-opus-5-5".into(),
        };
        let body = b.body(&ASK);
        assert_eq!(body["model"], "claude-opus-5-5");
        assert_eq!(body["output_config"]["effort"], "low");
        assert_eq!(body["fallbacks"], "default");
        assert!(
            body.get("temperature").is_none(),
            "refused by this API version"
        );
        assert!(body.get("thinking").is_none());
        let image = &body["messages"][0]["content"][0];
        assert_eq!(image["source"]["media_type"], "image/jpeg");
        assert_eq!(image["source"]["data"], "AAAA");
        assert_eq!(body["messages"][0]["content"][1]["text"], "read it");

        let reply = json!({
            "stop_reason": "end_turn",
            "content": [
                {"type": "thinking", "thinking": ""},
                {"type": "text", "text": "{\"name\": \"Opt\"}"}
            ]
        });
        assert_eq!(b.answer(&reply).unwrap(), "{\"name\": \"Opt\"}");
        let refused =
            json!({"stop_reason": "refusal", "stop_details": {"category": "cyber"}, "content": []});
        assert!(
            b.answer(&refused)
                .unwrap_err()
                .to_string()
                .contains("cyber")
        );
    }

    #[test]
    fn gemini_requests_and_answers() {
        let b = Gemini { url: "x".into() };
        let body = b.body(&ASK);
        let parts = &body["contents"][0]["parts"];
        assert_eq!(parts[0]["inline_data"]["mime_type"], "image/jpeg");
        assert_eq!(parts[1]["text"], "read it");
        let reply = json!({"candidates": [{"content": {"parts": [
            {"text": "thinking...", "thought": true},
            {"text": "[]"}
        ]}}]});
        assert_eq!(b.answer(&reply).unwrap(), "[]");
        let blocked = json!({"promptFeedback": {"blockReason": "SAFETY"}});
        assert!(
            b.answer(&blocked)
                .unwrap_err()
                .to_string()
                .contains("SAFETY")
        );
    }

    #[test]
    fn local_models_are_capped_and_answers_read() {
        let o = OllamaNative {
            url: "x".into(),
            model: "qwen3-vl:8b-instruct".into(),
        };
        let body = o.body(&ASK);
        assert_eq!(body["think"], false);
        assert_eq!(body["options"]["num_predict"], READ_MAX_TOKENS);
        assert_eq!(body["messages"][0]["images"][0], "AAAA");
        assert_eq!(
            o.answer(&json!({"message": {"content": "{}"}})).unwrap(),
            "{}"
        );
        assert!(o.answer(&json!({"message": {"content": " "}})).is_err());

        let c = OpenAiCompatible {
            url: "x".into(),
            model: "m".into(),
            reasoning_effort: Some("none".into()),
            openrouter: false,
        };
        let body = c.body(&Ask {
            task: Task::Detect,
            ..ASK
        });
        assert_eq!(body["max_tokens"], DETECT_MAX_TOKENS);
        assert_eq!(body["reasoning_effort"], "none");
        assert_eq!(
            c.answer(&json!({"choices": [{"message": {"content": "[]"}}]}))
                .unwrap(),
            "[]"
        );
    }

    #[test]
    fn error_messages_are_read_from_any_api() {
        assert_eq!(
            error_message(
                &json!({"error": {"type": "authentication_error", "message": "invalid x-api-key"}})
            ),
            "invalid x-api-key"
        );
        assert_eq!(
            error_message(&json!({"error": "model not found"})),
            "\"model not found\""
        );
    }

    #[test]
    fn parses_fenced_json() {
        let reply = "```json\n{\"name\": \"Sheoldred, the Apocalypse\", \"set_code\": \"DMU\", \
                     \"collector_number\": \"0107\", \"foil\": false, \"confidence\": 0.9}\n```";
        let (h, conf) = parse_answer(reply).unwrap();
        assert_eq!(h.name.as_deref(), Some("Sheoldred, the Apocalypse"));
        assert_eq!(h.set_code.as_deref(), Some("dmu"));
        assert_eq!(h.collector_number.as_deref(), Some("107"));
        assert_eq!(h.foil, Some(false));
        assert_eq!(conf, Some(0.9));
    }

    #[test]
    fn rejects_power_toughness_and_accepts_numbers() {
        let (h, _) =
            parse_answer(r#"{"name":"Gandalf","set_code":"ICE","collector_number":"4/5"}"#)
                .unwrap();
        assert_eq!(h.collector_number, None);
        let (h, _) =
            parse_answer(r#"{"name":"Opt","set_code":null,"collector_number":59}"#).unwrap();
        assert_eq!(h.collector_number.as_deref(), Some("59"));
        assert_eq!(h.set_code, None);
    }

    #[test]
    fn recovers_boxes_from_truncated_reply_and_drops_slivers() {
        let reply = "```json\n[{\"bbox_2d\": [145, 159, 245, 372], \"label\": \"card\"},\
                     {\"bbox_2d\": [552, 700, 564, 830], \"label\": \"card\"},\
                     {\"bbox_2d\": [552, 708, 562, 8";
        let boxes = parse_boxes(reply).unwrap();
        assert_eq!(boxes.len(), 2, "the cut-off third entry is ignored");
        // 16:9 frame: the first box is card-shaped, the second a sliver.
        let cards = plausible_cards(boxes, 16.0 / 9.0);
        assert_eq!(
            cards,
            vec![NormBox {
                x1: 0.145,
                y1: 0.159,
                x2: 0.245,
                y2: 0.372
            }]
        );
        let too_many = vec![cards[0]; MAX_DETECTIONS + 1];
        assert!(plausible_cards(too_many, 16.0 / 9.0).is_empty());
    }

    #[test]
    fn parses_detection_boxes() {
        let reply = "```json\n[{\"bbox_2d\": [145, 159, 245, 372], \"label\": \"card\"},\
                     {\"bbox_2d\": [300, 400, 200, 100]}, {\"bbox_2d\": [1, 2]}]\n```";
        let boxes = parse_boxes(reply).unwrap();
        assert_eq!(boxes.len(), 2);
        assert_eq!(
            boxes[0],
            NormBox {
                x1: 0.145,
                y1: 0.159,
                x2: 0.245,
                y2: 0.372
            }
        );
        // Swapped corners are normalised.
        assert_eq!(
            boxes[1],
            NormBox {
                x1: 0.2,
                y1: 0.1,
                x2: 0.3,
                y2: 0.4
            }
        );
    }
}
