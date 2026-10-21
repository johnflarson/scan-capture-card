//! Settings loaded from `mtg-scan.toml` (current directory first, then
//! `%APPDATA%\mtg-scan\config.toml`). Every field has a default, so the file
//! is optional. See `mtg-scan.example.toml`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Markdown file the collection is stored in (markdown backend), and the
    /// default target of `db import` / `db export`.
    pub collection: PathBuf,
    pub storage: StorageConfig,
    pub camera: CameraConfig,
    pub recognition: RecognitionConfig,
    pub llm: LlmConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            collection: PathBuf::from("collection.md"),
            storage: StorageConfig::default(),
            camera: CameraConfig::default(),
            recognition: RecognitionConfig::default(),
            llm: LlmConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum StorageBackend {
    /// `collection.md` (iteration 1).
    Markdown,
    /// PostgreSQL with price history (iteration 2).
    Postgres,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    pub backend: StorageBackend,
    /// Environment variable holding the connection string, e.g.
    /// `postgres://user:pass@host:5432/dbname?sslmode=require`.
    pub database_url_env: String,
    /// Connection string in the config file itself (prefer the env var so
    /// the password stays out of files).
    pub database_url: Option<String>,
    /// Postgres schema the tables live in (created if missing).
    pub schema: String,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            backend: StorageBackend::Markdown,
            database_url_env: "MTG_SCAN_DATABASE_URL".into(),
            database_url: None,
            schema: "mtg_scan".into(),
        }
    }
}

impl StorageConfig {
    /// Connection settings from (in order) the connection-string variable,
    /// `[storage] database_url`, or the separate `MTG_SCAN_DB_*` variables —
    /// normally kept in the app's `.env` file (see `.env.example`).
    pub fn resolved_connection(&self) -> Result<postgres::Config> {
        self.connection_from(|key| std::env::var(key).ok())
    }

    /// Connection for schema changes and role setup: the same
    /// server and database, logged in as `MTG_SCAN_DB_ADMIN_USER` (the
    /// schema owner) when set, otherwise the regular account.
    pub fn resolved_admin_connection(&self) -> Result<postgres::Config> {
        self.admin_connection_from(|key| std::env::var(key).ok())
    }

    fn admin_connection_from(
        &self,
        get: impl Fn(&str) -> Option<String>,
    ) -> Result<postgres::Config> {
        let mut config = self.connection_from(&get)?;
        let get = |key: &str| {
            get(key)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        if let Some(user) = get("MTG_SCAN_DB_ADMIN_USER") {
            config.user(&user);
            match get("MTG_SCAN_DB_ADMIN_PASSWORD") {
                Some(password) => config.password(password),
                None => {
                    bail!("MTG_SCAN_DB_ADMIN_USER is set but MTG_SCAN_DB_ADMIN_PASSWORD is not")
                }
            };
        }
        Ok(config)
    }

    fn connection_from(&self, get: impl Fn(&str) -> Option<String>) -> Result<postgres::Config> {
        let get = |key: &str| {
            get(key)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        if let Some(url) = get(&self.database_url_env).or_else(|| self.database_url.clone()) {
            return url.parse().context("invalid PostgreSQL connection string");
        }
        let Some(host) = get("MTG_SCAN_DB_HOST") else {
            bail!(
                "no PostgreSQL connection configured: set MTG_SCAN_DB_HOST, MTG_SCAN_DB_USER, \
                 MTG_SCAN_DB_PASSWORD (and optionally MTG_SCAN_DB_PORT, MTG_SCAN_DB_NAME, \
                 MTG_SCAN_DB_SSLMODE) in the .env file next to mtg-scan.toml — see .env.example"
            );
        };
        let user = get("MTG_SCAN_DB_USER").context("MTG_SCAN_DB_USER is not set")?;
        let mut config = postgres::Config::new();
        config.host(&host).user(&user).application_name("mtg-scan");
        if let Some(port) = get("MTG_SCAN_DB_PORT") {
            config.port(
                port.parse()
                    .with_context(|| format!("invalid MTG_SCAN_DB_PORT '{port}'"))?,
            );
        }
        if let Some(password) = get("MTG_SCAN_DB_PASSWORD") {
            config.password(password);
        }
        // Postgres defaults the database name to the user name.
        config.dbname(get("MTG_SCAN_DB_NAME").as_deref().unwrap_or(&user));
        if let Some(mode) = get("MTG_SCAN_DB_SSLMODE") {
            config.ssl_mode(match mode.to_lowercase().as_str() {
                "disable" => postgres::config::SslMode::Disable,
                "prefer" => postgres::config::SslMode::Prefer,
                "require" => postgres::config::SslMode::Require,
                other => {
                    bail!("invalid MTG_SCAN_DB_SSLMODE '{other}' (use disable, prefer or require)")
                }
            });
        }
        Ok(config)
    }
}

/// Load `.env` files so settings and secrets can live with the app instead
/// of in Windows environment variables. Real environment variables win, and
/// the first file to set a key wins. Must run before any threads start.
/// Returns the files that were loaded.
pub fn load_env_files() -> Vec<PathBuf> {
    let candidates = [
        Some(PathBuf::from(".env")),
        dirs::config_dir().map(|d| d.join("mtg-scan").join(".env")),
    ];
    let mut loaded = Vec::new();
    for path in candidates.into_iter().flatten().filter(|p| p.is_file()) {
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                for (key, value) in parse_env(&text) {
                    if std::env::var_os(&key).is_none() {
                        // SAFETY: called at the top of `main`, before any
                        // other thread exists.
                        unsafe { std::env::set_var(&key, value) };
                    }
                }
                loaded.push(path);
            }
            Err(e) => eprintln!("  ! could not read {}: {e}", path.display()),
        }
    }
    loaded
}

/// Set (`Some`) or remove (`None`) keys in a `.env` file, keeping every
/// other line (comments, ordering) intact. New keys are appended.
pub fn update_env_file(path: &Path, updates: &[(&str, Option<&str>)]) -> Result<()> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut done = vec![false; updates.len()];
    let mut lines: Vec<String> = Vec::new();
    for line in text.lines() {
        let key = line
            .trim()
            .strip_prefix("export ")
            .unwrap_or(line.trim())
            .split_once('=')
            .map(|(k, _)| k.trim());
        match key.and_then(|k| updates.iter().position(|(u, _)| *u == k)) {
            Some(i) => {
                if let (false, Some(value)) = (done[i], updates[i].1) {
                    lines.push(format!("{}={value}", updates[i].0));
                }
                done[i] = true;
            }
            None => lines.push(line.to_string()),
        }
    }
    for (i, (key, value)) in updates.iter().enumerate() {
        if let (false, Some(value)) = (done[i], value) {
            lines.push(format!("{key}={value}"));
        }
    }
    let tmp = path.with_extension("env.tmp");
    std::fs::write(&tmp, lines.join("\n") + "\n")
        .with_context(|| format!("failed to write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("failed to update {}", path.display()))
}

/// Minimal `.env` parser. Values are literal: everything after the first `=`
/// (trimmed), with one pair of matching surrounding quotes removed. No `$`
/// substitution, escapes or inline comments, so passwords containing `$`,
/// `#` or `\` work as typed. Lines starting with `#` are comments.
fn parse_env(text: &str) -> Vec<(String, String)> {
    text.lines()
        .map(|l| l.trim_start_matches('\u{feff}').trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let l = l.strip_prefix("export ").unwrap_or(l);
            let (key, value) = l.split_once('=')?;
            let key = key.trim();
            if key.is_empty() {
                return None;
            }
            let value = value.trim();
            let unquoted = ['\'', '"']
                .iter()
                .find_map(|q| value.strip_prefix(*q)?.strip_suffix(*q))
                .unwrap_or(value);
            Some((key.to_string(), unquoted.to_string()))
        })
        .collect()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CameraConfig {
    /// Camera index from `mtg-scan cameras`; default picks the Insta360.
    pub index: Option<u32>,
    /// Capture size in single-card mode.
    pub width: u32,
    pub height: u32,
    /// Capture size in table mode (every card in view): as large as possible.
    pub table_width: u32,
    pub table_height: u32,
    /// Card guide height as a fraction of the frame height.
    pub guide_height: f32,
}

impl Default for CameraConfig {
    fn default() -> Self {
        Self {
            index: None,
            width: 1920,
            height: 1080,
            table_width: 3840,
            table_height: 2160,
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
        assert_eq!(cfg.storage.backend, StorageBackend::Markdown);
        assert_eq!(cfg.storage.schema, "mtg_scan");
    }

    #[test]
    fn parses_postgres_storage() {
        let cfg: Config = toml::from_str(
            r#"
            [storage]
            backend = "postgres"
            database_url = "postgres://u:p@localhost/db"
            database_url_env = "MTG_SCAN_TEST_UNSET_VAR"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.storage.backend, StorageBackend::Postgres);
        let conn = cfg.storage.connection_from(|_| None).unwrap();
        assert_eq!(conn.get_user(), Some("u"));
        assert_eq!(conn.get_dbname(), Some("db"));
    }

    #[test]
    fn builds_connection_from_separate_variables() {
        let env = |key: &str| {
            match key {
                "MTG_SCAN_DB_HOST" => Some("10.0.0.5"),
                "MTG_SCAN_DB_PORT" => Some("5433"),
                "MTG_SCAN_DB_USER" => Some("svc"),
                // Characters that would need URL-encoding are fine here.
                "MTG_SCAN_DB_PASSWORD" => Some("p@ss:w/rd#1"),
                "MTG_SCAN_DB_SSLMODE" => Some("disable"),
                _ => None,
            }
            .map(String::from)
        };
        let conn = StorageConfig::default().connection_from(env).unwrap();
        assert_eq!(conn.get_ports(), &[5433]);
        assert_eq!(conn.get_user(), Some("svc"));
        assert_eq!(conn.get_password(), Some(&b"p@ss:w/rd#1"[..]));
        assert_eq!(
            conn.get_dbname(),
            Some("svc"),
            "database defaults to the user name"
        );
        assert_eq!(conn.get_ssl_mode(), postgres::config::SslMode::Disable);
    }

    #[test]
    fn env_values_are_literal() {
        let parsed = parse_env(
            "# comment\n\
             MTG_SCAN_DB_PASSWORD=ab$cd#ef\\g\r\n\
             QUOTED='with spaces $x'\n\
             DOUBLE=\"d$q\"\n\
             export EXPORTED = v \n\
             EMPTY=\n\
             EQUALS=a=b\n\
             not a pair\n",
        );
        let get = |k: &str| {
            parsed
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(get("MTG_SCAN_DB_PASSWORD"), Some("ab$cd#ef\\g"));
        assert_eq!(get("QUOTED"), Some("with spaces $x"));
        assert_eq!(get("DOUBLE"), Some("d$q"));
        assert_eq!(get("EXPORTED"), Some("v"));
        assert_eq!(get("EMPTY"), Some(""));
        assert_eq!(get("EQUALS"), Some("a=b"));
        assert_eq!(parsed.len(), 6);
    }

    #[test]
    fn admin_connection_overrides_only_the_login() {
        let env = |key: &str| {
            match key {
                "MTG_SCAN_DB_HOST" => Some("db"),
                "MTG_SCAN_DB_USER" => Some("app"),
                "MTG_SCAN_DB_PASSWORD" => Some("app-pw"),
                "MTG_SCAN_DB_NAME" => Some("cards"),
                "MTG_SCAN_DB_ADMIN_USER" => Some("owner"),
                "MTG_SCAN_DB_ADMIN_PASSWORD" => Some("owner-pw"),
                _ => None,
            }
            .map(String::from)
        };
        let admin = StorageConfig::default().admin_connection_from(env).unwrap();
        assert_eq!(admin.get_user(), Some("owner"));
        assert_eq!(admin.get_password(), Some(&b"owner-pw"[..]));
        assert_eq!(admin.get_dbname(), Some("cards"));
        assert_eq!(admin.get_hosts().len(), 1);
    }

    #[test]
    fn update_env_file_replaces_appends_and_removes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        std::fs::write(&path, "# keep me\nA=1\nB=old$\nC=gone\n").unwrap();
        update_env_file(
            &path,
            &[("B", Some("new$#")), ("C", None), ("D", Some("4"))],
        )
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, "# keep me\nA=1\nB=new$#\nD=4\n");
        let parsed = parse_env(&text);
        assert!(parsed.contains(&("B".into(), "new$#".into())));
    }

    #[test]
    fn missing_connection_settings_explain_the_env_file() {
        let err = StorageConfig::default()
            .connection_from(|_| None)
            .unwrap_err();
        assert!(err.to_string().contains(".env"));
    }
}
