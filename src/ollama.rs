//! What a local Ollama server can do: whether it answers, which models it
//! has, whether a model reads images (`/api/show` capabilities), how much of
//! a loaded model sits on the GPU (`/api/ps`), and downloading a model
//! (`/api/pull`, with progress). `mtg-scan llm check` / `llm pull` and the
//! setup wizard use it.

use std::io::{BufRead, BufReader};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::json;

/// A model on the server.
#[derive(Debug, Clone, PartialEq)]
pub struct Installed {
    pub name: String,
    /// Bytes on disk.
    pub size: u64,
}

/// What `/api/show` says about a model.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ModelInfo {
    /// e.g. `completion`, `vision`, `thinking`.
    pub capabilities: Vec<String>,
    /// e.g. "8.8B".
    pub parameter_size: Option<String>,
    /// e.g. "Q4_K_M".
    pub quantization: Option<String>,
}

impl ModelInfo {
    pub fn vision(&self) -> bool {
        self.capabilities.iter().any(|c| c == "vision")
    }
}

/// A model loaded in memory (`/api/ps`).
#[derive(Debug, Clone, PartialEq)]
pub struct Loaded {
    pub name: String,
    pub size: u64,
    pub size_vram: u64,
}

impl Loaded {
    /// The share of the model on the GPU (1.0: all of it). Less makes
    /// every scan slow.
    pub fn on_gpu(&self) -> f64 {
        if self.size == 0 {
            0.0
        } else {
            self.size_vram as f64 / self.size as f64
        }
    }
}

/// One line of `/api/pull` progress.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct PullProgress {
    #[serde(default)]
    pub status: String,
    pub total: Option<u64>,
    pub completed: Option<u64>,
    pub error: Option<String>,
}

pub struct Ollama {
    http: Client,
    base: String,
}

impl Ollama {
    /// The server at `base` (e.g. `http://localhost:11434`).
    pub fn new(base: &str) -> Result<Self> {
        crate::tls::init();
        Ok(Self {
            http: Client::builder().timeout(Duration::from_secs(30)).build()?,
            base: base.trim_end_matches('/').to_string(),
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// The server's version, if it answers.
    pub fn version(&self) -> Result<String> {
        let v: serde_json::Value = self
            .http
            .get(self.url("/api/version"))
            .send()
            .with_context(|| format!("Ollama doesn't answer at {}", self.base))?
            .error_for_status()?
            .json()?;
        Ok(v["version"].as_str().unwrap_or("?").to_string())
    }

    pub fn installed(&self) -> Result<Vec<Installed>> {
        let v: serde_json::Value = self
            .http
            .get(self.url("/api/tags"))
            .send()?
            .error_for_status()?
            .json()?;
        Ok(parse_tags(&v))
    }

    /// `None` when the server doesn't have the model.
    pub fn show(&self, model: &str) -> Result<Option<ModelInfo>> {
        let resp = self
            .http
            .post(self.url("/api/show"))
            .json(&json!({ "model": model }))
            .send()?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let v: serde_json::Value = resp.error_for_status()?.json()?;
        Ok(Some(parse_show(&v)))
    }

    pub fn loaded(&self) -> Result<Vec<Loaded>> {
        let v: serde_json::Value = self
            .http
            .get(self.url("/api/ps"))
            .send()?
            .error_for_status()?
            .json()?;
        Ok(parse_ps(&v))
    }

    /// Download `model`, calling `progress` for each update.
    pub fn pull(&self, model: &str, mut progress: impl FnMut(&PullProgress)) -> Result<()> {
        // A large model takes a long time: no overall timeout.
        let http = Client::builder().build()?;
        let resp = http
            .post(self.url("/api/pull"))
            .json(&json!({ "model": model, "stream": true }))
            .send()
            .with_context(|| format!("Ollama doesn't answer at {}", self.base))?
            .error_for_status()?;
        for line in BufReader::new(resp).lines() {
            let line = line.context("the download was interrupted")?;
            if line.trim().is_empty() {
                continue;
            }
            let p: PullProgress = serde_json::from_str(&line)
                .with_context(|| format!("unexpected answer from Ollama: {line}"))?;
            if let Some(e) = &p.error {
                bail!("Ollama couldn't download {model}: {e}");
            }
            progress(&p);
            if p.status == "success" {
                return Ok(());
            }
        }
        bail!("the download of {model} ended before it finished")
    }
}

/// Whether two names mean the same model (`qwen3-vl` is `qwen3-vl:latest`).
pub fn same_model(a: &str, b: &str) -> bool {
    let full = |n: &str| {
        if n.contains(':') {
            n.to_string()
        } else {
            format!("{n}:latest")
        }
    };
    full(a) == full(b)
}

fn parse_tags(v: &serde_json::Value) -> Vec<Installed> {
    v["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            Some(Installed {
                name: m["name"].as_str()?.to_string(),
                size: m["size"].as_u64().unwrap_or(0),
            })
        })
        .collect()
}

fn parse_show(v: &serde_json::Value) -> ModelInfo {
    let text = |s: &serde_json::Value| s.as_str().map(str::to_string);
    ModelInfo {
        capabilities: v["capabilities"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| c.as_str().map(str::to_string))
            .collect(),
        parameter_size: text(&v["details"]["parameter_size"]),
        quantization: text(&v["details"]["quantization_level"]),
    }
}

fn parse_ps(v: &serde_json::Value) -> Vec<Loaded> {
    v["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            Some(Loaded {
                name: m["name"].as_str()?.to_string(),
                size: m["size"].as_u64().unwrap_or(0),
                size_vram: m["size_vram"].as_u64().unwrap_or(0),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_tags_show_and_ps() {
        let tags = json!({"models": [
            {"name": "qwen3-vl:8b-instruct", "size": 6_100_000_000u64, "details": {}},
            {"name": "llama3:latest", "size": 4_700_000_000u64}
        ]});
        let installed = parse_tags(&tags);
        assert_eq!(installed.len(), 2);
        assert_eq!(installed[0].name, "qwen3-vl:8b-instruct");

        let show = json!({
            "capabilities": ["completion", "vision"],
            "details": {"parameter_size": "8.8B", "quantization_level": "Q4_K_M"}
        });
        let info = parse_show(&show);
        assert!(info.vision());
        assert_eq!(info.parameter_size.as_deref(), Some("8.8B"));
        assert!(!parse_show(&json!({"capabilities": ["completion"]})).vision());

        let ps =
            json!({"models": [{"name": "qwen3-vl:8b-instruct", "size": 8000, "size_vram": 6000}]});
        let loaded = parse_ps(&ps);
        assert_eq!(loaded[0].on_gpu(), 0.75);
    }

    #[test]
    fn model_names_match_with_or_without_latest() {
        assert!(same_model("qwen3-vl", "qwen3-vl:latest"));
        assert!(same_model("qwen3-vl:8b-instruct", "qwen3-vl:8b-instruct"));
        assert!(!same_model("qwen3-vl:8b", "qwen3-vl:8b-instruct"));
    }

    #[test]
    fn pull_progress_lines_parse() {
        let p: PullProgress = serde_json::from_str(
            r#"{"status":"pulling 8eeb52dfb3bb","digest":"sha256:8eeb","total":4683075271,"completed":1048576}"#,
        )
        .unwrap();
        assert_eq!(
            (p.total, p.completed),
            (Some(4_683_075_271), Some(1_048_576))
        );
        let e: PullProgress =
            serde_json::from_str(r#"{"error":"pull model manifest: file does not exist"}"#)
                .unwrap();
        assert!(e.error.is_some());
    }
}
