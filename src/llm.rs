//! Vision-LLM card reader and detector.
//!
//! Two wire formats are supported:
//! * OpenAI chat-completions — OpenRouter, LM Studio, OpenAI and most others.
//! * Ollama's native `/api/chat` — used for Ollama because it is the only way
//!   to reliably switch off "thinking" (which makes Qwen3-VL ~10x slower).

use std::io::Cursor;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use image::{DynamicImage, ImageFormat, imageops::FilterType};
use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::json;

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

/// Generation caps: enough for any real answer, but a model stuck repeating
/// itself stops quickly instead of running for minutes.
const READ_MAX_TOKENS: u32 = 400;
const DETECT_MAX_TOKENS: u32 = 2048;
/// More boxes than this means the model is looping, not seeing cards.
const MAX_DETECTIONS: usize = 60;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NormBox {
    /// Normalised 0.0..=1.0 coordinates.
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
}

#[derive(Clone, Copy, PartialEq)]
enum Wire {
    OpenAi,
    OllamaNative,
}

pub struct VisionLlm {
    http: Client,
    wire: Wire,
    url: String,
    model: String,
    /// The server's key (`.env`), if any.
    api_key: Option<String>,
    /// The key of the account being scanned for, used before the server's.
    user_key: Option<String>,
    reasoning_effort: Option<String>,
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
        let (wire, url) = if cfg.provider == LlmProvider::Ollama {
            (Wire::OllamaNative, format!("{base}/api/chat"))
        } else {
            (Wire::OpenAi, format!("{base}/chat/completions"))
        };
        crate::tls::init();
        let http = Client::builder()
            .timeout(Duration::from_secs(cfg.timeout_secs))
            .build()?;
        Ok(Some(Self {
            http,
            wire,
            url,
            model: cfg.resolved_model(),
            api_key,
            user_key: None,
            reasoning_effort: cfg.resolved_reasoning_effort(),
            provider: cfg.provider,
        }))
    }

    pub fn describe(&self) -> String {
        format!("{:?} {}", self.provider, self.model)
    }

    /// A cloud service that needs an API key (not a local model server).
    pub fn needs_key(&self) -> bool {
        matches!(self.provider, LlmProvider::Openrouter | LlmProvider::Openai)
    }

    /// Use this account's key, before the server's (`None`: the server's).
    pub fn set_user_key(&mut self, key: Option<String>) {
        self.user_key = key;
    }

    /// Read name / set / number from a photo of a single card.
    pub fn read_card(&self, img: &DynamicImage) -> Result<(CardHints, Option<f32>)> {
        let content = self.ask(READ_PROMPT, img, CARD_IMAGE_DIM, READ_MAX_TOKENS)?;
        parse_answer(&content)
    }

    /// Find every card in a photo of a table.
    pub fn detect_cards(&self, img: &DynamicImage) -> Result<Vec<NormBox>> {
        let content = self.ask(DETECT_PROMPT, img, SCENE_IMAGE_DIM, DETECT_MAX_TOKENS)?;
        let aspect = img.width() as f32 / img.height().max(1) as f32;
        Ok(plausible_cards(parse_boxes(&content)?, aspect))
    }

    fn ask(
        &self,
        prompt: &str,
        img: &DynamicImage,
        max_dim: u32,
        max_tokens: u32,
    ) -> Result<String> {
        let b64 = encode_jpeg(img, max_dim)?;
        let body = match self.wire {
            Wire::OllamaNative => json!({
                "model": self.model,
                "stream": false,
                "think": false,
                "options": {"temperature": 0, "num_predict": max_tokens},
                "messages": [{"role": "user", "content": prompt, "images": [b64]}]
            }),
            Wire::OpenAi => {
                let mut body = json!({
                    "model": self.model,
                    "temperature": 0,
                    "max_tokens": max_tokens,
                    "messages": [{
                        "role": "user",
                        "content": [
                            {"type": "text", "text": prompt},
                            {"type": "image_url", "image_url": {"url": format!("data:image/jpeg;base64,{b64}")}}
                        ]
                    }]
                });
                if let Some(effort) = &self.reasoning_effort {
                    body["reasoning_effort"] = json!(effort);
                }
                body
            }
        };

        let mut req = self.http.post(&self.url).json(&body);
        match self.user_key.as_ref().or(self.api_key.as_ref()) {
            Some(key) => req = req.bearer_auth(key),
            None if self.needs_key() => bail!(
                "no {:?} API key: add yours on the Settings page (or the admin sets one for \
                 the server in .env)",
                self.provider
            ),
            None => {}
        }
        if self.provider == LlmProvider::Openrouter {
            req = req
                .header(
                    "HTTP-Referer",
                    "https://github.com/johnflarson/scan-capture-card",
                )
                .header("X-Title", "mtg-scan");
        }
        let resp = req
            .send()
            .with_context(|| format!("LLM request to {} failed", self.url))?;
        let status = resp.status();
        let value: serde_json::Value = resp.json().context("LLM returned non-JSON")?;
        if !status.is_success() {
            bail!("LLM error {status}: {}", value["error"]);
        }
        let content = match self.wire {
            Wire::OllamaNative => value["message"]["content"].as_str(),
            Wire::OpenAi => value["choices"][0]["message"]["content"].as_str(),
        };
        content
            .filter(|c| !c.trim().is_empty())
            .map(str::to_string)
            .ok_or_else(|| {
                anyhow!("LLM response had no answer (a thinking model may have run out of tokens)")
            })
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
