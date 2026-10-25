//! Anonymous usage statistics: the daily ping a server sends when its admin
//! allowed it (`[telemetry] enabled = true`), and what the Settings page shows
//! of it (`GET /api/setup/telemetry`). The collector (`collector/`) reads the
//! same type, so both sides agree on every field.
//!
//! Only what is listed here is ever sent: no cards, collections, decks,
//! images, names, addresses or keys. Counts are rounded into ranges.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The ping's format; the collector refuses others.
pub const SCHEMA: u32 = 1;

/// Kinds of errors counted, in the order the metrics page shows them. Only
/// the kind is sent, never the message.
pub const ERROR_KINDS: [&str; 8] = [
    "camera",
    "vision_model",
    "ocr",
    "scryfall",
    "prices",
    "storage",
    "moxfield",
    "other",
];

/// The ranges counts are rounded into.
pub const COUNT_RANGES: [&str; 5] = ["0", "1-9", "10-99", "100-999", "1000+"];

/// The ranges the number of accounts is rounded into.
pub const ACCOUNT_RANGES: [&str; 4] = ["1", "2-5", "6-20", "21+"];

/// One server's day. A field that isn't listed here is refused, so nothing
/// else can slip in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ping {
    pub schema: u32,
    /// Random, made on this server the first time; it only tells pings of
    /// one server apart (delete `telemetry.json` for a new one).
    pub install_id: String,
    /// mtg-scan's version, e.g. `1.0.0-alpha.1`.
    pub version: String,
    /// `windows`, `linux` or `macos`.
    pub os: String,
    /// `x86_64`, `aarch64`, ...
    pub arch: String,
    /// How it was installed: `docker`, `windows-release` or `source`.
    pub install: String,
    /// `sqlite` or `postgres`.
    pub storage: String,
    /// `[recognition] mode`: `auto`, `ocr` or `llm`.
    pub recognition: String,
    /// The vision model's provider (`ollama`, `anthropic`, ...); `None` when
    /// cards are read from the printed text only.
    pub llm_provider: Option<String>,
    /// `[recognition] ocr`: `auto`, `windows` or `paddle`.
    pub ocr: String,
    pub card_kingdom: bool,
    /// The setup wizard was finished.
    pub setup_done: bool,
    /// Accounts with a password, as a range of [`ACCOUNT_RANGES`].
    pub accounts: String,
    /// Cards recognised since the last ping, as a range of [`COUNT_RANGES`].
    pub scans: String,
    /// Cards that weren't recognised since the last ping, as a range.
    pub unrecognised: String,
    /// Errors since the last ping by kind ([`ERROR_KINDS`]), as ranges.
    pub errors: BTreeMap<String, String>,
}

/// `GET /api/setup/telemetry` (admins): the choice and the next ping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TelemetryView {
    /// `[telemetry] enabled`; `None` until answered.
    pub enabled: Option<bool>,
    /// `DO_NOT_TRACK` is set, so nothing is sent whatever `enabled` says.
    pub do_not_track: bool,
    /// Where pings go.
    pub url: String,
    /// The day (UTC, `YYYY-MM-DD`) the last ping was received.
    pub last_sent: Option<String>,
    /// Exactly what the next ping will send.
    pub next: Ping,
}

/// `n` rounded into one of [`COUNT_RANGES`].
pub fn count_range(n: u64) -> &'static str {
    match n {
        0 => COUNT_RANGES[0],
        1..=9 => COUNT_RANGES[1],
        10..=99 => COUNT_RANGES[2],
        100..=999 => COUNT_RANGES[3],
        _ => COUNT_RANGES[4],
    }
}

/// The number of accounts rounded into one of [`ACCOUNT_RANGES`] (none
/// counts as one: the admin's is being set up).
pub fn account_range(n: u64) -> &'static str {
    match n {
        0 | 1 => ACCOUNT_RANGES[0],
        2..=5 => ACCOUNT_RANGES[1],
        6..=20 => ACCOUNT_RANGES[2],
        _ => ACCOUNT_RANGES[3],
    }
}

impl Ping {
    /// Why the collector should refuse this ping, if it should: an unknown
    /// format, an id that isn't 32 hex digits, a value that isn't a short
    /// lowercase word, or a range or error kind it doesn't know.
    pub fn problem(&self) -> Option<String> {
        if self.schema != SCHEMA {
            return Some(format!("unknown schema {}", self.schema));
        }
        if self.install_id.len() != 32 || !self.install_id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Some("install_id must be 32 hex digits".into());
        }
        let words = [
            ("version", Some(&self.version)),
            ("os", Some(&self.os)),
            ("arch", Some(&self.arch)),
            ("install", Some(&self.install)),
            ("storage", Some(&self.storage)),
            ("recognition", Some(&self.recognition)),
            ("llm_provider", self.llm_provider.as_ref()),
            ("ocr", Some(&self.ocr)),
        ];
        for (field, value) in words {
            if let Some(v) = value
                && !is_word(v)
            {
                return Some(format!("{field} isn't a short word"));
            }
        }
        if !ACCOUNT_RANGES.contains(&self.accounts.as_str()) {
            return Some("accounts isn't a known range".into());
        }
        for (field, v) in [("scans", &self.scans), ("unrecognised", &self.unrecognised)] {
            if !COUNT_RANGES.contains(&v.as_str()) {
                return Some(format!("{field} isn't a known range"));
            }
        }
        for (kind, range) in &self.errors {
            if !ERROR_KINDS.contains(&kind.as_str()) {
                return Some(format!("unknown error kind {kind:?}"));
            }
            if !COUNT_RANGES.contains(&range.as_str()) {
                return Some(format!("errors.{kind} isn't a known range"));
            }
        }
        None
    }
}

/// 1 to 40 characters of lowercase letters, digits and `.`, `_`, `+`, `-`.
fn is_word(s: &str) -> bool {
    (1..=40).contains(&s.len())
        && s.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'+' | b'-')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ping() -> Ping {
        Ping {
            schema: SCHEMA,
            install_id: "0123456789abcdef0123456789abcdef".into(),
            version: "1.0.0-alpha.1".into(),
            os: "windows".into(),
            arch: "x86_64".into(),
            install: "windows-release".into(),
            storage: "sqlite".into(),
            recognition: "auto".into(),
            llm_provider: Some("ollama".into()),
            ocr: "auto".into(),
            card_kingdom: false,
            setup_done: true,
            accounts: "1".into(),
            scans: "10-99".into(),
            unrecognised: "1-9".into(),
            errors: BTreeMap::from([("camera".into(), "1-9".into())]),
        }
    }

    #[test]
    fn counts_are_rounded_into_ranges() {
        let ranges: Vec<_> = [0, 1, 9, 10, 99, 100, 999, 1000, 50_000]
            .map(count_range)
            .to_vec();
        assert_eq!(
            ranges,
            [
                "0", "1-9", "1-9", "10-99", "10-99", "100-999", "100-999", "1000+", "1000+"
            ]
        );
        let accounts: Vec<_> = [0, 1, 2, 5, 6, 20, 21].map(account_range).to_vec();
        assert_eq!(accounts, ["1", "1", "2-5", "2-5", "6-20", "6-20", "21+"]);
    }

    #[test]
    fn a_proper_ping_passes() {
        assert_eq!(ping().problem(), None);
        let mut p = ping();
        p.llm_provider = None;
        p.errors.clear();
        assert_eq!(p.problem(), None);
    }

    #[test]
    fn anything_beyond_the_listed_values_is_refused() {
        let cases: [fn(&mut Ping); 8] = [
            |p| p.schema = 2,
            |p| p.install_id = "not-an-id".into(),
            |p| p.version = "1.0 <script>".into(),
            |p| p.os = "Windows 11 Pro, user jane".into(),
            |p| p.llm_provider = Some(String::new()),
            |p| p.scans = "1234".into(),
            |p| p.accounts = "7".into(),
            |p| {
                p.errors
                    .insert("camera lost: device 9 not found".into(), "1-9".into());
            },
        ];
        for (i, change) in cases.into_iter().enumerate() {
            let mut p = ping();
            change(&mut p);
            assert!(p.problem().is_some(), "case {i} passed");
        }
    }

    #[test]
    fn unknown_fields_are_refused() {
        let mut json = serde_json::to_value(ping()).unwrap();
        json["collection"] = serde_json::json!(["Lightning Bolt"]);
        assert!(serde_json::from_value::<Ping>(json).is_err());
    }
}
