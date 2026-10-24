//! The Settings page (`GET`/`POST /api/settings`, `GET /api/settings/database`).
//! Passwords and API keys are never part of these.

use serde::{Deserialize, Serialize};

use crate::{Currency, Finish};

/// Card languages a collection can be scanned in: Scryfall's language codes
/// with their names. Scans pick the printing in this language when Scryfall
/// has one.
pub const CARD_LANGUAGES: [(&str, &str); 11] = [
    ("en", "English"),
    ("de", "German"),
    ("fr", "French"),
    ("it", "Italian"),
    ("es", "Spanish"),
    ("pt", "Portuguese"),
    ("ja", "Japanese"),
    ("ko", "Korean"),
    ("ru", "Russian"),
    ("zhs", "Simplified Chinese"),
    ("zht", "Traditional Chinese"),
];

/// Prices: the headline currency and the optional Card Kingdom source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceSettings {
    pub currency: Currency,
    pub card_kingdom: bool,
}

/// How `mtg-scan ui` starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Startup {
    pub auto_scan: bool,
    pub table_mode: bool,
    pub finish: Finish,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingsView {
    pub startup: Startup,
    /// Command-line flags that overrode a startup setting for this run, e.g.
    /// "--manual: auto-scan started off".
    pub overridden: Vec<String>,
    /// The Moxfield link or user name as entered (empty when none).
    pub moxfield: String,
    /// The user name it resolves to.
    pub moxfield_user: Option<String>,
    /// Where `moxfield` comes from: the settings file or `.env`.
    pub moxfield_source: Option<String>,
    /// Moxfield deck IDs hidden from the Decks page and deck statistics.
    pub hidden_decks: Vec<String>,
    pub prices: PriceSettings,
    /// Scryfall language code of the cards being scanned, e.g. `en`, `ja`.
    pub card_language: String,
    /// The file changes are saved to.
    pub file: String,
    /// Why changes can't be saved right now (a dry run).
    pub locked: Option<String>,
    pub connection: Connection,
    #[serde(default)]
    pub llm: LlmView,
}

/// The vision model scans ask when OCR can't read a card, and whose API
/// key pays for it. The key itself is never sent back.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmView {
    /// e.g. "Openrouter google/gemini-2.5-flash"; `None` without a model.
    pub model: Option<String>,
    /// A cloud service that needs an API key (not a local model).
    pub needs_key: bool,
    /// The server has a key of its own (`.env`), used when an account has none.
    pub server_key: bool,
    /// This account saved a key.
    pub key_set: bool,
    /// Why keys can't be saved on this server, if they can't.
    pub cannot_save: Option<String>,
}

/// One change from the Settings page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SettingsChange {
    Startup(Startup),
    /// A Moxfield profile link or user name; empty for none.
    Moxfield {
        user: String,
    },
    DeckHidden {
        id: String,
        hidden: bool,
    },
    Prices(PriceSettings),
    /// A code from [`CARD_LANGUAGES`].
    CardLanguage {
        code: String,
    },
    /// The account's own API key for the vision model; empty removes it.
    LlmKey {
        key: String,
    },
}

/// How to reach the web UI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Connection {
    pub this_computer: String,
    /// The address for other computers and phones, unless the UI only
    /// listens on this computer.
    pub network: Option<String>,
    pub local_only: bool,
}

/// A label and a value, e.g. ("Host", "db.example.com").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InfoRow {
    pub label: String,
    pub value: String,
}

impl InfoRow {
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
        }
    }
}

/// Where the collection is stored and how it's doing. Read-only: the
/// connection settings live in `.env`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DatabaseView {
    /// "PostgreSQL" or "Markdown file".
    pub backend: String,
    /// Connection settings (no password).
    pub settings: Vec<InfoRow>,
    /// Live figures: server version, schema version, row counts.
    pub status: Vec<InfoRow>,
    /// Why the status couldn't be read.
    pub error: Option<String>,
}
