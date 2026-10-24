//! The setup wizard (`/api/setup`, admins): how cards are recognised, a
//! local model's download and a quick test, the telemetry choice, and
//! finishing. Prices and the card language go through the Settings API.

use serde::{Deserialize, Serialize};

/// How cards are read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecognitionChoice {
    /// The printed text only: works anywhere, offline.
    Ocr,
    /// The printed text, then a vision model on this server (Ollama).
    Local,
    /// The printed text, then a cloud vision model with an API key.
    Cloud,
}

/// A cloud vision service the wizard offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudProvider {
    /// As in the settings file: `anthropic`, `gemini`, `openrouter`, `openai`.
    pub id: String,
    pub label: String,
    /// The model used when none is given.
    pub default_model: String,
    /// Where to get a key.
    pub key_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecognitionView {
    pub choice: RecognitionChoice,
    /// As in the settings file, e.g. `ollama`, `anthropic`; `none` for OCR only.
    pub provider: String,
    pub model: String,
    /// Where the model server is (Ollama: e.g. `http://localhost:11434`).
    pub base_url: Option<String>,
    /// The OCR engine, e.g. "PaddleOCR".
    pub ocr: String,
    /// The signed-in admin saved a key of their own.
    pub own_key: bool,
    /// The server has a key for this provider (`.env`).
    pub server_key: bool,
}

/// `GET /api/setup`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetupView {
    pub done: bool,
    /// "SQLite" or "PostgreSQL".
    pub storage: String,
    /// The SQLite file or the PostgreSQL server (no password).
    pub storage_location: String,
    pub recognition: RecognitionView,
    pub cloud_providers: Vec<CloudProvider>,
    /// The model the wizard suggests for Ollama.
    pub local_model: String,
    /// `None` until answered.
    pub telemetry: Option<bool>,
}

/// A model on the Ollama server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OllamaModelView {
    pub name: String,
    pub size_gb: f64,
}

/// `GET /api/setup/ollama?url=`: what an Ollama server has.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OllamaView {
    pub url: String,
    /// `None` when it doesn't answer (see `error`).
    pub version: Option<String>,
    pub error: Option<String>,
    pub models: Vec<OllamaModelView>,
    /// The suggested model is downloaded and reads images.
    pub ready: bool,
}

/// `POST /api/setup/recognition`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecognitionSetup {
    pub choice: RecognitionChoice,
    /// Cloud: the provider's id; ignored otherwise.
    #[serde(default)]
    pub provider: String,
    /// Empty: the provider's default.
    #[serde(default)]
    pub model: String,
    /// Local: the Ollama server.
    #[serde(default)]
    pub base_url: String,
}

/// `GET`/`POST /api/setup/pull`: an Ollama download.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PullView {
    pub model: String,
    pub status: String,
    /// 0-100 while bytes are counted.
    pub percent: Option<f64>,
    pub running: bool,
    pub done: bool,
    pub error: Option<String>,
}

/// `POST /api/setup/benchmark`: one card read with the chosen model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BenchmarkView {
    pub seconds: f64,
    /// The card it was shown.
    pub expected: String,
    /// What it read (`None` if it failed; see `error`).
    pub read: Option<String>,
    pub correct: bool,
    /// Ollama: the share of the model on the GPU after the read.
    pub gpu_share: Option<f64>,
    /// In words: fast, fine, slow.
    pub verdict: String,
    pub error: Option<String>,
}
