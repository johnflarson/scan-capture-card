//! The Settings page: how `mtg-scan ui` starts, the Moxfield account and
//! hidden decks (saved in the settings file, `mtg-scan.toml`), plus read-only
//! database and connection details. Passwords and tokens stay in `.env`; the
//! only secret ever sent is the sign-in link, and only to this computer.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{Result, bail};
use axum::Json;
use axum::extract::{ConnectInfo, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use mtg_scan_shared::{Connection, DatabaseView, InfoRow, SettingsChange, SettingsView, Startup};

use super::{AppState, access, blocking, lan_ip};
use crate::app;
use crate::config::{self, Config, DecksConfig, StorageBackend, UiConfig};
use crate::moxfield;
use crate::scryfall::Scryfall;
use crate::store::CardCache;
use crate::store::markdown::MarkdownStore;
use crate::store::postgres::{self, PostgresStore};
use crate::store::sqlite::SqliteStore;

/// What the Settings page edits, as last saved.
pub struct Settings {
    ui: UiConfig,
    decks: DecksConfig,
    file: PathBuf,
    /// Command-line flags that overrode a startup setting for this run.
    overridden: Vec<String>,
}

impl Settings {
    pub fn new(cfg: &Config, overridden: Vec<String>) -> Mutex<Self> {
        Mutex::new(Self {
            ui: cfg.ui.clone(),
            decks: cfg.decks.clone(),
            file: cfg.settings_file(),
            overridden,
        })
    }

    pub fn moxfield_user(&self) -> Option<String> {
        moxfield::configured_user(self.decks.moxfield_user.as_deref())
    }

    pub fn is_hidden(&self, deck_id: &str) -> bool {
        self.decks.hidden.iter().any(|h| h == deck_id)
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
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
) -> Json<SettingsView> {
    Json(view(&st, peer))
}

pub async fn change(
    State(st): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(change): Json<SettingsChange>,
) -> Response {
    if st.engine.view().dry_run {
        return (StatusCode::CONFLICT, DRY_RUN).into_response();
    }
    let state = st.clone();
    match tokio::task::spawn_blocking(move || apply(&state, change)).await {
        Ok(Ok(())) => Json(view(&st, peer)).into_response(),
        Ok(Err(e)) => (StatusCode::BAD_REQUEST, format!("{e:#}")).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Validate and save one change; the settings in memory change only once
/// the file is written.
fn apply(st: &AppState, change: SettingsChange) -> Result<()> {
    let mut settings = lock(&st.settings);
    let mut ui = settings.ui.clone();
    let mut decks = settings.decks.clone();
    match change {
        SettingsChange::Startup(s) => {
            ui.auto_scan = s.auto_scan;
            ui.table_mode = s.table_mode;
            ui.finish = s.finish.into();
        }
        SettingsChange::Moxfield { user } => {
            let user = user.trim();
            if !user.is_empty() && moxfield::parse_user(user).is_none() {
                bail!(
                    "\"{user}\" isn't a Moxfield user name or profile link \
                     (like https://moxfield.com/users/YourName)"
                );
            }
            decks.moxfield_user = Some(user.to_string());
        }
        SettingsChange::DeckHidden { id, hidden } => {
            if !moxfield::valid_id(&id) {
                bail!("\"{id}\" isn't a Moxfield deck ID");
            }
            decks.hidden.retain(|h| *h != id);
            if hidden {
                decks.hidden.push(id);
            }
        }
    }
    config::save_settings(&settings.file, &ui, &decks)?;
    settings.ui = ui;
    settings.decks = decks;
    Ok(())
}

fn view(st: &AppState, peer: SocketAddr) -> SettingsView {
    let settings = lock(&st.settings);
    let configured = moxfield::configured(settings.decks.moxfield_user.as_deref());
    let file_name = settings
        .file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "the settings file".into());
    SettingsView {
        startup: Startup {
            auto_scan: settings.ui.auto_scan,
            table_mode: settings.ui.table_mode,
            finish: settings.ui.finish.into(),
        },
        overridden: settings.overridden.clone(),
        moxfield_user: configured
            .as_ref()
            .and_then(|(v, _)| moxfield::parse_user(v)),
        moxfield_source: configured.as_ref().map(|(_, source)| match source {
            moxfield::Source::Settings => file_name,
            moxfield::Source::Env => format!("{} in .env", moxfield::USER_VAR),
        }),
        moxfield: configured.map(|(v, _)| v).unwrap_or_default(),
        hidden_decks: settings.decks.hidden.clone(),
        file: std::path::absolute(&settings.file)
            .unwrap_or_else(|_| settings.file.clone())
            .display()
            .to_string(),
        locked: st.engine.view().dry_run.then(|| DRY_RUN.to_string()),
        connection: connection(st.addr, peer),
    }
}

/// How this computer, other computers and phones reach the UI.
fn connection(addr: SocketAddr, peer: SocketAddr) -> Connection {
    let port = addr.port();
    let local_only = addr.ip().is_loopback();
    let network = if local_only {
        None
    } else if addr.ip().is_unspecified() {
        lan_ip().map(|ip| format!("http://{}", SocketAddr::new(ip, port)))
    } else {
        Some(format!("http://{addr}"))
    };
    let token = access::configured_token().filter(|_| !local_only);
    let sign_in_link = match (&network, &token) {
        (Some(network), Some(token)) if access::is_local(peer.ip()) => {
            Some(format!("{network}/?token={token}"))
        }
        _ => None,
    };
    Connection {
        this_computer: format!("http://localhost:{port}"),
        network,
        local_only,
        token_required: token.is_some(),
        sign_in_link,
    }
}

pub async fn database(State(st): State<Arc<AppState>>) -> Response {
    let cfg = st.cfg.clone();
    blocking(StatusCode::INTERNAL_SERVER_ERROR, move || {
        Ok(match cfg.storage.backend {
            StorageBackend::Sqlite => sqlite_view(&cfg),
            StorageBackend::Postgres => postgres_view(&cfg),
            StorageBackend::Markdown => markdown_view(&cfg),
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

fn markdown_view(cfg: &Config) -> DatabaseView {
    let path = std::path::absolute(&cfg.collection).unwrap_or_else(|_| cfg.collection.clone());
    let mut view = DatabaseView {
        backend: "Markdown file".into(),
        settings: vec![InfoRow::new("File", path.display().to_string())],
        status: Vec::new(),
        error: None,
    };
    let entries = MarkdownStore::open(&path).and_then(|store| {
        use crate::store::CollectionStore;
        store.entries()
    });
    match entries {
        Ok(entries) => {
            view.status = vec![
                InfoRow::new("Printings", entries.len().to_string()),
                InfoRow::new(
                    "Cards",
                    entries
                        .iter()
                        .map(|e| u64::from(e.quantity))
                        .sum::<u64>()
                        .to_string(),
                ),
            ];
            if let Ok(meta) = std::fs::metadata(&path) {
                view.status.push(InfoRow::new(
                    "Size",
                    format!("{:.1} KB", meta.len() as f64 / 1024.0),
                ));
                if let Ok(modified) = meta.modified() {
                    view.status.push(InfoRow::new(
                        "Last changed",
                        chrono::DateTime::<chrono::Local>::from(modified)
                            .format("%Y-%m-%d %H:%M")
                            .to_string(),
                    ));
                }
            }
        }
        Err(e) => view.error = Some(format!("{e:#}")),
    }
    view
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    #[test]
    fn local_only_ui_has_no_network_address() {
        let c = connection(addr("127.0.0.1:8787"), addr("127.0.0.1:50000"));
        assert!(c.local_only);
        assert_eq!(c.network, None);
        assert_eq!(c.this_computer, "http://localhost:8787");
        assert!(!c.token_required);
        assert_eq!(c.sign_in_link, None);
    }

    #[test]
    fn a_specific_address_is_the_network_address() {
        let c = connection(addr("192.168.1.20:9000"), addr("192.168.1.30:50000"));
        assert!(!c.local_only);
        assert_eq!(c.network.as_deref(), Some("http://192.168.1.20:9000"));
        // Never sent to another computer.
        assert_eq!(c.sign_in_link, None);
    }
}
