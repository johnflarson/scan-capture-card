//! One scan session per signed-in account, all sharing one recognition
//! worker ([`Scanner`]). A session starts the first time its account opens
//! the app, from the account's startup settings (and the `--manual`,
//! `--table` and `--finish` flags), and saves scans into the account's
//! collection in its card language, using its own LLM key when it has one.
//!
//! The camera attached to the server belongs to the first account's
//! session (the owner); the others scan with their browser's camera.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{Result, anyhow};
use mtg_scan_shared::Mode;

use crate::accounts::{Accounts, User};
use crate::config::Config;
use crate::engine::{Engine, EngineOptions, ScanSetup, Scanner, Setup};
use crate::live;
use crate::model::Finish;
use crate::secrets::SecretBox;

/// Flags that override every account's startup settings for this run.
#[derive(Debug, Clone, Copy, Default)]
pub struct StartFlags {
    pub manual: bool,
    pub table: bool,
    pub finish: Option<Finish>,
}

impl StartFlags {
    /// What the flags change, for the Settings page.
    pub fn describe(&self) -> Vec<String> {
        let mut notes = Vec::new();
        if self.manual {
            notes.push("--manual: auto-scan starts off".to_string());
        }
        if self.table {
            notes.push("--table: scanning starts in table mode".to_string());
        }
        if let Some(f) = self.finish {
            notes.push(format!("--finish {f}: new cards start as {f}"));
        }
        notes
    }
}

pub struct Sessions {
    scanner: Arc<Scanner>,
    cfg: Config,
    opts: EngineOptions,
    flags: StartFlags,
    accounts: Arc<Accounts>,
    secrets: Option<Arc<SecretBox>>,
    sessions: Mutex<HashMap<i64, Arc<Engine>>>,
}

impl Sessions {
    pub fn start(
        cfg: &Config,
        opts: EngineOptions,
        flags: StartFlags,
        accounts: Arc<Accounts>,
        secrets: Option<Arc<SecretBox>>,
    ) -> Result<Self> {
        Ok(Self {
            scanner: Scanner::start(cfg, opts)?,
            cfg: cfg.clone(),
            opts,
            flags,
            accounts,
            secrets,
            sessions: Mutex::new(HashMap::new()),
        })
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<i64, Arc<Engine>>> {
        self.sessions.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The account's session, started on first use (blocking: the first
    /// account's opens the server's camera).
    pub fn of(&self, user: &User) -> Result<Arc<Engine>> {
        let mut sessions = self.lock();
        if let Some(engine) = sessions.get(&user.id) {
            if engine.is_running() {
                return Ok(engine.clone());
            }
            // Its camera thread stopped (e.g. the camera was unplugged): start over.
            sessions.remove(&user.id);
        }
        let (_, prefs) = self.accounts.prefs(user, &self.cfg)?;
        let owner = self.accounts.first_account()? == Some(user.id);
        let mut opts = self.opts;
        opts.mode = if self.flags.table || prefs.startup.table_mode {
            Mode::Table
        } else {
            Mode::Single
        };
        opts.finish = self.flags.finish.unwrap_or(prefs.startup.finish);
        opts.auto = prefs.startup.auto_scan && !self.flags.manual;
        opts.server_camera = owner;
        let engine = Arc::new(
            Engine::start(&self.scanner, &self.cfg, opts, self.setup(user))
                .map_err(|e| anyhow!("could not start the scan session: {e:#}"))?,
        );
        let who = (!owner).then(|| user.name.clone());
        live::spawn_console_logger(&engine, who);
        sessions.insert(user.id, engine.clone());
        Ok(engine)
    }

    /// What each of the account's scans needs, read when it starts (so
    /// settings changes apply to the next scan).
    fn setup(&self, user: &User) -> Setup {
        let (accounts, cfg, secrets) = (
            self.accounts.clone(),
            self.cfg.clone(),
            self.secrets.clone(),
        );
        let user = user.clone();
        Arc::new(move || {
            let mut setup = ScanSetup {
                user: Some(user.id),
                ..ScanSetup::default()
            };
            match accounts.prefs(&user, &cfg) {
                Ok((_, prefs)) => {
                    setup.card_language = prefs.card_language;
                    setup.llm_key = match (&secrets, prefs.llm_key) {
                        (Some(secrets), Some(sealed)) => match secrets.unseal(&sealed) {
                            Ok(key) => Some(key),
                            Err(e) => {
                                eprintln!("  ! {}'s LLM key {e:#}", user.name);
                                None
                            }
                        },
                        _ => None,
                    };
                }
                Err(e) => eprintln!("  ! could not read {}'s settings ({e:#})", user.name),
            }
            setup
        })
    }

    pub fn dry_run(&self) -> bool {
        self.opts.dry_run
    }

    /// The recognition worker runs.
    pub fn is_running(&self) -> bool {
        self.scanner.is_running()
    }

    /// Whether scans are being saved: not in a dry run, and unless every
    /// session has its Save switch off. Background price recording pauses
    /// with it.
    pub fn saving(self: &Arc<Self>) -> impl Fn() -> bool + Send + 'static {
        let this = self.clone();
        move || {
            !this.opts.dry_run && {
                let sessions = this.lock();
                sessions.is_empty() || sessions.values().any(|e| e.is_saving())
            }
        }
    }

    /// Stop every session, then the worker.
    pub fn shutdown(&self) -> Result<()> {
        let sessions: Vec<Arc<Engine>> = self.lock().drain().map(|(_, e)| e).collect();
        let mut result = Ok(());
        for engine in sessions {
            let stopped = match Arc::try_unwrap(engine) {
                Ok(engine) => engine.shutdown(),
                Err(_) => Err(anyhow!("a scan session is still in use")),
            };
            if result.is_ok() {
                result = stopped;
            }
        }
        self.scanner.shutdown().and(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_say_what_they_override() {
        assert!(StartFlags::default().describe().is_empty());
        let notes = StartFlags {
            manual: true,
            table: true,
            finish: Some(Finish::Foil),
        }
        .describe();
        assert_eq!(notes.len(), 3);
        assert!(notes[2].contains("foil"));
    }
}
