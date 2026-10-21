//! Web front end: `mtg-scan ui` serves the Leptos app (built by Trunk from
//! `ui/` and embedded in the binary) plus a small API, reachable from other
//! computers on the network.
//!
//! | Route | |
//! |---|---|
//! | `GET /api/session` | scanner state + this session's cards |
//! | `GET /api/events` | Server-Sent Events: state changes, new cards, messages |
//! | `GET /api/stream.mjpg` | live camera preview (MJPEG) |
//! | `POST /api/command` | `ClientCommand` JSON: scan now, undo, set mode, … |
//! | `GET /api/collection` | the whole collection with prices and totals |
//! | everything else | the embedded UI |

use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{StatusCode, Uri, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::{Stream, StreamExt};
use mtg_scan_shared::{
    ClientCommand, CollectionRow, CollectionView, ServerEvent, SessionView, Totals,
};
use tokio::sync::watch;
use tokio_stream::wrappers::BroadcastStream;

use crate::app::App;
use crate::config::Config;
use crate::engine::{self, Engine, EngineEvent, EngineOptions};
use crate::live;

/// Cap the preview stream at ~15 frames per second per viewer.
const STREAM_FRAME_INTERVAL: Duration = Duration::from_millis(66);

pub struct WebOptions {
    pub addr: SocketAddr,
    pub open_browser: bool,
}

/// The UI files built by `trunk build --release` in `ui/`.
#[derive(rust_embed::Embed)]
#[folder = "ui/dist/"]
struct Assets;

struct AppState {
    engine: Arc<Engine>,
    cfg: Config,
    /// Flips to true on shutdown so long-lived streams end.
    shutdown: watch::Receiver<bool>,
}

pub fn run(cfg: &Config, opts: EngineOptions, web: WebOptions) -> Result<()> {
    if opts.dry_run {
        println!("Dry run: cards are identified and priced but not saved.");
    }
    let engine = Arc::new(engine::start(cfg, opts)?);
    println!("Camera: {}", engine.view().camera);
    let logger = live::spawn_console_logger(&engine);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to start the async runtime")?;
    let served = runtime.block_on(serve(cfg.clone(), engine.clone(), web));
    // Dropping the runtime ends every connection, releasing their engine handles.
    drop(runtime);

    let engine = Arc::try_unwrap(engine).map_err(|_| anyhow!("the scanner is still in use"))?;
    let stopped = engine.shutdown();
    let _ = logger.join();
    served.and(stopped)
}

async fn serve(cfg: Config, engine: Arc<Engine>, web: WebOptions) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(web.addr)
        .await
        .with_context(|| format!("could not listen on {} (port in use?)", web.addr))?;
    let port = web.addr.port();
    let local = format!("http://localhost:{port}");
    println!("\nWeb UI: {local}");
    if web.addr.ip().is_unspecified() {
        if let Some(ip) = lan_ip() {
            println!("   on other computers: http://{ip}:{port}");
        }
        println!(
            "   (Windows may ask whether to allow mtg-scan on the network: allow Private networks.)"
        );
    }
    println!("Press Ctrl+C to stop.\n");
    if web.open_browser {
        open_browser(&local);
    }

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let state = Arc::new(AppState {
        engine: engine.clone(),
        cfg,
        shutdown: shutdown_rx,
    });
    let app = Router::new()
        .route("/api/session", get(session))
        .route("/api/events", get(events))
        .route("/api/stream.mjpg", get(stream))
        .route("/api/command", post(command))
        .route("/api/collection", get(collection))
        .fallback(static_file)
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
                    if !engine.is_running() {
                        println!("The scanner stopped; shutting down the web UI.");
                        break;
                    }
                }
            }
        }
        let _ = shutdown_tx.send(true);
    };
    axum::serve(listener, app)
        .with_graceful_shutdown(stop)
        .await
        .context("web server failed")
}

/// Resolves once shutdown starts (for ending long-lived streams).
async fn on_shutdown(mut rx: watch::Receiver<bool>) {
    let _ = rx.wait_for(|stop| *stop).await;
}

async fn session(State(st): State<Arc<AppState>>) -> Json<SessionView> {
    Json(st.engine.session())
}

async fn command(State(st): State<Arc<AppState>>, Json(cmd): Json<ClientCommand>) -> StatusCode {
    st.engine.send(cmd.into());
    StatusCode::NO_CONTENT
}

async fn events(
    State(st): State<Arc<AppState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let engine = st.engine.clone();
    // Start with the current state, so the browser knows it is connected
    // (and can resync) without waiting for something to change.
    let first = futures_util::stream::iter([ServerEvent::State(st.engine.view())]);
    let updates = BroadcastStream::new(st.engine.subscribe())
        // A slow client that fell behind just misses some events.
        .filter_map(move |e| {
            let engine = engine.clone();
            async move { to_server_event(&engine, e.ok()?) }
        });
    let stream = first
        .chain(updates)
        .map(|ev| Ok(Event::default().json_data(ev).unwrap_or_default()))
        .take_until(on_shutdown(st.shutdown.clone()));
    Sse::new(stream).keep_alive(KeepAlive::default())
}

fn to_server_event(engine: &Engine, e: EngineEvent) -> Option<ServerEvent> {
    Some(match e {
        EngineEvent::State => ServerEvent::State(engine.view()),
        EngineEvent::Card(card) => ServerEvent::Card(*card),
        EngineEvent::Undone { seqs, .. } => ServerEvent::Undone { seqs },
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
async fn stream(State(st): State<Arc<AppState>>) -> Response {
    let frames =
        futures_util::stream::unfold((st.engine.preview(), 0u64), |(mut rx, last)| async move {
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

async fn collection(State(st): State<Arc<AppState>>) -> Response {
    let cfg = st.cfg.clone();
    // The store and Scryfall clients are blocking; keep them off the async threads.
    let result = tokio::task::spawn_blocking(move || collection_view(&cfg)).await;
    match result {
        Ok(Ok(view)) => Json(view).into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

fn collection_view(cfg: &Config) -> Result<CollectionView> {
    let app = App::new(cfg)?;
    let entries = app.store.entries()?;
    let t = crate::store::totals(&entries);
    Ok(CollectionView {
        location: app.store.location(),
        totals: Totals {
            printings: t.printings,
            cards: t.cards,
            tcgplayer: t.tcgplayer,
            cardkingdom: t.cardkingdom,
            cardkingdom_buylist: t.cardkingdom_buylist,
        },
        rows: entries
            .into_iter()
            .map(|e| CollectionRow {
                scryfall_url: e.scryfall_url(),
                scryfall_id: e.scryfall_id,
                name: e.name,
                set_code: e.set_code,
                set_name: e.set_name,
                number: e.collector_number,
                rarity: e.rarity,
                finish: e.finish.into(),
                quantity: e.quantity,
                tcgplayer: e.prices.tcgplayer,
                cardkingdom: e.prices.cardkingdom,
                cardkingdom_buylist: e.prices.cardkingdom_buylist,
                priced: e.price_updated.map(|d| d.to_string()),
                tcgplayer_url: e.prices.tcgplayer_url,
                cardkingdom_url: e.prices.cardkingdom_url,
            })
            .collect(),
    })
}

async fn static_file(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
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
    #[cfg(not(windows))]
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
}
