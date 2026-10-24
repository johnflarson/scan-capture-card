//! Who may use the web UI: everyone signs in (see [`crate::accounts`]),
//! whether on this computer, the network or behind a reverse proxy.
//!
//! Browsers carry a session cookie (`HttpOnly`, `SameSite=Lax`, `Secure`
//! when the proxy says the page came over HTTPS); scripts send an API token
//! as `Authorization: Bearer tome_...`. The app's files, `/api/health` and
//! the sign-in requests are open; every other `/api/` request needs a user,
//! and `/api/users` an admin. Requests that change something are refused
//! when the browser says they come from another site's page.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use axum::extract::{ConnectInfo, Path, Query, Request, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use chrono::{DateTime, Utc};
use mtg_scan_shared::{
    ApiTokenView, AuthStatus, CreatedApiToken, LinkInfo, LinkView, Me, NewApiToken, PasswordChange,
    Redeem, SetUp, SignIn, UserChange, UserView,
};
use serde::Deserialize;

use super::{AppState, blocking};
use crate::accounts::{self, Accounts, ApiToken, LinkKind, SignInError, Signed, User};

const COOKIE: &str = "tome_session";

/// Requests that work without signing in (besides the app's own files).
const OPEN: [&str; 5] = [
    "/api/health",
    "/api/auth",
    "/api/auth/setup",
    "/api/auth/sign-in",
    "/api/auth/link",
];

/// Runs before every route.
pub async fn guard(State(st): State<Arc<AppState>>, mut req: Request, next: Next) -> Response {
    let changes = !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    if changes && req.headers().contains_key(header::ORIGIN) && !same_origin(req.headers()) {
        return (
            StatusCode::FORBIDDEN,
            "This request came from another site's page. (Behind a reverse proxy, pass the \
             Host header on, or set X-Forwarded-Host.)",
        )
            .into_response();
    }
    let signed = signed(&st.accounts, req.headers()).await;
    let path = req.uri().path().to_string();
    if let Some(s) = &signed {
        req.extensions_mut().insert(s.clone());
    }
    let api = path == "/api" || path.starts_with("/api/");
    if !api || OPEN.contains(&path.as_str()) {
        return next.run(req).await;
    }
    match signed {
        None => (
            StatusCode::UNAUTHORIZED,
            "Sign in first: open this server's address in a browser.",
        )
            .into_response(),
        Some(s) if path.starts_with("/api/users") && !s.user.is_admin => {
            (StatusCode::FORBIDDEN, "Only admins can manage users.").into_response()
        }
        Some(_) => next.run(req).await,
    }
}

/// The user behind the session cookie or API token, if any.
async fn signed(accounts: &Arc<Accounts>, headers: &HeaderMap) -> Option<Signed> {
    let session = cookie(headers).map(str::to_string);
    let token = bearer(headers).map(str::to_string);
    if session.is_none() && token.is_none() {
        return None;
    }
    let accounts = accounts.clone();
    let found = tokio::task::spawn_blocking(move || match (session, token) {
        (Some(s), _) => accounts.session(&s),
        (None, Some(t)) => accounts.api_token(&t),
        (None, None) => Ok(None),
    })
    .await;
    match found {
        Ok(Ok(signed)) => signed,
        Ok(Err(e)) => {
            eprintln!("  ! could not check the sign-in ({e:#})");
            None
        }
        Err(_) => None,
    }
}

fn cookie(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == COOKIE)
        .map(|(_, value)| value)
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
}

/// The page's origin (scheme://host[:port]) names this server: the Host
/// header, or the host a reverse proxy says it forwarded.
pub fn same_origin(headers: &HeaderMap) -> bool {
    let get = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let Some((_, origin)) = get("origin").and_then(|o| o.split_once("://")) else {
        return false;
    };
    [get("host"), get("x-forwarded-host")]
        .into_iter()
        .flatten()
        .any(|host| origin.eq_ignore_ascii_case(host.trim()))
}

/// The page came over HTTPS (through a reverse proxy; the app itself
/// serves plain HTTP).
fn over_https(headers: &HeaderMap) -> bool {
    headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|p| p.trim().eq_ignore_ascii_case("https"))
}

fn session_cookie(value: &str, max_age: i64, headers: &HeaderMap) -> String {
    let secure = if over_https(headers) { "; Secure" } else { "" };
    format!("{COOKIE}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}")
}

fn me(user: &User) -> Me {
    Me {
        id: user.id,
        name: user.name.clone(),
        is_admin: user.is_admin,
    }
}

fn time(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Start a session for `user` and answer with its cookie and who they are.
async fn signed_in(st: &Arc<AppState>, user: User, headers: &HeaderMap) -> Response {
    let accounts = st.accounts.clone();
    let agent = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let started = {
        let user = user.clone();
        tokio::task::spawn_blocking(move || accounts.start_session(&user, &agent)).await
    };
    match started {
        Ok(Ok(secret)) => (
            [(
                header::SET_COOKIE,
                session_cookie(&secret, accounts::SESSION_DAYS * 86_400, headers),
            )],
            Json(AuthStatus {
                user: Some(me(&user)),
                setup_needed: false,
            }),
        )
            .into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

pub async fn status(
    State(st): State<Arc<AppState>>,
    signed: Option<Extension<Signed>>,
) -> Json<AuthStatus> {
    Json(AuthStatus {
        user: signed.map(|Extension(s)| me(&s.user)),
        setup_needed: st.accounts.setup_code().is_some(),
    })
}

pub async fn set_up(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(setup): Json<SetUp>,
) -> Response {
    let accounts = st.accounts.clone();
    let created = tokio::task::spawn_blocking(move || {
        accounts.set_up(&setup.code, setup.name.trim(), &setup.password)
    })
    .await;
    match created {
        Ok(Ok(user)) => {
            println!("Created the admin account {}.", user.name);
            signed_in(&st, user, &headers).await
        }
        Ok(Err(e)) => (StatusCode::BAD_REQUEST, format!("{e:#}")).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

pub async fn sign_in(
    State(st): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(form): Json<SignIn>,
) -> Response {
    let accounts = st.accounts.clone();
    let checked = tokio::task::spawn_blocking(move || {
        accounts.sign_in(&form.name, &form.password, peer.ip())
    })
    .await;
    match checked {
        Ok(Ok(user)) => signed_in(&st, user, &headers).await,
        Ok(Err(SignInError::Wrong)) => {
            tokio::time::sleep(Duration::from_millis(300)).await;
            (
                StatusCode::UNAUTHORIZED,
                "That name and password don't match an account.",
            )
                .into_response()
        }
        Ok(Err(SignInError::TooMany)) => (
            StatusCode::TOO_MANY_REQUESTS,
            "Too many failed sign-ins. Wait 15 minutes and try again.",
        )
            .into_response(),
        Ok(Err(SignInError::Other(e))) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

pub async fn sign_out(
    State(st): State<Arc<AppState>>,
    Extension(signed): Extension<Signed>,
    headers: HeaderMap,
) -> Response {
    if let Some(session) = signed.session {
        let accounts = st.accounts.clone();
        let _ = tokio::task::spawn_blocking(move || accounts.sign_out(&session)).await;
    }
    (
        StatusCode::NO_CONTENT,
        [(header::SET_COOKIE, session_cookie("", 0, &headers))],
    )
        .into_response()
}

#[derive(Deserialize)]
pub struct CodeQuery {
    code: String,
}

pub async fn link_info(State(st): State<Arc<AppState>>, Query(q): Query<CodeQuery>) -> Response {
    let accounts = st.accounts.clone();
    blocking(StatusCode::NOT_FOUND, move || {
        let link = accounts.link(&q.code)?.ok_or_else(|| {
            anyhow!("This link was already used or has expired. Ask the admin for a new one.")
        })?;
        Ok(LinkInfo {
            kind: match link.kind {
                LinkKind::Invite => mtg_scan_shared::LinkKind::Invite,
                LinkKind::Reset => mtg_scan_shared::LinkKind::Reset,
            },
            name: link.user.map(|u| u.name),
            expires: time(link.expires_at),
        })
    })
    .await
}

pub async fn redeem(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(form): Json<Redeem>,
) -> Response {
    let accounts = st.accounts.clone();
    let done = tokio::task::spawn_blocking(move || {
        accounts.redeem(&form.code, form.name.trim(), &form.password)
    })
    .await;
    match done {
        Ok(Ok(user)) => signed_in(&st, user, &headers).await,
        Ok(Err(e)) => (StatusCode::BAD_REQUEST, format!("{e:#}")).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

pub async fn change_password(
    State(st): State<Arc<AppState>>,
    Extension(signed): Extension<Signed>,
    Json(change): Json<PasswordChange>,
) -> Response {
    let accounts = st.accounts.clone();
    blocking(StatusCode::BAD_REQUEST, move || {
        accounts.change_password(
            &signed.user,
            &change.current,
            &change.new,
            signed.session.as_deref(),
        )
    })
    .await
}

fn token_view(t: ApiToken) -> ApiTokenView {
    ApiTokenView {
        id: t.id,
        name: t.name,
        created: time(t.created_at),
        last_used: t.last_used_at.map(time),
    }
}

pub async fn tokens(
    State(st): State<Arc<AppState>>,
    Extension(signed): Extension<Signed>,
) -> Response {
    let accounts = st.accounts.clone();
    blocking(StatusCode::INTERNAL_SERVER_ERROR, move || {
        Ok(accounts
            .api_tokens(&signed.user)?
            .into_iter()
            .map(token_view)
            .collect::<Vec<_>>())
    })
    .await
}

pub async fn new_token(
    State(st): State<Arc<AppState>>,
    Extension(signed): Extension<Signed>,
    Json(new): Json<NewApiToken>,
) -> Response {
    let accounts = st.accounts.clone();
    blocking(StatusCode::BAD_REQUEST, move || {
        let (token, secret) = accounts.new_api_token(&signed.user, &new.name)?;
        Ok(CreatedApiToken {
            token: token_view(token),
            secret,
        })
    })
    .await
}

pub async fn delete_token(
    State(st): State<Arc<AppState>>,
    Extension(signed): Extension<Signed>,
    Path(id): Path<i64>,
) -> Response {
    let accounts = st.accounts.clone();
    blocking(StatusCode::NOT_FOUND, move || {
        if !accounts.delete_api_token(&signed.user, id)? {
            return Err(anyhow!("There is no such token"));
        }
        Ok(())
    })
    .await
}

fn user_view(u: User) -> UserView {
    UserView {
        id: u.id,
        name: u.name,
        is_admin: u.is_admin,
        disabled: u.disabled,
        has_password: u.has_password,
        created: time(u.created_at),
        last_seen: u.last_seen_at.map(time),
    }
}

pub async fn users(State(st): State<Arc<AppState>>) -> Response {
    let accounts = st.accounts.clone();
    blocking(StatusCode::INTERNAL_SERVER_ERROR, move || {
        Ok(accounts
            .users()?
            .into_iter()
            .map(user_view)
            .collect::<Vec<_>>())
    })
    .await
}

pub async fn invite(
    State(st): State<Arc<AppState>>,
    Extension(signed): Extension<Signed>,
) -> Response {
    let accounts = st.accounts.clone();
    blocking(StatusCode::INTERNAL_SERVER_ERROR, move || {
        let (code, expires) = accounts.invite(&signed.user)?;
        Ok(link_view("join", &code, expires))
    })
    .await
}

pub async fn reset(
    State(st): State<Arc<AppState>>,
    Extension(signed): Extension<Signed>,
    Path(id): Path<i64>,
) -> Response {
    let accounts = st.accounts.clone();
    blocking(StatusCode::NOT_FOUND, move || {
        accounts
            .user(id)?
            .ok_or_else(|| anyhow!("There is no such user"))?;
        let (code, expires) = accounts.reset_link(id, Some(&signed.user))?;
        Ok(link_view("reset", &code, expires))
    })
    .await
}

pub async fn change_user(
    State(st): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Json(change): Json<UserChange>,
) -> Response {
    let accounts = st.accounts.clone();
    blocking(StatusCode::BAD_REQUEST, move || -> Result<UserView> {
        accounts.set_flags(id, change.is_admin, change.disabled)?;
        let user = accounts
            .user(id)?
            .ok_or_else(|| anyhow!("There is no such user"))?;
        Ok(user_view(user))
    })
    .await
}

pub fn link_view(page: &str, code: &str, expires: DateTime<Utc>) -> LinkView {
    LinkView {
        path: format!("/{page}?code={code}"),
        expires: time(expires),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (name, value) in pairs {
            h.insert(*name, value.parse().unwrap());
        }
        h
    }

    #[test]
    fn same_origin_means_this_server() {
        assert!(same_origin(&headers(&[
            ("origin", "http://localhost:8787"),
            ("host", "localhost:8787")
        ])));
        assert!(same_origin(&headers(&[
            ("origin", "https://tome.example.com"),
            ("host", "tome.example.com")
        ])));
        // A reverse proxy that passes its own address as Host.
        assert!(same_origin(&headers(&[
            ("origin", "https://tome.example.com"),
            ("host", "127.0.0.1:8787"),
            ("x-forwarded-host", "tome.example.com")
        ])));
        assert!(!same_origin(&headers(&[
            ("origin", "https://evil.example"),
            ("host", "localhost:8787")
        ])));
        assert!(!same_origin(&headers(&[
            ("origin", "http://localhost:9999"),
            ("host", "localhost:8787")
        ])));
        assert!(!same_origin(&headers(&[("host", "localhost:8787")])));
        assert!(!same_origin(&headers(&[
            ("origin", "null"),
            ("host", "localhost:8787")
        ])));
    }

    #[test]
    fn reads_the_session_cookie_and_bearer_token() {
        let h = headers(&[
            ("cookie", "theme=dark; tome_session=s3cret; x=1"),
            ("authorization", "Bearer tome_abc"),
        ]);
        assert_eq!(cookie(&h), Some("s3cret"));
        assert_eq!(bearer(&h), Some("tome_abc"));
        assert_eq!(cookie(&headers(&[("cookie", "x=1")])), None);
    }

    #[test]
    fn session_cookies_are_http_only_and_secure_over_https() {
        let plain = session_cookie("v", 60, &HeaderMap::new());
        assert!(plain.contains("HttpOnly") && plain.contains("SameSite=Lax"));
        assert!(!plain.contains("Secure"));
        let proxied = session_cookie("v", 60, &headers(&[("x-forwarded-proto", "https")]));
        assert!(proxied.ends_with("; Secure"));
    }
}
