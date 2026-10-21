//! Who may use `mtg-scan ui` from other computers.
//!
//! Requests from this computer (loopback) are always allowed. When
//! `MTG_SCAN_UI_TOKEN` is set (in `.env`), requests from anywhere else need
//! it: as the `mtg_scan_token` cookie, an `Authorization: Bearer` header, or
//! once as `?token=` in a link (which sets the cookie). Without the token the
//! browser gets a small sign-in page and API calls get 401. Without
//! `MTG_SCAN_UI_TOKEN` anyone on the network can use the UI, as before.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::body::to_bytes;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Redirect, Response};

/// `.env` / environment setting holding the access token.
pub const TOKEN_VAR: &str = "MTG_SCAN_UI_TOKEN";
const COOKIE: &str = "mtg_scan_token";
const LOGIN_PATH: &str = "/login";

/// The token other computers need, if one is set.
pub fn configured_token() -> Option<String> {
    std::env::var(TOKEN_VAR)
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

#[derive(Clone)]
pub struct Access {
    token: Option<Arc<str>>,
}

impl Access {
    pub fn new(token: Option<String>) -> Self {
        Self {
            token: token.map(Into::into),
        }
    }
}

pub(super) fn is_local(ip: IpAddr) -> bool {
    ip.to_canonical().is_loopback()
}

/// Same length and same bytes, without stopping at the first difference.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

fn cookie_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == COOKIE)
        .map(|(_, value)| value)
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

/// `token=` from a query string or form body.
fn token_param(text: &str) -> Option<String> {
    text.split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(name, _)| *name == "token")
        .map(|(_, value)| crate::scryfall::percent_decode(&value.replace('+', " ")))
}

/// The query string without `token=…`.
fn without_token(query: &str) -> String {
    query
        .split('&')
        .filter(|pair| !pair.is_empty() && !pair.starts_with("token="))
        .collect::<Vec<_>>()
        .join("&")
}

fn signed_in(to: &str, token: &str) -> Response {
    let cookie = format!("{COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age=31536000");
    let mut resp = Redirect::to(to).into_response();
    if let Ok(v) = cookie.parse() {
        resp.headers_mut().insert(header::SET_COOKIE, v);
    }
    resp
}

/// Runs before every route.
pub async fn guard(
    State(access): State<Access>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    let Some(token) = access.token.as_deref() else {
        return next.run(req).await;
    };
    if is_local(peer.ip()) {
        return next.run(req).await;
    }
    let headers = req.headers();
    if cookie_token(headers).is_some_and(|t| same(t, token))
        || bearer_token(headers).is_some_and(|t| same(t, token))
    {
        return next.run(req).await;
    }

    let path = req.uri().path().to_string();
    let query = req.uri().query().unwrap_or_default().to_string();

    // A shared link: http://host:port/?token=…
    if req.method() == Method::GET
        && let Some(given) = token_param(&query)
    {
        if same(&given, token) {
            let rest = without_token(&query);
            let to = if rest.is_empty() {
                path
            } else {
                format!("{path}?{rest}")
            };
            return signed_in(&to, token);
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        return login_page(
            StatusCode::UNAUTHORIZED,
            Some("That link's token is not valid."),
        );
    }

    // The sign-in form.
    if req.method() == Method::POST && path == LOGIN_PATH {
        let body = to_bytes(req.into_body(), 4096).await.unwrap_or_default();
        let given = token_param(&String::from_utf8_lossy(&body)).unwrap_or_default();
        if same(given.trim(), token) {
            return signed_in("/", token);
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        return login_page(StatusCode::UNAUTHORIZED, Some("That token is not valid."));
    }

    if path.starts_with("/api/") {
        return (
            StatusCode::UNAUTHORIZED,
            "Sign in first: open this page's address in a browser and enter the access token.",
        )
            .into_response();
    }
    login_page(StatusCode::UNAUTHORIZED, None)
}

fn login_page(status: StatusCode, error: Option<&str>) -> Response {
    let error = error
        .map(|e| format!(r#"<p class="error">{e}</p>"#))
        .unwrap_or_default();
    let html = format!(
        r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>mtg-scan · Sign in</title>
<style>
  body {{ margin: 0; min-height: 100vh; display: grid; place-items: center; padding: 16px;
         background: #1a110b; color: #f3e3cf; font: 15px system-ui, sans-serif; }}
  form {{ width: min(380px, 100%); padding: 28px; border-radius: 24px; background: #2b1d14;
          box-shadow: 0 8px 24px rgb(0 0 0 / .5); }}
  h1 {{ margin: 0 0 8px; font-weight: 400; font-size: 22px; }}
  p {{ color: #cbb59c; margin: 0 0 16px; line-height: 1.4; }}
  input {{ box-sizing: border-box; width: 100%; padding: 10px 14px; border-radius: 8px;
           border: 1px solid #8d7360; background: #1f150e; color: inherit; font: inherit; }}
  button {{ margin-top: 14px; width: 100%; padding: 10px; border: 0; border-radius: 999px;
            background: #4cbfbf; color: #002020; font: 500 15px system-ui, sans-serif; cursor: pointer; }}
  .error {{ color: #ffb4ab; }}
</style></head>
<body><form method="post" action="{LOGIN_PATH}">
<h1>mtg-scan</h1>
<p>This scanner needs an access token from other computers. It is the <code>{TOKEN_VAR}</code>
value in the .env file next to mtg-scan.</p>
{error}
<input name="token" type="password" placeholder="Access token" autocomplete="current-password" autofocus required>
<button type="submit">Sign in</button>
</form></body></html>"#
    );
    (status, Html(html)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_tokens_fully() {
        assert!(same("abc123", "abc123"));
        assert!(!same("abc123", "abc124"));
        assert!(!same("abc", "abc123"));
        assert!(!same("", "x"));
    }

    #[test]
    fn reads_tokens_from_cookies_headers_and_forms() {
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            "theme=dark; mtg_scan_token=s3cret; x=1".parse().unwrap(),
        );
        h.insert(header::AUTHORIZATION, "Bearer t0ken".parse().unwrap());
        assert_eq!(cookie_token(&h), Some("s3cret"));
        assert_eq!(bearer_token(&h), Some("t0ken"));
        assert_eq!(token_param("a=1&token=ab%2Bc&b=2").as_deref(), Some("ab+c"));
        assert_eq!(token_param("token=a+b").as_deref(), Some("a b"));
        assert_eq!(token_param("a=1"), None);
        assert_eq!(without_token("token=x&tab=decks"), "tab=decks");
        assert_eq!(without_token("token=x"), "");
    }

    #[test]
    fn loopback_is_local() {
        assert!(is_local("127.0.0.1".parse().unwrap()));
        assert!(is_local("::1".parse().unwrap()));
        assert!(is_local("::ffff:127.0.0.1".parse().unwrap()));
        assert!(!is_local("192.168.1.20".parse().unwrap()));
    }
}
