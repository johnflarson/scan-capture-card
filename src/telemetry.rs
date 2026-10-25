//! Anonymous usage statistics (#77): once a day, while `mtg-scan ui` runs
//! and an admin allowed it (`[telemetry] enabled = true`, not overridden by
//! `DO_NOT_TRACK`), the server sends one [`Ping`] to the collector: its
//! version and platform, the setup choices, and rounded counts of cards
//! scanned and of errors by kind. Nothing else: never cards, collections,
//! decks, images, names, addresses, messages or keys.
//!
//! Counts are kept in memory ([`count_scans`], [`count_error`]) and moved
//! into `telemetry.json` in the data folder every few minutes, with the
//! server's random id and the day of the last ping, so a restart loses
//! little. While sending isn't allowed the counts are dropped, not kept.
//! The work runs on a thread of its own inside the app, not an OS scheduler.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use mtg_scan_shared::{ERROR_KINDS, Ping, SCHEMA, account_range, count_range};
use serde::{Deserialize, Serialize};

use crate::app::USER_AGENT;
use crate::config::{
    self, LlmConfig, LlmProvider, RecognitionConfig, RecognitionMode, StorageBackend,
    TelemetryConfig,
};
use crate::ocr::OcrChoice;

/// Where pings go unless `MTG_SCAN_TELEMETRY_URL` or `[telemetry] url` says
/// otherwise.
pub const DEFAULT_URL: &str = "https://www.luminaldata.com/app/shufflingtome/v1/ping";

/// How often counts are saved and a due ping is tried.
const CHECK_EVERY: Duration = Duration::from_secs(15 * 60);
/// The first check after the server starts.
const FIRST_CHECK: Duration = Duration::from_secs(2 * 60);
/// A failed ping is tried again after this long.
const RETRY_AFTER: Duration = Duration::from_secs(60 * 60);

static SCANS: AtomicU64 = AtomicU64::new(0);
static UNRECOGNISED: AtomicU64 = AtomicU64::new(0);
static ERRORS: [AtomicU64; ERROR_KINDS.len()] = [const { AtomicU64::new(0) }; ERROR_KINDS.len()];

/// Count a finished scan: cards recognised and cards that weren't.
pub fn count_scans(recognised: u64, unrecognised: u64) {
    SCANS.fetch_add(recognised, Ordering::Relaxed);
    UNRECOGNISED.fetch_add(unrecognised, Ordering::Relaxed);
}

/// Count an error shown to someone. Only its kind is kept ([`error_kind`]).
pub fn count_error(message: &str) {
    let kind = error_kind(message);
    if let Some(i) = ERROR_KINDS.iter().position(|k| *k == kind) {
        ERRORS[i].fetch_add(1, Ordering::Relaxed);
    }
}

/// The kind of error a message is about, one of [`ERROR_KINDS`].
pub fn error_kind(message: &str) -> &'static str {
    let m = message.to_lowercase();
    let any = |words: &[&str]| words.iter().any(|w| m.contains(w));
    if any(&["moxfield"]) {
        "moxfield"
    } else if any(&["scryfall"]) {
        "scryfall"
    } else if any(&["tcgplayer", "card kingdom", "cardkingdom", "price"]) {
        "prices"
    } else if any(&["database", "sqlite", "postgres", "could not add", "save"]) {
        "storage"
    } else if any(&["camera", "webcam", "media foundation", "video device"]) {
        "camera"
    } else if any(&["ocr", "paddle"]) {
        "ocr"
    } else if any(&[
        "ollama",
        "llm",
        "vision model",
        "openrouter",
        "openai",
        "anthropic",
        "gemini",
        "lm studio",
        "api key",
        "model",
    ]) {
        "vision_model"
    } else {
        "other"
    }
}

/// `DO_NOT_TRACK` is set (to anything but `0` or `false`): nothing is sent.
pub fn do_not_track() -> bool {
    std::env::var("DO_NOT_TRACK").is_ok_and(|v| {
        let v = v.trim().to_lowercase();
        !v.is_empty() && v != "0" && v != "false"
    })
}

/// Whether pings may be sent with this `[telemetry] enabled`.
pub fn allowed(enabled: Option<bool>) -> bool {
    enabled == Some(true) && !do_not_track()
}

/// Where pings go: `MTG_SCAN_TELEMETRY_URL`, else `[telemetry] url`, else
/// [`DEFAULT_URL`].
pub fn url(cfg: &TelemetryConfig) -> String {
    std::env::var("MTG_SCAN_TELEMETRY_URL")
        .ok()
        .or_else(|| cfg.url.clone())
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| DEFAULT_URL.to_string())
}

/// `telemetry.json` in the data folder.
pub fn state_file() -> PathBuf {
    config::data_dir().join("telemetry.json")
}

/// How the server is set up, as the ping reports it.
#[derive(Debug, Clone)]
pub struct Facts {
    pub storage: StorageBackend,
    pub mode: RecognitionMode,
    pub llm: LlmProvider,
    pub ocr: OcrChoice,
    pub card_kingdom: bool,
    pub setup_done: bool,
    /// Accounts with a password.
    pub accounts: u64,
}

impl Facts {
    pub fn new(
        storage: StorageBackend,
        recognition: &RecognitionConfig,
        llm: &LlmConfig,
        card_kingdom: bool,
        setup_done: bool,
        accounts: u64,
    ) -> Self {
        Self {
            storage,
            mode: recognition.mode,
            llm: llm.provider,
            ocr: recognition.ocr,
            card_kingdom,
            setup_done,
            accounts,
        }
    }
}

/// Counts not sent yet.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Pending {
    scans: u64,
    unrecognised: u64,
    errors: BTreeMap<String, u64>,
}

impl Pending {
    fn is_empty(&self) -> bool {
        *self == Pending::default()
    }

    /// Add the counts in memory, emptying them when `take`.
    fn add_counts(&mut self, take: bool) {
        let read = |a: &AtomicU64| {
            if take {
                a.swap(0, Ordering::Relaxed)
            } else {
                a.load(Ordering::Relaxed)
            }
        };
        self.scans += read(&SCANS);
        self.unrecognised += read(&UNRECOGNISED);
        for (kind, count) in ERROR_KINDS.iter().zip(&ERRORS) {
            let n = read(count);
            if n > 0 {
                *self.errors.entry(kind.to_string()).or_default() += n;
            }
        }
    }
}

/// `telemetry.json`: the server's id, the last ping's day, counts not sent.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Log {
    install_id: Option<String>,
    last_sent: Option<String>,
    pending: Pending,
    #[serde(skip)]
    path: PathBuf,
}

impl Log {
    /// The file at `path`; a missing or unreadable one starts afresh.
    pub fn load(path: &Path) -> Self {
        let mut log: Log = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        log.path = path.to_path_buf();
        log
    }

    pub fn save(&self) -> Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("failed to create {}", dir.display()))?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)
            .with_context(|| format!("failed to write {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("failed to update {}", self.path.display()))
    }

    pub fn last_sent(&self) -> Option<&str> {
        self.last_sent.as_deref()
    }

    /// The server's id, made now if there is none yet.
    fn install_id(&mut self) -> Result<String> {
        if let Some(id) = &self.install_id {
            return Ok(id.clone());
        }
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(|e| anyhow!("no secure random source: {e}"))?;
        let id: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        self.install_id = Some(id.clone());
        Ok(id)
    }

    /// Move the counts in memory into the file's.
    fn collect(&mut self) {
        self.pending.add_counts(true);
    }

    /// Drop every count: sending isn't allowed. True if there were any.
    fn discard(&mut self) -> bool {
        let mut dropped = Pending::default();
        dropped.add_counts(true);
        let had = !self.pending.is_empty() || !dropped.is_empty();
        self.pending = Pending::default();
        had
    }

    /// What the next ping sends: the counts so far, without taking them.
    pub fn preview(&mut self, facts: &Facts) -> Result<Ping> {
        let mut pending = self.pending.clone();
        pending.add_counts(false);
        Ok(build(self.install_id()?, facts, &pending))
    }

    fn ping(&mut self, facts: &Facts) -> Result<Ping> {
        let pending = self.pending.clone();
        Ok(build(self.install_id()?, facts, &pending))
    }

    fn sent(&mut self, day: String) {
        self.last_sent = Some(day);
        self.pending = Pending::default();
    }
}

/// How this copy was installed: `MTG_SCAN_INSTALL` (the Docker image sets
/// `docker`), else what the release build set (`windows-release`), else
/// `source`.
fn install_kind() -> String {
    std::env::var("MTG_SCAN_INSTALL")
        .ok()
        .or_else(|| option_env!("MTG_SCAN_BUILD").map(str::to_string))
        .map(|v| v.trim().to_lowercase())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "source".into())
}

fn build(install_id: String, facts: &Facts, pending: &Pending) -> Ping {
    let llm = (facts.mode != RecognitionMode::Ocr && facts.llm != LlmProvider::None)
        .then(|| facts.llm.as_str().to_string());
    Ping {
        schema: SCHEMA,
        install_id,
        version: env!("CARGO_PKG_VERSION").to_string(),
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        install: install_kind(),
        storage: match facts.storage {
            StorageBackend::Sqlite => "sqlite",
            StorageBackend::Postgres => "postgres",
            StorageBackend::Markdown => "markdown",
        }
        .into(),
        recognition: match facts.mode {
            RecognitionMode::Auto => "auto",
            RecognitionMode::Ocr => "ocr",
            RecognitionMode::Llm => "llm",
        }
        .into(),
        llm_provider: llm,
        ocr: match facts.ocr {
            OcrChoice::Auto => "auto",
            OcrChoice::Windows => "windows",
            OcrChoice::Paddle => "paddle",
        }
        .into(),
        card_kingdom: facts.card_kingdom,
        setup_done: facts.setup_done,
        accounts: account_range(facts.accounts).into(),
        scans: count_range(pending.scans).into(),
        unrecognised: count_range(pending.unrecognised).into(),
        errors: pending
            .errors
            .iter()
            .filter(|(_, n)| **n > 0)
            .map(|(kind, n)| (kind.clone(), count_range(*n).to_string()))
            .collect(),
    }
}

/// Today in UTC, `YYYY-MM-DD`: one ping per server and day.
fn today() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

/// Send `ping` to `url`.
pub fn send(url: &str, ping: &Ping) -> Result<()> {
    crate::tls::init();
    reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(20))
        .build()?
        .post(url)
        .json(ping)
        .send()
        .and_then(|r| r.error_for_status())
        .with_context(|| format!("failed to send usage statistics to {url}"))?;
    Ok(())
}

/// Save the counts, then send today's ping if it is due. `facts` is `None`
/// while sending isn't allowed. True when a ping was sent.
pub fn check(path: &Path, url: &str, facts: Option<&Facts>) -> Result<bool> {
    let mut log = Log::load(path);
    let Some(facts) = facts else {
        if log.discard() {
            log.save()?;
        }
        return Ok(false);
    };
    log.collect();
    let day = today();
    let due = log.last_sent.as_deref() != Some(day.as_str());
    let result = if due {
        let ping = log.ping(facts)?;
        send(url, &ping).map(|()| log.sent(day))
    } else {
        Ok(())
    };
    log.save()?;
    result.map(|()| due)
}

/// Send now, whatever the day (`mtg-scan telemetry send`).
pub fn send_now(path: &Path, url: &str, facts: &Facts) -> Result<Ping> {
    let mut log = Log::load(path);
    log.collect();
    let ping = log.ping(facts)?;
    send(url, &ping)?;
    log.sent(today());
    log.save()?;
    Ok(ping)
}

/// Check every 15 minutes while the server runs (the first time two
/// minutes after it starts). `facts` gives the server's setup when sending
/// is allowed, `None` otherwise; it is asked each time, so changing the
/// setting applies without a restart.
pub fn spawn(path: PathBuf, url: String, facts: impl Fn() -> Option<Facts> + Send + 'static) {
    let spawned = std::thread::Builder::new()
        .name("telemetry".into())
        .spawn(move || {
            std::thread::sleep(FIRST_CHECK);
            let mut failed_at: Option<Instant> = None;
            let mut told = false;
            loop {
                let facts = facts();
                // After a failure, keep saving counts but wait before trying again.
                let waiting = failed_at.is_some_and(|t| t.elapsed() < RETRY_AFTER);
                let result = if waiting && facts.is_some() {
                    let mut log = Log::load(&path);
                    log.collect();
                    log.save().map(|()| false)
                } else {
                    check(&path, &url, facts.as_ref())
                };
                match result {
                    Ok(_) => failed_at = None,
                    Err(e) => {
                        failed_at = Some(Instant::now());
                        if !told {
                            told = true;
                            eprintln!(
                                "  ! usage statistics weren't sent ({e:#}); trying again later"
                            );
                        }
                    }
                }
                std::thread::sleep(CHECK_EVERY);
            }
        });
    if let Err(e) = spawned {
        eprintln!("  ! could not start sending usage statistics ({e})");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> Facts {
        Facts {
            storage: StorageBackend::Sqlite,
            mode: RecognitionMode::Auto,
            llm: LlmProvider::Ollama,
            ocr: OcrChoice::Auto,
            card_kingdom: false,
            setup_done: true,
            accounts: 3,
        }
    }

    #[test]
    fn errors_are_counted_by_kind_only() {
        let kinds: Vec<_> = [
            "camera lost: device 9 not found",
            "Ollama at http://localhost:11434 doesn't answer",
            "Scryfall: 429 Too Many Requests",
            "could not add it: database is locked",
            "Moxfield deck list failed",
            "PaddleOCR models couldn't load",
            "something odd",
        ]
        .map(error_kind)
        .to_vec();
        assert_eq!(
            kinds,
            [
                "camera",
                "vision_model",
                "scryfall",
                "storage",
                "moxfield",
                "ocr",
                "other"
            ]
        );
    }

    #[test]
    fn ping_reports_the_setup_and_rounded_counts() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = Log::load(&dir.path().join("telemetry.json"));
        log.pending = Pending {
            scans: 42,
            unrecognised: 3,
            errors: BTreeMap::from([("camera".into(), 1), ("scryfall".into(), 0)]),
        };
        let p = log.ping(&facts()).unwrap();
        assert_eq!(p.problem(), None, "the collector accepts it");
        assert_eq!(p.install_id.len(), 32);
        assert_eq!(
            (
                p.storage.as_str(),
                p.recognition.as_str(),
                p.llm_provider.as_deref()
            ),
            ("sqlite", "auto", Some("ollama"))
        );
        assert_eq!(
            (
                p.accounts.as_str(),
                p.scans.as_str(),
                p.unrecognised.as_str()
            ),
            ("2-5", "10-99", "1-9")
        );
        assert_eq!(
            p.errors,
            BTreeMap::from([("camera".to_string(), "1-9".to_string())])
        );
        // The id stays the same.
        assert_eq!(log.ping(&facts()).unwrap().install_id, p.install_id);

        let mut ocr_only = facts();
        ocr_only.mode = RecognitionMode::Ocr;
        assert_eq!(log.ping(&ocr_only).unwrap().llm_provider, None);
    }

    #[test]
    fn the_log_survives_a_restart_and_is_emptied_once_sent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("telemetry.json");
        let mut log = Log::load(&path);
        log.pending.scans = 5;
        let id = log.ping(&facts()).unwrap().install_id;
        log.save().unwrap();

        let mut again = Log::load(&path);
        assert_eq!(again.pending.scans, 5);
        assert_eq!(again.ping(&facts()).unwrap().install_id, id);
        again.sent("2026-10-25".into());
        again.save().unwrap();
        let after = Log::load(&path);
        assert_eq!(after.last_sent(), Some("2026-10-25"));
        assert!(after.pending.is_empty());
    }

    #[test]
    fn nothing_is_sent_or_kept_when_not_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telemetry.json");
        let mut log = Log::load(&path);
        log.pending.scans = 7;
        log.save().unwrap();
        // An address nothing listens on: a send would fail.
        let sent = check(&path, "http://127.0.0.1:9/v1/ping", None).unwrap();
        assert!(!sent);
        assert!(Log::load(&path).pending.is_empty(), "counts are dropped");
    }

    #[test]
    fn a_failed_send_keeps_the_counts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telemetry.json");
        let mut log = Log::load(&path);
        log.pending.scans = 7;
        log.save().unwrap();
        assert!(check(&path, "http://127.0.0.1:9/v1/ping", Some(&facts())).is_err());
        let log = Log::load(&path);
        assert!(log.pending.scans >= 7, "other tests may count scans too");
        assert_eq!(log.last_sent(), None);
    }

    #[test]
    fn only_a_yes_allows_sending() {
        // DO_NOT_TRACK isn't set in tests.
        assert!(!allowed(None));
        assert!(!allowed(Some(false)));
        assert_eq!(allowed(Some(true)), !do_not_track());
    }
}
