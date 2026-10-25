//! Web front end: `mtg-scan ui` serves the Leptos app (built by Trunk from
//! `ui/` and embedded in the binary) plus a small API, reachable from other
//! computers on the network.
//!
//! | Route | |
//! |---|---|
//! | `GET /api/auth` | who is signed in, and whether the admin account still needs creating |
//! | `POST /api/auth/setup`, `/sign-in`, `/sign-out` | create the first (admin) account, sign in, sign out |
//! | `GET`/`POST /api/auth/link` | an invite or password-reset link: what it is for, then use it |
//! | `POST /api/account/password` | change one's password (signs out other browsers) |
//! | `GET`/`POST /api/account/tokens`, `DELETE /api/account/tokens/{id}` | API tokens for scripts |
//! | `GET /api/users`, `POST /api/users/invite`, `POST /api/users/{id}[/reset]` | admins: users, invite links, reset links, admin and disabled flags |
//! | `GET /api/session` | the account's scan session: state + its cards (started on first use) |
//! | `GET /api/events` | Server-Sent Events: state changes, new cards, messages |
//! | `GET /api/stream.mjpg` | live camera preview (MJPEG) |
//! | `GET /api/camera` | WebSocket: the page sends its camera's frames (see [`camera`]) |
//! | `GET /api/health` | "ok" while the scanner runs, else 503 (`mtg-scan health`, container health checks) |
//! | `POST /api/command` | `ClientCommand` JSON: scan now, undo, set mode, … |
//! | `GET /api/collection` | the signed-in user's collection with prices and totals |
//! | `GET /api/printing?q=` | a printing from a Scryfall link or ID (edit dialog) |
//! | `GET /api/printings?name=` | every paper printing of a card (review: pick another printing) |
//! | `POST /api/collection/edit` | `RowEdit` JSON: set quantity, remove, change printing/finish |
//! | `POST /api/collection/add` | `AddCard` JSON: add copies of a printing by hand |
//! | `GET /api/decks[?refresh=true][&all=true]` | the signed-in account's Moxfield user's public decks, without hidden ones unless `all` |
//! | `GET /api/decks/{id}` | one of those decks with its cards |
//! | `GET`/`POST /api/settings` | Settings page: the account's startup options, Moxfield account, hidden decks, currency, card language; Card Kingdom (admins) (`SettingsChange` JSON) |
//! | `/api/setup*` | admins: the setup wizard (recognition, Ollama check and download, a one-card test, usage statistics and exactly what they send, done) |
//! | `GET /api/settings/database` | admins: where the collection is stored, with live status (no passwords) |
//! | everything else | the embedded UI |
//!
//! Everything but the app's files, the health check and signing in needs a
//! signed-in user (see [`auth`]).

mod auth;
mod camera;
mod sessions;
mod settings;
mod setup;

pub use sessions::StartFlags;

use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, Uri, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Extension, Json, Router};
use futures_util::{Stream, StreamExt};
use mtg_scan_shared::{
    AddCard, ClientCommand, CollectionRow, CollectionView, DecksView, EditResult, PrintingView,
    RowEdit, ServerEvent, SessionView, Totals, ValueChange,
};
use tokio::sync::watch;
use tokio_stream::wrappers::BroadcastStream;

use crate::accounts::{self, Accounts, Signed};
use crate::app::{self, App};
use crate::config::Config;
use crate::engine::{Engine, EngineEvent, EngineOptions};
use crate::model::{Card, CollectionEntry, Finish, RowKey, copies};
use crate::moxfield::Moxfield;
use crate::scryfall::{self, Scryfall};
use crate::secrets::SecretBox;
use sessions::Sessions;

/// Cap the preview stream at ~15 frames per second per viewer.
const STREAM_FRAME_INTERVAL: Duration = Duration::from_millis(66);

pub struct WebOptions {
    pub addr: SocketAddr,
    pub open_browser: bool,
    /// Flags that override every account's startup settings this run.
    pub flags: StartFlags,
}

/// The UI files built by `trunk build --release` in `ui/`.
#[derive(rust_embed::Embed)]
#[folder = "ui/dist/"]
struct Assets;

struct AppState {
    /// Every account's scan session.
    sessions: Arc<Sessions>,
    accounts: Arc<Accounts>,
    /// Seals the API keys people save (`None` if the server's key couldn't
    /// be read or created).
    secrets: Option<Arc<SecretBox>>,
    cfg: Config,
    /// The setup wizard's model download.
    setup: setup::SetupState,
    /// What the Settings page changes while running.
    settings: std::sync::Mutex<settings::Settings>,
    /// The address the server listens on.
    addr: SocketAddr,
    /// Flips to true on shutdown so long-lived streams end.
    shutdown: watch::Receiver<bool>,
}

pub fn run(cfg: &Config, opts: EngineOptions, web: WebOptions) -> Result<()> {
    if opts.dry_run {
        println!("Dry run: cards are identified and priced but not saved.");
    }
    let accounts = Arc::new(Accounts::new(accounts::open_store(cfg)?)?);
    let secrets = match SecretBox::open() {
        Ok(secrets) => Some(Arc::new(secrets)),
        Err(e) => {
            eprintln!("  ! people can't save API keys: {e:#}");
            None
        }
    };
    let sessions = Arc::new(Sessions::start(
        cfg,
        opts,
        web.flags,
        accounts.clone(),
        secrets.clone(),
    )?);
    // The owner's session has the server's camera: open it now, as before
    // accounts, so scanning works as soon as the server is up.
    if let Some(owner) = accounts.first_account_user()? {
        let engine = sessions.of(&owner)?;
        println!("Camera: {}", engine.view().camera);
    }
    // Keeps the card data and daily prices current while the app runs.
    crate::bulk::spawn_daily(cfg.clone(), sessions.saving());

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to start the async runtime")?;
    let served = runtime.block_on(serve(cfg.clone(), sessions.clone(), accounts, secrets, web));
    // Dropping the runtime ends every connection, releasing their session handles.
    drop(runtime);
    served.and(sessions.shutdown())
}

async fn serve(
    cfg: Config,
    sessions: Arc<Sessions>,
    accounts: Arc<Accounts>,
    secrets: Option<Arc<SecretBox>>,
    web: WebOptions,
) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(web.addr)
        .await
        .with_context(|| format!("could not listen on {} (port in use?)", web.addr))?;
    let port = web.addr.port();
    let local = format!("http://localhost:{port}");
    println!("\nWeb UI: {local}");
    if web.addr.ip().is_loopback() {
        println!("   only on this computer (--bind {})", web.addr.ip());
    } else {
        if web.addr.ip().is_unspecified()
            && let Some(ip) = lan_ip()
        {
            println!("   on other computers: http://{ip}:{port}");
        }
        println!(
            "   (Windows may ask whether to allow mtg-scan on the network: allow Private networks.)"
        );
    }
    // Without accounts, whoever has this code creates the admin account.
    let first_page = match accounts.setup_code() {
        Some(code) => {
            let link = format!("{local}/setup?code={code}");
            println!("\nNo accounts yet. Create the admin account here:\n   {link}");
            println!("   (from another computer, put its address in place of localhost)");
            link
        }
        None => local,
    };
    println!("Press Ctrl+C to stop.\n");
    if web.open_browser {
        open_browser(&first_page);
    }

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let state = Arc::new(AppState {
        sessions: sessions.clone(),
        accounts,
        secrets,
        setup: setup::SetupState::default(),
        settings: settings::Settings::new(&cfg, web.flags.describe()),
        cfg,
        addr: web.addr,
        shutdown: shutdown_rx,
    });
    // Usage statistics, once a day while allowed (see `telemetry`).
    let st = state.clone();
    crate::telemetry::spawn(
        crate::telemetry::state_file(),
        crate::telemetry::url(&st.cfg.telemetry),
        move || setup::telemetry_facts(&st, true),
    );
    let app = Router::new()
        .route("/api/auth", get(auth::status))
        .route("/api/auth/setup", post(auth::set_up))
        .route("/api/auth/sign-in", post(auth::sign_in))
        .route("/api/auth/sign-out", post(auth::sign_out))
        .route("/api/auth/link", get(auth::link_info).post(auth::redeem))
        .route("/api/account/password", post(auth::change_password))
        .route(
            "/api/account/tokens",
            get(auth::tokens).post(auth::new_token),
        )
        .route("/api/account/tokens/{id}", delete(auth::delete_token))
        .route("/api/users", get(auth::users))
        .route("/api/users/invite", post(auth::invite))
        .route("/api/users/{id}", post(auth::change_user))
        .route("/api/users/{id}/reset", post(auth::reset))
        .route("/api/session", get(session))
        .route("/api/events", get(events))
        .route("/api/stream.mjpg", get(stream))
        .route("/api/camera", get(camera::connect))
        .route("/api/health", get(health))
        .route("/api/command", post(command))
        .route("/api/collection", get(collection))
        .route("/api/collection/edit", post(edit))
        .route("/api/collection/add", post(add))
        .route("/api/printing", get(printing))
        .route("/api/printings", get(printings))
        .route("/api/decks", get(decks))
        .route("/api/decks/{id}", get(deck))
        .route("/api/settings", get(settings::get).post(settings::change))
        .route("/api/settings/database", get(settings::database))
        .route("/api/setup", get(setup::get))
        .route("/api/setup/ollama", get(setup::ollama))
        .route("/api/setup/recognition", post(setup::recognition))
        .route("/api/setup/pull", get(setup::pull).post(setup::start_pull))
        .route("/api/setup/benchmark", post(setup::benchmark))
        .route(
            "/api/setup/telemetry",
            get(setup::telemetry_view).post(setup::telemetry),
        )
        .route("/api/setup/done", post(setup::done))
        .fallback(static_file)
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::guard,
        ))
        .with_state(state);

    let stop = async move {
        let mut check = tokio::time::interval(Duration::from_millis(500));
        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    println!("Stopping…");
                    break;
                }
                _ = check.tick() => {
                    if !sessions.is_running() {
                        println!("The scanner stopped; shutting down the web UI.");
                        break;
                    }
                }
            }
        }
        let _ = shutdown_tx.send(true);
    };
    // The peer address counts failed sign-ins per address (see `accounts`).
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(stop)
    .await
    .context("web server failed")
}

/// Resolves once shutdown starts (for ending long-lived streams).
async fn on_shutdown(mut rx: watch::Receiver<bool>) {
    let _ = rx.wait_for(|stop| *stop).await;
}

async fn health(State(st): State<Arc<AppState>>) -> Response {
    if st.sessions.is_running() {
        format!("ok: mtg-scan {}", env!("CARGO_PKG_VERSION")).into_response()
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "the scanner stopped").into_response()
    }
}

/// The signed-in account's scan session (started on first use, which can
/// take a moment: the owner's opens the server's camera).
async fn session_of(
    st: &Arc<AppState>,
    signed: Signed,
) -> Result<Arc<Engine>, (StatusCode, String)> {
    let sessions = st.sessions.clone();
    match tokio::task::spawn_blocking(move || sessions.of(&signed.user)).await {
        Ok(Ok(engine)) => Ok(engine),
        Ok(Err(e)) => Err((StatusCode::SERVICE_UNAVAILABLE, format!("{e:#}"))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    }
}

async fn session(
    State(st): State<Arc<AppState>>,
    Extension(signed): Extension<Signed>,
) -> Response {
    match session_of(&st, signed).await {
        Ok(engine) => Json::<SessionView>(engine.session()).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn command(
    State(st): State<Arc<AppState>>,
    Extension(signed): Extension<Signed>,
    Json(cmd): Json<ClientCommand>,
) -> Response {
    match session_of(&st, signed).await {
        Ok(engine) => {
            engine.send(cmd.into());
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => e.into_response(),
    }
}

async fn events(State(st): State<Arc<AppState>>, Extension(signed): Extension<Signed>) -> Response {
    match session_of(&st, signed).await {
        Ok(engine) => Sse::new(event_stream(&st, engine))
            .keep_alive(KeepAlive::default())
            .into_response(),
        Err(e) => e.into_response(),
    }
}

fn event_stream(
    st: &AppState,
    engine: Arc<Engine>,
) -> impl Stream<Item = Result<Event, Infallible>> + use<> {
    // Start with the current state, so the browser knows it is connected
    // (and can resync) without waiting for something to change.
    let first = futures_util::stream::iter([ServerEvent::State(engine.view())]);
    let updates = BroadcastStream::new(engine.subscribe())
        // A slow client that fell behind just misses some events.
        .filter_map(move |e| {
            let engine = engine.clone();
            async move { to_server_event(&engine, e.ok()?) }
        });
    first
        .chain(updates)
        .map(|ev| Ok(Event::default().json_data(ev).unwrap_or_default()))
        .take_until(on_shutdown(st.shutdown.clone()))
}

fn to_server_event(engine: &Engine, e: EngineEvent) -> Option<ServerEvent> {
    Some(match e {
        EngineEvent::State => ServerEvent::State(engine.view()),
        EngineEvent::Card(card) => ServerEvent::Card(*card),
        EngineEvent::Undone { seqs, .. } => ServerEvent::Undone { seqs },
        EngineEvent::Cleared => ServerEvent::Cleared,
        EngineEvent::Ready(message) | EngineEvent::Info(message) => ServerEvent::Info {
            message,
            error: false,
        },
        EngineEvent::Error(message) => ServerEvent::Info {
            message,
            error: true,
        },
        // Cards arrive individually as `Card`; state carries the summary.
        EngineEvent::Results { .. } => return None,
    })
}

/// `multipart/x-mixed-replace` stream of JPEG previews: an `<img>` tag
/// pointed at it shows live video without any JavaScript.
async fn stream(State(st): State<Arc<AppState>>, Extension(signed): Extension<Signed>) -> Response {
    let engine = match session_of(&st, signed).await {
        Ok(engine) => engine,
        Err(e) => return e.into_response(),
    };
    let frames =
        futures_util::stream::unfold((engine.preview(), 0u64), |(mut rx, last)| async move {
            if last != 0 {
                tokio::time::sleep(STREAM_FRAME_INTERVAL).await;
            }
            loop {
                let latest = rx.borrow_and_update().clone();
                match latest {
                    Some(p) if p.id != last => {
                        let id = p.id;
                        let jpeg = tokio::task::spawn_blocking(move || p.jpeg().to_vec())
                            .await
                            .ok()?;
                        let mut part = format!(
                            "--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
                            jpeg.len()
                        )
                        .into_bytes();
                        part.extend_from_slice(&jpeg);
                        part.extend_from_slice(b"\r\n");
                        return Some((Ok::<_, Infallible>(Bytes::from(part)), (rx, id)));
                    }
                    _ => rx.changed().await.ok()?,
                }
            }
        })
        .take_until(on_shutdown(st.shutdown.clone()));

    Response::builder()
        .header(
            header::CONTENT_TYPE,
            "multipart/x-mixed-replace; boundary=frame",
        )
        .header(header::CACHE_CONTROL, "no-cache, no-store")
        .body(Body::from_stream(frames))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// Run blocking work (the store and Scryfall clients are blocking) off the
/// async threads. Errors become `error_status` with the message as text.
async fn blocking<T: serde::Serialize + Send + 'static>(
    error_status: StatusCode,
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Response {
    match tokio::task::spawn_blocking(work).await {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(e)) => (error_status, format!("{e:#}")).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn collection(
    State(st): State<Arc<AppState>>,
    Extension(signed): Extension<Signed>,
) -> Response {
    let cfg = st.cfg.clone();
    blocking(StatusCode::INTERNAL_SERVER_ERROR, move || {
        collection_view(&cfg, signed.user.id)
    })
    .await
}

#[derive(serde::Deserialize)]
struct PrintingQuery {
    q: String,
}

async fn printing(Query(query): Query<PrintingQuery>) -> Response {
    blocking(StatusCode::BAD_REQUEST, move || find_printing(&query.q)).await
}

const DRY_RUN: &str =
    "Dry run: the collection can't be changed. Restart without --dry-run to edit it.";

async fn edit(
    State(st): State<Arc<AppState>>,
    Extension(signed): Extension<Signed>,
    Json(edit): Json<RowEdit>,
) -> Response {
    if st.sessions.dry_run() {
        return (StatusCode::CONFLICT, DRY_RUN).into_response();
    }
    let cfg = st.cfg.clone();
    blocking(StatusCode::BAD_REQUEST, move || {
        edit_row(&mut App::for_user(&cfg, signed.user.id)?, edit)
    })
    .await
}

async fn add(
    State(st): State<Arc<AppState>>,
    Extension(signed): Extension<Signed>,
    Json(add): Json<AddCard>,
) -> Response {
    if st.sessions.dry_run() {
        return (StatusCode::CONFLICT, DRY_RUN).into_response();
    }
    let cfg = st.cfg.clone();
    blocking(StatusCode::BAD_REQUEST, move || {
        add_card(&mut App::for_user(&cfg, signed.user.id)?, add)
    })
    .await
}

/// A Scryfall language code: 2 or 3 lowercase letters (`en`, `ja`, `zhs`).
fn check_language(code: &str) -> Result<()> {
    if !(2..=3).contains(&code.len()) || !code.bytes().all(|b| b.is_ascii_lowercase()) {
        bail!("\"{code}\" isn't a language code like en or ja");
    }
    Ok(())
}

/// Add copies of a printing by hand, priced now; merged into the row with
/// that printing, finish, condition and language when there is one.
fn add_card(app: &mut App, add: AddCard) -> Result<EditResult> {
    if add.quantity == 0 {
        bail!("Add at least one copy");
    }
    let finish: Finish = add.finish.into();
    let card = app
        .scryfall
        .by_id(&add.scryfall_id)?
        .ok_or_else(|| anyhow!("Scryfall doesn't know the printing {}", add.scryfall_id))?;
    if !card.finishes.is_empty() && !card.finishes.contains(&finish) {
        bail!(
            "{} ({} #{}) isn't printed in {finish}",
            card.name,
            card.set_code.to_uppercase(),
            card.collector_number
        );
    }
    let language = add.language.unwrap_or_else(|| card.lang.clone());
    check_language(&language)?;
    let prices = app.prices.price(&card, finish);
    let row = app.store.add(CollectionEntry {
        condition: add.condition,
        language,
        ..CollectionEntry::from_card(&card, finish, add.quantity, prices)
    })?;
    let now = if row.quantity > add.quantity {
        format!(" ({} owned now)", row.quantity)
    } else {
        String::new()
    };
    Ok(EditResult {
        message: format!(
            "Added {}× {} ({} #{}, {}){now}",
            add.quantity,
            row.name,
            row.set_code,
            row.collector_number,
            copies(row.finish, row.condition, &row.language)
        ),
        row: Some(row_with_details(app, row)),
    })
}

#[derive(serde::Deserialize)]
struct DecksQuery {
    /// Ask Moxfield now instead of using the list fetched in the last minutes.
    #[serde(default)]
    refresh: bool,
    /// Include decks hidden on the Settings page (for that page's list).
    #[serde(default)]
    all: bool,
}

async fn decks(
    State(st): State<Arc<AppState>>,
    Extension(signed): Extension<Signed>,
    Query(query): Query<DecksQuery>,
) -> Response {
    let state = st.clone();
    blocking(StatusCode::INTERNAL_SERVER_ERROR, move || {
        let prefs = settings::prefs(&state, &signed)?;
        let Some(user) = prefs.moxfield_user() else {
            return Ok(DecksView {
                user: None,
                profile_url: None,
                decks: Vec::new(),
                warning: None,
            });
        };
        let mut view = Moxfield::new(app::http_client()?).decks(&user, query.refresh);
        if !query.all {
            view.decks.retain(|d| !prefs.is_hidden(&d.id));
        }
        Ok(view)
    })
    .await
}

/// Only decks of the signed-in account's Moxfield user are served, so this
/// isn't a general Moxfield proxy.
async fn deck(
    State(st): State<Arc<AppState>>,
    Extension(signed): Extension<Signed>,
    Path(id): Path<String>,
) -> Response {
    let state = st.clone();
    blocking(StatusCode::NOT_FOUND, move || {
        let user = settings::prefs(&state, &signed)?
            .moxfield_user()
            .ok_or_else(|| anyhow!("No Moxfield account is set (see Settings)."))?;
        let client = Moxfield::new(app::http_client()?);
        let summary = client
            .decks(&user, false)
            .decks
            .into_iter()
            .find(|d| d.id == id)
            .ok_or_else(|| anyhow!("{user} has no public deck {id}"))?;
        client.deck(&summary)
    })
    .await
}

#[derive(serde::Deserialize)]
struct PrintingsQuery {
    name: String,
}

async fn printings(Query(query): Query<PrintingsQuery>) -> Response {
    blocking(StatusCode::BAD_REQUEST, move || {
        let scryfall = Scryfall::new(app::http_client()?);
        let cards = scryfall.printings(&query.name)?;
        if cards.is_empty() {
            bail!("Scryfall has no printings of {}", query.name);
        }
        Ok(cards.iter().map(printing_view).collect::<Vec<_>>())
    })
    .await
}

fn find_printing(link: &str) -> Result<PrintingView> {
    let Some(card_ref) = scryfall::parse_card_ref(link) else {
        bail!("Paste a Scryfall card link, e.g. https://scryfall.com/card/fdn/123/...");
    };
    let scryfall = Scryfall::new(app::http_client()?);
    let card = scryfall
        .by_ref(&card_ref)?
        .ok_or_else(|| anyhow!("Scryfall has no card at that link"))?;
    Ok(printing_view(&card))
}

fn printing_view(card: &Card) -> PrintingView {
    PrintingView {
        scryfall_url: format!(
            "https://scryfall.com/card/{}/{}",
            card.set_code.to_lowercase(),
            card.collector_number
        ),
        scryfall_id: card.scryfall_id.clone(),
        lang: card.lang.clone(),
        name: card.name.clone(),
        set_code: card.set_code.to_uppercase(),
        set_name: card.set_name.clone(),
        number: card.collector_number.clone(),
        rarity: card.rarity.clone(),
        finishes: card.finishes.iter().map(|&f| f.into()).collect(),
        image_url: card
            .image_small
            .as_ref()
            .map(|u| u.replace("/small/", "/normal/")),
    }
}

/// Apply a [`RowEdit`]: remove, set the quantity, or change the row's
/// printing, finish, condition or language (priced afresh, merged into a row
/// that already exists).
fn edit_row(app: &mut App, edit: RowEdit) -> Result<EditResult> {
    const GONE: &str = "That row is no longer in the collection. Refresh the page.";
    let finish: Finish = edit.finish.into();
    let to_finish: Finish = edit.to_finish.into();
    let key = RowKey {
        scryfall_id: edit.scryfall_id.clone(),
        finish,
        condition: edit.condition,
        language: edit.language.clone(),
    };
    check_language(&edit.to_language)?;
    let described = copies(finish, edit.condition, &edit.language);

    if edit.quantity == 0 {
        let gone = app
            .store
            .set_quantity(&key, 0)?
            .ok_or_else(|| anyhow!(GONE))?;
        return Ok(EditResult {
            message: format!("Removed {} ({described}) from the collection", gone.name),
            row: None,
        });
    }

    if edit.to_scryfall_id == edit.scryfall_id
        && to_finish == finish
        && edit.to_condition == edit.condition
        && edit.to_language == edit.language
    {
        let row = app
            .store
            .set_quantity(&key, edit.quantity)?
            .ok_or_else(|| anyhow!(GONE))?;
        return Ok(EditResult {
            message: format!("{} ({described}): {} owned", row.name, row.quantity),
            row: Some(row_with_details(app, row)),
        });
    }

    let card = app
        .scryfall
        .by_id(&edit.to_scryfall_id)?
        .ok_or_else(|| anyhow!("Scryfall doesn't know the printing {}", edit.to_scryfall_id))?;
    if !card.finishes.is_empty() && !card.finishes.contains(&to_finish) {
        bail!(
            "{} ({} #{}) isn't printed in {to_finish}",
            card.name,
            card.set_code.to_uppercase(),
            card.collector_number
        );
    }
    let prices = app.prices.price(&card, to_finish);
    let to = CollectionEntry {
        condition: edit.to_condition,
        language: edit.to_language.clone(),
        ..CollectionEntry::from_card(&card, to_finish, edit.quantity, prices)
    };
    let row = app.store.relocate(&key, to)?.ok_or_else(|| anyhow!(GONE))?;
    let merged = if row.quantity > edit.quantity {
        format!(
            ", merged with the copies already there ({} owned)",
            row.quantity
        )
    } else {
        String::new()
    };
    Ok(EditResult {
        message: format!(
            "Changed to {} ({} #{}, {}){merged}",
            row.name,
            row.set_code,
            row.collector_number,
            copies(row.finish, row.condition, &row.language)
        ),
        row: Some(row_with_details(app, row)),
    })
}

/// Look up colors and type line for entries that don't carry them (the
/// Markdown store doesn't keep them). Degrades to "unknown" if Scryfall
/// can't be reached.
fn fill_details(app: &App, entries: &mut [CollectionEntry]) {
    let missing: Vec<String> = entries
        .iter()
        .filter(|e| e.type_line.is_empty())
        .map(|e| e.scryfall_id.clone())
        .collect();
    if missing.is_empty() {
        return;
    }
    let cards = match app.scryfall.cards_by_id(&missing) {
        Ok(cards) => cards,
        Err(e) => {
            eprintln!("  ! could not fetch card details from Scryfall ({e:#})");
            return;
        }
    };
    for e in entries.iter_mut().filter(|e| e.type_line.is_empty()) {
        if let Some(card) = cards.get(&e.scryfall_id) {
            e.colors = card.colors.clone();
            e.type_line = card.type_line.clone();
        }
    }
}

fn row_with_details(app: &App, mut entry: CollectionEntry) -> CollectionRow {
    fill_details(app, std::slice::from_mut(&mut entry));
    collection_row(entry)
}

fn collection_row(e: CollectionEntry) -> CollectionRow {
    CollectionRow {
        scryfall_url: e.scryfall_url(),
        scryfall_id: e.scryfall_id,
        name: e.name,
        set_code: e.set_code,
        set_name: e.set_name,
        number: e.collector_number,
        rarity: e.rarity,
        colors: e.colors,
        type_line: e.type_line,
        finish: e.finish.into(),
        condition: e.condition,
        language: e.language,
        quantity: e.quantity,
        tcgplayer: e.prices.tcgplayer,
        cardmarket: e.prices.cardmarket,
        cardkingdom: e.prices.cardkingdom,
        cardkingdom_buylist: e.prices.cardkingdom_buylist,
        added: e.added.to_string(),
        priced: e.price_updated.map(|d| d.to_string()),
        tcgplayer_url: e.prices.tcgplayer_url,
        cardkingdom_url: e.prices.cardkingdom_url,
    }
}

fn collection_view(cfg: &Config, user_id: i64) -> Result<CollectionView> {
    let app = App::for_user(cfg, user_id)?;
    let mut entries = app.store.entries()?;
    fill_details(&app, &mut entries);
    let t = crate::store::totals(&entries);
    let since = chrono::Local::now().date_naive() - chrono::Days::new(7);
    let value_change = match app.store.value_change(since) {
        Ok(change) => change.map(|c| ValueChange {
            since: since.to_string(),
            before: c.before,
            now: c.now,
            rows: c.rows,
        }),
        Err(e) => {
            eprintln!("  ! could not compute the value change ({e:#})");
            None
        }
    };
    Ok(CollectionView {
        location: app.store.location(),
        totals: Totals {
            printings: t.printings,
            cards: t.cards,
            tcgplayer: t.tcgplayer,
            cardmarket: t.cardmarket,
            cardkingdom: t.cardkingdom,
            cardkingdom_buylist: t.cardkingdom_buylist,
        },
        rows: entries.into_iter().map(collection_row).collect(),
        value_change,
        card_kingdom: cfg.prices.card_kingdom,
    })
}

async fn static_file(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    // An unknown API call must not get the app's HTML: the UI would fail to parse
    // it as JSON. It happens when the embedded UI is newer than the server.
    if path == "api" || path.starts_with("api/") {
        return (
            StatusCode::NOT_FOUND,
            format!(
                "This server has no /{path} request. The web UI and the server may come \
                 from different builds: run trunk build --release in ui, then cargo build \
                 --release."
            ),
        )
            .into_response();
    }
    // Unknown paths without an extension get the app, so reloading any page works.
    let found = match Assets::get(path) {
        Some(f) => Some((path, f)),
        None if !path.contains('.') => Assets::get("index.html").map(|f| ("index.html", f)),
        None => None,
    };
    match found {
        Some((served, f)) => {
            let mime = mime_guess::from_path(served).first_or_octet_stream();
            // Trunk fingerprints asset names, so only index.html must stay fresh.
            let cache = if served == "index.html" {
                "no-cache"
            } else {
                "public, max-age=31536000, immutable"
            };
            (
                [
                    (header::CONTENT_TYPE, mime.as_ref().to_string()),
                    (header::CACHE_CONTROL, cache.to_string()),
                ],
                f.data,
            )
                .into_response()
        }
        None if path == "index.html" => (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            "<h1>mtg-scan</h1><p>The web UI hasn't been built into this binary. Run \
             <code>trunk build --release</code> in the <code>ui</code> folder, then \
             <code>cargo build --release</code>.</p>",
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// This computer's address on the local network (no packets are sent).
fn lan_ip() -> Option<IpAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:80").ok()?;
    socket.local_addr().ok().map(|a| a.ip())
}

fn open_browser(url: &str) {
    #[cfg(windows)]
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    let _ = url;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_ui_build_explains_how_to_build() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let resp = rt.block_on(static_file(Uri::from_static("/")));
        // Either the embedded UI or the build instructions, never a bare 404.
        assert!(
            resp.status() == StatusCode::OK || resp.status() == StatusCode::SERVICE_UNAVAILABLE
        );
        let missing = rt.block_on(static_file(Uri::from_static("/nope.js")));
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn unknown_api_call_is_not_found_not_the_app() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let text = |uri: &'static str| {
            rt.block_on(async {
                let resp = static_file(Uri::from_static(uri)).await;
                let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await;
                String::from_utf8_lossy(&body.unwrap()).into_owned()
            })
        };
        for uri in ["/api/nope", "/api/decks/x/y", "/api"] {
            let resp = rt.block_on(static_file(Uri::from_static(uri)));
            assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{uri}");
            assert!(text(uri).contains("different builds"), "{uri}");
        }
        // Other paths are client-side pages (the app, or 404 when no UI is built).
        assert!(!text("/apis").contains("different builds"));
    }
}
