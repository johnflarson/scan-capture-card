//! The Settings page. Each account's own settings (how its scan session
//! starts, its Moxfield account and hidden decks, currency, card language)
//! are saved with the account (see `prefs`); the one server setting it
//! changes, Card Kingdom prices, goes into the settings file
//! (`mtg-scan.toml`) and only admins change it, as only admins see the
//! database details. Passwords and keys stay in `.env` and are never sent.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{Result, bail};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use mtg_scan_shared::{
    CARD_LANGUAGES, Connection, DatabaseView, InfoRow, LlmView, PriceSettings, SettingsChange,
    SettingsView, Startup,
};

use super::{AppState, blocking, lan_ip};
use crate::accounts::Signed;
use crate::app;
use crate::config::{self, Config, LlmProvider, StorageBackend};
use crate::moxfield;
use crate::prefs::Prefs;
use crate::scryfall::Scryfall;
use crate::store::CardCache;
use crate::store::postgres::{self, PostgresStore};
use crate::store::sqlite::SqliteStore;

/// The server settings the Settings page changes, as last saved.
pub struct Settings {
    card_kingdom: bool,
    file: PathBuf,
    /// Command-line flags that overrode a startup setting for this run.
    overridden: Vec<String>,
}

impl Settings {
    pub fn new(cfg: &Config, overridden: Vec<String>) -> Mutex<Self> {
        Mutex::new(Self {
            card_kingdom: cfg.prices.card_kingdom,
            file: cfg.settings_file(),
            overridden,
        })
    }
}

/// The settings, even if a panic elsewhere poisoned the lock (they are
/// only replaced whole, so they're never half-changed).
pub fn lock(settings: &Mutex<Settings>) -> MutexGuard<'_, Settings> {
    settings
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

const DRY_RUN: &str =
    "Dry run: settings can't be changed. Restart without --dry-run to change them.";

pub async fn get(
    State(st): State<Arc<AppState>>,
    Extension(signed): Extension<Signed>,
) -> Response {
    let state = st.clone();
    blocking(StatusCode::INTERNAL_SERVER_ERROR, move || {
        view(&state, &signed)
    })
    .await
}

pub async fn change(
    State(st): State<Arc<AppState>>,
    Extension(signed): Extension<Signed>,
    Json(change): Json<SettingsChange>,
) -> Response {
    if st.sessions.dry_run() {
        return (StatusCode::CONFLICT, DRY_RUN).into_response();
    }
    let state = st.clone();
    blocking(StatusCode::BAD_REQUEST, move || {
        apply(&state, &signed, change)?;
        view(&state, &signed)
    })
    .await
}

/// Validate and save one change.
fn apply(st: &AppState, signed: &Signed, change: SettingsChange) -> Result<()> {
    let (mut stored, prefs) = st.accounts.prefs(&signed.user, &st.cfg)?;
    match change {
        SettingsChange::Startup(s) => {
            stored.auto_scan = Some(s.auto_scan);
            stored.table_mode = Some(s.table_mode);
            stored.finish = Some(s.finish.into());
        }
        SettingsChange::Moxfield { user } => {
            let user = user.trim();
            if !user.is_empty() && moxfield::parse_user(user).is_none() {
                bail!(
                    "\"{user}\" isn't a Moxfield user name or profile link \
                     (like https://moxfield.com/users/YourName)"
                );
            }
            stored.moxfield_user = Some(user.to_string());
        }
        SettingsChange::DeckHidden { id, hidden } => {
            if !moxfield::valid_id(&id) {
                bail!("\"{id}\" isn't a Moxfield deck ID");
            }
            let mut decks = prefs.hidden_decks;
            decks.retain(|h| *h != id);
            if hidden {
                decks.push(id);
            }
            stored.hidden_decks = Some(decks);
        }
        SettingsChange::Prices(p) => {
            let mut settings = lock(&st.settings);
            if p.card_kingdom != settings.card_kingdom {
                if !signed.user.is_admin {
                    bail!("Only an admin can switch Card Kingdom prices: they are for everyone");
                }
                config::save_card_kingdom(&settings.file, p.card_kingdom)?;
                settings.card_kingdom = p.card_kingdom;
            }
            stored.currency = Some(p.currency);
        }
        SettingsChange::CardLanguage { code } => {
            if !CARD_LANGUAGES.iter().any(|(c, _)| *c == code) {
                bail!("\"{code}\" isn't a card language mtg-scan knows");
            }
            stored.card_language = Some(code);
        }
        SettingsChange::LlmKey { key } => {
            let key = key.trim();
            stored.llm_key = if key.is_empty() {
                None
            } else {
                if key.len() > 400 || key.chars().any(char::is_whitespace) {
                    bail!("That doesn't look like an API key (no spaces, at most 400 characters)");
                }
                let Some(secrets) = &st.secrets else {
                    bail!(
                        "This server can't store keys: it couldn't read or create its \
                         secret.key (see the server's output)"
                    );
                };
                Some(secrets.seal(key)?)
            };
        }
    }
    st.accounts.save_prefs(&signed.user, &stored)
}

/// The signed-in account's settings with the server's defaults filled in.
pub fn prefs(st: &AppState, signed: &Signed) -> Result<Prefs> {
    Ok(st.accounts.prefs(&signed.user, &st.cfg)?.1)
}

fn view(st: &AppState, signed: &Signed) -> Result<SettingsView> {
    let prefs = prefs(st, signed)?;
    let settings = lock(&st.settings);
    Ok(SettingsView {
        startup: Startup {
            auto_scan: prefs.startup.auto_scan,
            table_mode: prefs.startup.table_mode,
            finish: prefs.startup.finish.into(),
        },
        overridden: settings.overridden.clone(),
        moxfield_user: prefs.moxfield_user(),
        moxfield_source: None,
        moxfield: prefs.moxfield.clone(),
        hidden_decks: prefs.hidden_decks.clone(),
        prices: PriceSettings {
            currency: prefs.currency,
            card_kingdom: settings.card_kingdom,
        },
        card_language: prefs.card_language.clone(),
        file: std::path::absolute(&settings.file)
            .unwrap_or_else(|_| settings.file.clone())
            .display()
            .to_string(),
        locked: st.sessions.dry_run().then(|| DRY_RUN.to_string()),
        connection: connection(st.addr),
        llm: llm_view(st, &prefs),
    })
}

/// The server's vision model, and whose key it uses for this account.
fn llm_view(st: &AppState, prefs: &Prefs) -> LlmView {
    let llm = &st.cfg.llm;
    let model = (llm.provider != LlmProvider::None)
        .then(|| format!("{:?} {}", llm.provider, llm.resolved_model()));
    LlmView {
        model,
        needs_key: matches!(llm.provider, LlmProvider::Openrouter | LlmProvider::Openai),
        server_key: llm.resolved_api_key().is_some(),
        key_set: prefs.llm_key.is_some(),
        cannot_save: st.secrets.is_none().then(|| {
            "This server can't store keys: it couldn't read or create its secret.key.".into()
        }),
    }
}

/// How this computer, other computers and phones reach the UI.
fn connection(addr: SocketAddr) -> Connection {
    let port = addr.port();
    let local_only = addr.ip().is_loopback();
    let network = if local_only {
        None
    } else if addr.ip().is_unspecified() {
        lan_ip().map(|ip| format!("http://{}", SocketAddr::new(ip, port)))
    } else {
        Some(format!("http://{addr}"))
    };
    Connection {
        this_computer: format!("http://localhost:{port}"),
        network,
        local_only,
    }
}

/// Admins only: it names the database server and its login.
pub async fn database(
    State(st): State<Arc<AppState>>,
    Extension(signed): Extension<Signed>,
) -> Response {
    if !signed.user.is_admin {
        return (
            StatusCode::FORBIDDEN,
            "Only admins see the database details.",
        )
            .into_response();
    }
    let cfg = st.cfg.clone();
    blocking(StatusCode::INTERNAL_SERVER_ERROR, move || {
        Ok(match cfg.storage.backend {
            StorageBackend::Sqlite => sqlite_view(&cfg),
            StorageBackend::Postgres => postgres_view(&cfg),
            StorageBackend::Markdown => DatabaseView {
                backend: "Markdown file".into(),
                settings: vec![InfoRow::new("File", cfg.collection.display().to_string())],
                status: Vec::new(),
                error: Some(
                    "Since 1.0 a Markdown file is no longer a storage backend: set [storage] \
                     backend = \"sqlite\" and run mtg-scan db import to copy it in."
                        .into(),
                ),
            },
        })
    })
    .await
}

fn postgres_view(cfg: &Config) -> DatabaseView {
    let mut view = DatabaseView {
        backend: "PostgreSQL".into(),
        settings: Vec::new(),
        status: Vec::new(),
        error: None,
    };
    let conn = match cfg.storage.resolved_connection() {
        Ok(conn) => conn,
        Err(e) => {
            view.error = Some(format!("{e:#}"));
            return view;
        }
    };
    let host = match conn.get_hosts().first() {
        Some(::postgres::config::Host::Tcp(name)) => name.clone(),
        #[allow(unreachable_patterns)]
        Some(_) => "local socket".into(),
        None => "localhost".into(),
    };
    let admin = std::env::var("MTG_SCAN_DB_ADMIN_USER")
        .ok()
        .filter(|u| !u.trim().is_empty())
        .unwrap_or_else(|| "not set (only db migrate needs it)".into());
    view.settings = vec![
        InfoRow::new("Host", host),
        InfoRow::new(
            "Port",
            conn.get_ports()
                .first()
                .copied()
                .unwrap_or(5432)
                .to_string(),
        ),
        InfoRow::new("Database", conn.get_dbname().unwrap_or("(login name)")),
        InfoRow::new("Schema", cfg.storage.schema.clone()),
        InfoRow::new("Login", conn.get_user().unwrap_or("(none)")),
        InfoRow::new("Admin login", admin),
        InfoRow::new("SSL", ssl_mode(conn.get_ssl_mode())),
        InfoRow::new(
            "Password",
            if conn.get_password().is_some() {
                "set in .env (not shown)"
            } else {
                "not set"
            },
        ),
    ];
    let status = app::http_client()
        .map(|http| Rc::new(Scryfall::new(http)))
        .and_then(|scryfall| PostgresStore::open(conn, &cfg.storage.schema, scryfall))
        .and_then(|store| Ok((store.status()?, store)));
    match status {
        Ok((s, store)) => {
            let latest = postgres::latest_migration();
            let schema = if s.schema_version < latest {
                format!("{} of {latest}: run mtg-scan db migrate", s.schema_version)
            } else {
                format!("{} (up to date)", s.schema_version)
            };
            view.status = vec![
                InfoRow::new("Server", format!("PostgreSQL {}", s.server_version)),
                InfoRow::new("Logged in as", s.user),
                InfoRow::new("Schema version", schema),
                InfoRow::new("Printings", s.printings.to_string()),
                InfoRow::new("Cards", s.cards.to_string()),
                InfoRow::new(
                    "Price points",
                    match s.first_price_date {
                        Some(d) => format!("{} since {d}", s.price_points),
                        None => s.price_points.to_string(),
                    },
                ),
            ];
            if s.schema_version >= 2 {
                view.status.push(cache_row(&store));
            }
        }
        Err(e) => view.error = Some(format!("{e:#}")),
    }
    view
}

fn sqlite_view(cfg: &Config) -> DatabaseView {
    let path = cfg.storage.sqlite_path();
    let path = std::path::absolute(&path).unwrap_or(path);
    let mut view = DatabaseView {
        backend: "SQLite".into(),
        settings: vec![InfoRow::new("File", path.display().to_string())],
        status: Vec::new(),
        error: None,
    };
    match SqliteStore::open(&path, None).and_then(|store| Ok((store.status()?, store))) {
        Ok((s, store)) => {
            view.status = vec![
                InfoRow::new("Engine", format!("SQLite {}", s.sqlite_version)),
                InfoRow::new("Schema version", s.schema_version.to_string()),
                InfoRow::new("Printings", s.printings.to_string()),
                InfoRow::new("Cards", s.cards.to_string()),
                InfoRow::new(
                    "Price points",
                    match s.first_price_date {
                        Some(d) => format!("{} since {d}", s.price_points),
                        None => s.price_points.to_string(),
                    },
                ),
            ];
            if let Ok(meta) = std::fs::metadata(&path) {
                view.status.push(InfoRow::new(
                    "Size",
                    format!("{:.1} MB", meta.len() as f64 / 1_048_576.0),
                ));
            }
            view.status.push(cache_row(&store));
        }
        Err(e) => view.error = Some(format!("{e:#}")),
    }
    view
}

/// The card cache's size and last sync, for the Database box.
fn cache_row(cache: &dyn CardCache) -> InfoRow {
    let count = cache.cached_count().unwrap_or(0);
    let value = match cache.job(crate::bulk::JOB).ok().flatten() {
        Some(job) => format!(
            "{count} printings, synced {}",
            job.done_at
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
        ),
        None if count > 0 => format!("{count} printings, daily sync not run yet"),
        None => "empty: fills on the first daily sync".into(),
    };
    InfoRow::new("Card data", value)
}

fn ssl_mode(mode: ::postgres::config::SslMode) -> &'static str {
    use ::postgres::config::SslMode;
    match mode {
        SslMode::Disable => "disable",
        SslMode::Prefer => "prefer",
        SslMode::Require => "require",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    #[test]
    fn local_only_ui_has_no_network_address() {
        let c = connection(addr("127.0.0.1:8787"));
        assert!(c.local_only);
        assert_eq!(c.network, None);
        assert_eq!(c.this_computer, "http://localhost:8787");
    }

    #[test]
    fn a_specific_address_is_the_network_address() {
        let c = connection(addr("192.168.1.20:9000"));
        assert!(!c.local_only);
        assert_eq!(c.network.as_deref(), Some("http://192.168.1.20:9000"));
    }
}
