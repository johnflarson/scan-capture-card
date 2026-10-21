//! The Settings page (`GET`/`POST /api/settings`, `GET /api/settings/database`).
//! Passwords and API keys are never part of these.

use serde::{Deserialize, Serialize};

use crate::Finish;

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
    /// The file changes are saved to.
    pub file: String,
    /// Why changes can't be saved right now (a dry run).
    pub locked: Option<String>,
    pub connection: Connection,
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
}

/// How to reach the web UI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Connection {
    pub this_computer: String,
    /// The address for other computers and phones, unless the UI only
    /// listens on this computer.
    pub network: Option<String>,
    pub local_only: bool,
    /// Other computers must sign in with the access token.
    pub token_required: bool,
    /// The network address with the token, for opening on a phone. Only
    /// sent to a browser on this computer.
    pub sign_in_link: Option<String>,
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
