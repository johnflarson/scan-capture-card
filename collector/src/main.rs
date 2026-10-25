//! `tome-collector`: receives The Shuffling Tome's opt-in usage statistics
//! and shows them, with the project's GitHub numbers, on a password-protected
//! metrics page. Meant for one small server behind a reverse proxy; see
//! `collector/README.md`.
//!
//! | Route (under `TOME_COLLECTOR_BASE_PATH`) | |
//! |---|---|
//! | `POST /v1/ping` | a server's daily ping (`mtg_scan_shared::Ping`); 204, or 400 with the reason |
//! | `GET /` | the metrics page (HTTP Basic sign-in, any user name, `TOME_COLLECTOR_PASSWORD`) |
//! | `GET /api/metrics` | the same numbers as JSON (same sign-in) |
//! | `GET /health` | "ok" (`tome-collector health` asks it, for container health checks) |
//!
//! IP addresses are only used, in memory, to limit how often one address
//! may send pings or try the password; they are never stored.

mod github;
mod limit;
mod metrics;
mod page;
mod store;

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::{Context, Result};
use axum::body::Bytes;
use axum::extract::{ConnectInfo, DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use chrono::Utc;
use mtg_scan_shared::Ping;
use sha2::{Digest, Sha256};

use limit::Limiter;
use store::Store;

/// How often GitHub's numbers are fetched.
const GITHUB_EVERY: Duration = Duration::from_secs(6 * 3600);
/// Pings are kept this long.
const KEEP_DAYS: i64 = 400;
/// A ping is a few hundred bytes.
const MAX_BODY: usize = 4096;

struct Config {
    bind: SocketAddr,
    db: PathBuf,
    /// Where the proxy puts the collector, e.g. `/app/shufflingtome` (empty
    /// when the proxy strips it).
    base: String,
    /// SHA-256 of the metrics page's password; no page without one.
    password: Option<[u8; 32]>,
    repo: String,
    sponsor: String,
    github_token: Option<String>,
    /// Take the client's address from `X-Forwarded-For` / `X-Real-IP` (behind a proxy).
    trust_proxy: bool,
}

fn env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

impl Config {
    fn from_env() -> Result<Self> {
        let base = env("TOME_COLLECTOR_BASE_PATH")
            .unwrap_or_default()
            .trim_end_matches('/')
            .to_string();
        let base = if base.is_empty() || base.starts_with('/') {
            base
        } else {
            format!("/{base}")
        };
        let repo =
            env("TOME_COLLECTOR_REPO").unwrap_or_else(|| "johnflarson/scan-capture-card".into());
        Ok(Self {
            bind: env("TOME_COLLECTOR_BIND")
                .unwrap_or_else(|| "127.0.0.1:8790".into())
                .parse()
                .context("TOME_COLLECTOR_BIND must be an address:port, e.g. 0.0.0.0:8790")?,
            db: env("TOME_COLLECTOR_DB")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("collector.db")),
            base,
            password: env("TOME_COLLECTOR_PASSWORD").map(|p| Sha256::digest(p.as_bytes()).into()),
            sponsor: env("TOME_COLLECTOR_SPONSOR")
                .unwrap_or_else(|| repo.split('/').next().unwrap_or_default().to_string()),
            repo,
            github_token: env("GITHUB_TOKEN"),
            trust_proxy: env("TOME_COLLECTOR_TRUST_PROXY").is_none_or(|v| v != "false" && v != "0"),
        })
    }
}

struct App {
    cfg: Config,
    store: Mutex<Store>,
    /// Pings per address: 10 an hour.
    pings: Limiter,
    /// Wrong passwords per address: 10 in 15 minutes.
    sign_ins: Limiter,
    /// The last GitHub fetch: when, and what failed.
    github: Mutex<(Option<String>, Vec<String>)>,
}

impl App {
    fn store(&self) -> MutexGuard<'_, Store> {
        self.store.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The client's address: the proxy's header when trusted, else the peer.
    fn client(&self, headers: &HeaderMap, peer: SocketAddr) -> IpAddr {
        if self.cfg.trust_proxy {
            let forwarded = headers
                .get("x-forwarded-for")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.split(',').next())
                .or_else(|| headers.get("x-real-ip").and_then(|v| v.to_str().ok()))
                .and_then(|v| v.trim().parse().ok());
            if let Some(ip) = forwarded {
                return ip;
            }
        }
        peer.ip()
    }
}

#[tokio::main]
async fn main() {
    // reqwest is built without a TLS provider; ring is it. Before any client.
    let _ = rustls::crypto::ring::default_provider().install_default();
    // `tome-collector health`: the container's health check.
    let result = if std::env::args().nth(1).as_deref() == Some("health") {
        health().await
    } else {
        run().await
    };
    if let Err(e) = result {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

/// Exit 0 when this computer's collector answers `/health`.
async fn health() -> Result<()> {
    let cfg = Config::from_env()?;
    let port = cfg.bind.port();
    let url = format!("http://127.0.0.1:{port}{}/health", cfg.base);
    reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?
        .get(&url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .with_context(|| format!("{url} doesn't answer"))?;
    println!("ok");
    Ok(())
}

async fn run() -> Result<()> {
    let cfg = Config::from_env()?;
    let store = Store::open(&cfg.db)?;
    if cfg.password.is_none() {
        eprintln!(
            "TOME_COLLECTOR_PASSWORD isn't set: pings are received, the metrics page is off."
        );
    }
    let bind = cfg.bind;
    let app = Arc::new(App {
        cfg,
        store: Mutex::new(store),
        pings: Limiter::new(10, Duration::from_secs(3600)),
        sign_ins: Limiter::new(10, Duration::from_secs(15 * 60)),
        github: Mutex::new((None, Vec::new())),
    });
    tokio::spawn(refresh_github(app.clone()));

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("could not listen on {bind}"))?;
    println!(
        "tome-collector {} on http://{bind}{}/",
        env!("CARGO_PKG_VERSION"),
        app.cfg.base
    );
    axum::serve(
        listener,
        router(app).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await?;
    Ok(())
}

fn router(app: Arc<App>) -> Router {
    let base = app.cfg.base.clone();
    let mut router = Router::new()
        .route(&format!("{base}/v1/ping"), post(ping))
        .route(&format!("{base}/"), get(metrics_page))
        .route(&format!("{base}/api/metrics"), get(metrics_json))
        .route(&format!("{base}/health"), get(|| async { "ok" }));
    if !base.is_empty() {
        // Relative links on the page need the trailing slash.
        let to = format!("{base}/");
        router = router.route(&base, get(move || async move { Redirect::permanent(&to) }));
    }
    router
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .with_state(app)
}

fn today() -> String {
    Utc::now().format("%Y-%m-%d").to_string()
}

async fn ping(
    State(app): State<Arc<App>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !app.pings.allow(app.client(&headers, peer)) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "too many pings from this address",
        )
            .into_response();
    }
    let ping: Ping = match serde_json::from_slice(&body) {
        Ok(p) => p,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("not a ping: {e}")).into_response(),
    };
    if let Some(problem) = ping.problem() {
        return (StatusCode::BAD_REQUEST, problem).into_response();
    }
    let now = Utc::now().to_rfc3339();
    match app.store().record(&today(), &ping, &now) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            eprintln!("could not store a ping: {e:#}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Why the request may not see the metrics, if it may not: it needs the
/// password (HTTP Basic, any user name).
fn refusal(app: &App, headers: &HeaderMap, peer: SocketAddr) -> Option<Response> {
    let Some(want) = app.cfg.password else {
        return Some(
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "The metrics page is off: set TOME_COLLECTOR_PASSWORD.",
            )
                .into_response(),
        );
    };
    let ip = app.client(headers, peer);
    if app.sign_ins.blocked(ip) {
        return Some(
            (
                StatusCode::TOO_MANY_REQUESTS,
                "Too many wrong passwords; try later.",
            )
                .into_response(),
        );
    }
    let given = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Basic "))
        .and_then(|v| {
            base64::engine::general_purpose::STANDARD
                .decode(v.trim())
                .ok()
        })
        .and_then(|v| String::from_utf8(v).ok())
        .and_then(|v| v.split_once(':').map(|(_, p)| p.to_string()));
    if let Some(password) = &given {
        let hash: [u8; 32] = Sha256::digest(password.as_bytes()).into();
        // Comparing digests keeps the time independent of the password.
        if hash == want {
            return None;
        }
        app.sign_ins.allow(ip);
    }
    Some(
        (
            StatusCode::UNAUTHORIZED,
            [(
                header::WWW_AUTHENTICATE,
                "Basic realm=\"Tome metrics\", charset=\"UTF-8\"",
            )],
            "Sign in with the collector's password.",
        )
            .into_response(),
    )
}

fn current_metrics(app: &App) -> Result<metrics::Metrics> {
    let today = Utc::now().date_naive();
    let store = app.store();
    let mut github = metrics::github(&store, today)?;
    let (updated, problems) = app.github.lock().unwrap_or_else(|e| e.into_inner()).clone();
    github.updated = updated;
    github.problems = problems;
    metrics::compute(&store, today, github)
}

async fn metrics_page(
    State(app): State<Arc<App>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if let Some(r) = refusal(&app, &headers, peer) {
        return r;
    }
    match current_metrics(&app) {
        Ok(m) => (
            [(header::CACHE_CONTROL, "no-store")],
            Html(page::render(&m)),
        )
            .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response(),
    }
}

async fn metrics_json(
    State(app): State<Arc<App>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if let Some(r) = refusal(&app, &headers, peer) {
        return r;
    }
    match current_metrics(&app) {
        Ok(m) => Json(m).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response(),
    }
}

/// Fetch GitHub's numbers now and every six hours; drop old pings.
async fn refresh_github(app: Arc<App>) {
    let source = match github::Source::new(
        app.cfg.repo.clone(),
        app.cfg.sponsor.clone(),
        app.cfg.github_token.clone(),
    ) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("GitHub numbers are off: {e:#}");
            return;
        }
    };
    loop {
        let day = today();
        let snap = source.fetch(&day).await;
        {
            let store = app.store();
            let mut stored = Ok(());
            for (d, metric, value) in &snap.daily {
                stored = stored.and_then(|()| store.set_github(d, metric, *value));
            }
            stored = stored.and_then(|()| store.set_releases(&snap.releases));
            let cutoff = (Utc::now() - chrono::Duration::days(KEEP_DAYS))
                .format("%Y-%m-%d")
                .to_string();
            stored = stored.and_then(|()| store.prune(&cutoff).map(|_| ()));
            if let Err(e) = stored {
                eprintln!("could not store GitHub numbers: {e:#}");
            }
        }
        for p in &snap.problems {
            eprintln!("GitHub: {p}");
        }
        *app.github.lock().unwrap_or_else(|e| e.into_inner()) = (
            Some(Utc::now().format("%Y-%m-%d %H:%M").to_string()),
            snap.problems,
        );
        tokio::time::sleep(GITHUB_EVERY).await;
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    use super::*;
    use crate::store::tests::ping;

    fn app(base: &str, password: Option<&str>) -> Arc<App> {
        Arc::new(App {
            cfg: Config {
                bind: "127.0.0.1:0".parse().unwrap(),
                db: PathBuf::new(),
                base: base.into(),
                password: password.map(|p| Sha256::digest(p.as_bytes()).into()),
                repo: "o/r".into(),
                sponsor: "o".into(),
                github_token: None,
                trust_proxy: true,
            },
            store: Mutex::new(Store::in_memory().unwrap()),
            pings: Limiter::new(3, Duration::from_secs(3600)),
            sign_ins: Limiter::new(2, Duration::from_secs(900)),
            github: Mutex::new((None, Vec::new())),
        })
    }

    async fn send(app: &Arc<App>, req: Request<Body>) -> (StatusCode, String) {
        let mut req = req;
        req.extensions_mut()
            .insert(ConnectInfo::<SocketAddr>("10.0.0.1:5000".parse().unwrap()));
        let resp = router(app.clone()).oneshot(req).await.unwrap();
        let status = resp.status();
        let body = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&body).into_owned())
    }

    fn post_ping(path: &str, body: String, from: &str) -> Request<Body> {
        Request::post(path)
            .header("content-type", "application/json")
            .header("x-forwarded-for", from)
            .body(Body::from(body))
            .unwrap()
    }

    fn basic(password: &str) -> String {
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("me:{password}"))
        )
    }

    #[tokio::test]
    async fn pings_are_checked_stored_and_limited() {
        let app = app("/app/shufflingtome", Some("secret"));
        let good = serde_json::to_string(&ping(1)).unwrap();
        let (status, _) = send(
            &app,
            post_ping("/app/shufflingtome/v1/ping", good.clone(), "1.2.3.4"),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(app.store().pings_since("2000-01-01").unwrap().len(), 1);

        let mut bad = serde_json::to_value(ping(2)).unwrap();
        bad["collection"] = serde_json::json!(["Black Lotus"]);
        let (status, body) = send(
            &app,
            post_ping("/app/shufflingtome/v1/ping", bad.to_string(), "1.2.3.4"),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        let mut odd = ping(3);
        odd.errors.insert("camera: device 9".into(), "1-9".into());
        let (status, body) = send(
            &app,
            post_ping(
                "/app/shufflingtome/v1/ping",
                serde_json::to_string(&odd).unwrap(),
                "1.2.3.4",
            ),
        )
        .await;
        assert_eq!(
            (status, body.as_str()),
            (
                StatusCode::BAD_REQUEST,
                "unknown error kind \"camera: device 9\""
            )
        );

        // The fourth from one address within the hour is refused; another address is fine.
        let (status, _) = send(
            &app,
            post_ping("/app/shufflingtome/v1/ping", good.clone(), "1.2.3.4"),
        )
        .await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        let (status, _) = send(
            &app,
            post_ping("/app/shufflingtome/v1/ping", good, "5.6.7.8"),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let big = format!("{{\"pad\":\"{}\"}}", "x".repeat(MAX_BODY));
        let (status, _) = send(
            &app,
            post_ping("/app/shufflingtome/v1/ping", big, "9.9.9.9"),
        )
        .await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn the_page_needs_the_password() {
        let app = app("/app/shufflingtome", Some("secret"));
        let page = |auth: Option<String>| {
            let mut req = Request::get("/app/shufflingtome/").header("x-forwarded-for", "1.2.3.4");
            if let Some(a) = auth {
                req = req.header("authorization", a);
            }
            req.body(Body::empty()).unwrap()
        };
        let (status, _) = send(&app, page(None)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, body) = send(&app, page(Some(basic("secret")))).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("<h2>Servers</h2>"));

        let (status, json) = send(
            &app,
            Request::get("/app/shufflingtome/api/metrics")
                .header("authorization", basic("secret"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(json.contains("\"servers\""));

        // Two wrong passwords, then even the right one waits.
        for _ in 0..2 {
            let (status, _) = send(&app, page(Some(basic("guess")))).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
        }
        let (status, _) = send(&app, page(Some(basic("secret")))).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);

        let (status, _) = send(
            &app,
            Request::get("/app/shufflingtome")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::PERMANENT_REDIRECT);
    }

    #[tokio::test]
    async fn no_password_no_page() {
        let app = app("", None);
        let (status, _) = send(&app, Request::get("/").body(Body::empty()).unwrap()).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        let (status, body) = send(&app, Request::get("/health").body(Body::empty()).unwrap()).await;
        assert_eq!((status, body.as_str()), (StatusCode::OK, "ok"));
    }
}
