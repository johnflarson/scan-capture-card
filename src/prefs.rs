//! Each account's own settings: how its scan session starts, its Moxfield
//! account and hidden decks, the headline currency and the language its
//! cards are printed in. They are saved per account (`user_settings`, one
//! JSON object); whatever an account hasn't set comes from the server's
//! settings file (`[ui]`, `[prices] currency`, `[recognition]
//! card_language`).
//!
//! The first account, which owns a collection from before accounts, also
//! inherits the file's Moxfield account (or `MTG_SCAN_MOXFIELD_USER`) and
//! hidden decks, so an upgraded install looks as it did. Nobody else does:
//! they are someone's own.

use anyhow::{Context, Result};
use mtg_scan_shared::Currency;
use serde::{Deserialize, Serialize};

use crate::config::{Config, UiConfig};
use crate::model::Finish;
use crate::moxfield;

/// What an account has set; `None` means "the server's default".
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Stored {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_scan: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table_mode: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish: Option<Finish>,
    /// A Moxfield user name or profile link as entered; empty for none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub moxfield_user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hidden_decks: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub currency: Option<Currency>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub card_language: Option<String>,
    /// The account's vision-LLM API key, sealed (see `secrets`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_key: Option<String>,
}

impl Stored {
    pub fn parse(json: &str) -> Result<Self> {
        serde_json::from_str(json).context("unreadable account settings")
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }

    /// The first account's starting point: the settings file's Moxfield
    /// account and hidden decks.
    pub fn inherited(cfg: &Config) -> Self {
        Self {
            moxfield_user: moxfield::configured(cfg.decks.moxfield_user.as_deref())
                .map(|(value, _)| value),
            hidden_decks: Some(cfg.decks.hidden.clone()).filter(|h| !h.is_empty()),
            ..Self::default()
        }
    }
}

/// An account's settings with the server's defaults filled in.
#[derive(Debug, Clone, PartialEq)]
pub struct Prefs {
    pub startup: UiConfig,
    /// The Moxfield account as entered (empty for none).
    pub moxfield: String,
    pub hidden_decks: Vec<String>,
    pub currency: Currency,
    pub card_language: String,
    /// The account's vision-LLM API key, sealed.
    pub llm_key: Option<String>,
}

impl Prefs {
    pub fn resolve(stored: &Stored, cfg: &Config) -> Self {
        Self {
            startup: UiConfig {
                auto_scan: stored.auto_scan.unwrap_or(cfg.ui.auto_scan),
                table_mode: stored.table_mode.unwrap_or(cfg.ui.table_mode),
                finish: stored.finish.unwrap_or(cfg.ui.finish),
            },
            moxfield: stored.moxfield_user.clone().unwrap_or_default(),
            hidden_decks: stored.hidden_decks.clone().unwrap_or_default(),
            currency: stored.currency.unwrap_or(cfg.prices.currency),
            card_language: stored
                .card_language
                .clone()
                .unwrap_or_else(|| cfg.recognition.card_language.clone()),
            llm_key: stored.llm_key.clone(),
        }
    }

    /// The Moxfield user name, if one is set and readable.
    pub fn moxfield_user(&self) -> Option<String> {
        moxfield::parse_user(&self.moxfield)
    }

    pub fn is_hidden(&self, deck_id: &str) -> bool {
        self.hidden_decks.iter().any(|h| h == deck_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_values_come_from_the_settings_file() {
        let mut cfg = Config::default();
        cfg.ui.table_mode = true;
        cfg.prices.currency = Currency::Eur;
        cfg.recognition.card_language = "ja".into();
        let stored = Stored {
            auto_scan: Some(false),
            ..Stored::default()
        };
        let prefs = Prefs::resolve(&stored, &cfg);
        assert!(!prefs.startup.auto_scan, "set by the account");
        assert!(prefs.startup.table_mode, "from the file");
        assert_eq!(prefs.currency, Currency::Eur);
        assert_eq!(prefs.card_language, "ja");
        assert_eq!(prefs.moxfield_user(), None);
        assert!(prefs.hidden_decks.is_empty());
    }

    #[test]
    fn the_first_account_inherits_the_files_decks() {
        let mut cfg = Config::default();
        cfg.decks.moxfield_user = Some("https://moxfield.com/users/Someone".into());
        cfg.decks.hidden = vec!["abc".into()];
        let prefs = Prefs::resolve(&Stored::inherited(&cfg), &cfg);
        assert_eq!(prefs.moxfield_user().as_deref(), Some("Someone"));
        assert!(prefs.is_hidden("abc"));
    }

    #[test]
    fn stored_settings_round_trip_and_leave_out_defaults() {
        let stored = Stored {
            finish: Some(Finish::Foil),
            moxfield_user: Some(String::new()),
            ..Stored::default()
        };
        let json = stored.to_json();
        assert_eq!(json, r#"{"finish":"foil","moxfield_user":""}"#);
        assert_eq!(Stored::parse(&json).unwrap(), stored);
        assert_eq!(Stored::parse("{}").unwrap(), Stored::default());
        assert!(Stored::parse("not json").is_err());
    }
}
