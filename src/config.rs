//! Settings loaded from `mtg-scan.toml` (current directory first, then
//! `%APPDATA%\mtg-scan\config.toml`). Every field has a default, so the file
//! is optional. See `mtg-scan.example.toml`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Markdown file the collection is stored in.
    pub collection: PathBuf,
    pub camera: CameraConfig,
    pub recognition: RecognitionConfig,
    pub llm: LlmConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            collection: PathBuf::from("collection.md"),
            camera: CameraConfig::default(),
            recognition: RecognitionConfig::default(),
            llm: LlmConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CameraConfig {
    /// Camera index from `mtg-scan cameras`; default picks the Insta360.
    pub index: Option<u32>,
    pub width: u32,
    pub height: u32,
    /// Card guide height as a fraction of the frame height.
    pub guide_height: f32,
}

impl Default for CameraConfig {
    fn default() -> Self {
        Self {
            index: None,
            width: 1920,
            height: 1080,
            guide_height: 0.85,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum RecognitionMode {
    /// Windows OCR only (offline, fast).
    Ocr,
    /// Vision LLM only (OpenRouter or a local model).
    Llm,
    /// OCR first; ask the LLM when OCR can't pin down the exact printing.
    Auto,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RecognitionConfig {
    pub mode: RecognitionMode,
    /// Save each captured card image into this folder (for debugging).
    pub save_scans: Option<PathBuf>,
}

impl Default for RecognitionConfig {
    fn default() -> Self {
        Self {
            mode: RecognitionMode::Auto,
            save_scans: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum LlmProvider {
    /// No LLM; `auto` mode then behaves like `ocr`.
    None,
    /// https://openrouter.ai (needs OPENROUTER_API_KEY).
    Openrouter,
    /// Local Ollama server (http://localhost:11434).
    Ollama,
    /// Local LM Studio server (http://localhost:1234).
    Lmstudio,
    /// Any other OpenAI-compatible endpoint; set `base_url`.
    Openai,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LlmConfig {
    pub provider: LlmProvider,
    /// Model id; provider-specific default when unset.
    pub model: Option<String>,
    /// Override the provider's base URL. OpenAI-style providers include the
    /// `/v1` suffix; Ollama is the server root (http://host:11434).
    pub base_url: Option<String>,
    /// Name of the environment variable holding the API key.
    pub api_key_env: Option<String>,
    pub timeout_secs: u64,
    /// Sent as `reasoning_effort` to OpenAI-style providers. Thinking models
    /// are far slower and gain little for reading a card, so LM Studio
    /// defaults to "none". (Ollama always gets `think: false`.)
    pub reasoning_effort: Option<String>,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            provider: LlmProvider::None,
            model: None,
            base_url: None,
            api_key_env: None,
            timeout_secs: 120,
            reasoning_effort: None,
        }
    }
}

impl LlmConfig {
    pub fn resolved_base_url(&self) -> Option<String> {
        self.base_url.clone().or_else(|| {
            Some(
                match self.provider {
                    LlmProvider::Openrouter => "https://openrouter.ai/api/v1",
                    LlmProvider::Ollama => "http://localhost:11434",
                    LlmProvider::Lmstudio => "http://localhost:1234/v1",
                    LlmProvider::Openai => "https://api.openai.com/v1",
                    LlmProvider::None => return None,
                }
                .to_string(),
            )
        })
    }

    pub fn resolved_model(&self) -> String {
        self.model.clone().unwrap_or_else(|| {
            match self.provider {
                LlmProvider::Openrouter => "google/gemini-2.5-flash",
                LlmProvider::Ollama => "qwen3-vl:8b-instruct",
                LlmProvider::Lmstudio => "qwen/qwen3-vl-8b",
                LlmProvider::Openai => "gpt-5-mini",
                LlmProvider::None => "",
            }
            .to_string()
        })
    }

    pub fn resolved_api_key(&self) -> Option<String> {
        let var = self.api_key_env.clone().or_else(|| match self.provider {
            LlmProvider::Openrouter => Some("OPENROUTER_API_KEY".into()),
            LlmProvider::Openai => Some("OPENAI_API_KEY".into()),
            _ => None,
        })?;
        std::env::var(var).ok().filter(|k| !k.trim().is_empty())
    }

    pub fn resolved_reasoning_effort(&self) -> Option<String> {
        self.reasoning_effort
            .clone()
            .or_else(|| match self.provider {
                LlmProvider::Lmstudio => Some("none".into()),
                _ => None,
            })
    }
}

impl Config {
    pub fn load(explicit: Option<&Path>) -> Result<Self> {
        let candidates: Vec<PathBuf> = match explicit {
            Some(p) => vec![p.to_path_buf()],
            None => [
                Some(PathBuf::from("mtg-scan.toml")),
                dirs::config_dir().map(|d| d.join("mtg-scan").join("config.toml")),
            ]
            .into_iter()
            .flatten()
            .collect(),
        };
        for path in candidates {
            if path.exists() {
                let text = std::fs::read_to_string(&path)
                    .with_context(|| format!("failed to read {}", path.display()))?;
                return toml::from_str(&text)
                    .with_context(|| format!("invalid config in {}", path.display()));
            }
        }
        if let Some(p) = explicit {
            anyhow::bail!("config file {} not found", p.display());
        }
        Ok(Self::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_partial_config() {
        let cfg: Config = toml::from_str(
            r#"
            collection = "cards.md"
            [llm]
            provider = "ollama"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.collection, PathBuf::from("cards.md"));
        assert_eq!(cfg.llm.provider, LlmProvider::Ollama);
        assert_eq!(cfg.llm.resolved_model(), "qwen3-vl:8b-instruct");
        assert_eq!(
            cfg.llm.resolved_base_url().as_deref(),
            Some("http://localhost:11434")
        );
        assert_eq!(cfg.recognition.mode, RecognitionMode::Auto);
        assert_eq!(cfg.camera.width, 1920);
    }
}
