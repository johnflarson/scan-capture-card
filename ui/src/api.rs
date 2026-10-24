//! Talking to the mtg-scan server: JSON requests and the live event stream.

use futures::StreamExt;
use gloo_net::eventsource::futures::EventSource;
use gloo_net::http::{Request, Response};
use leptos::task::spawn_local;
use mtg_scan_shared::{
    AddCard, ApiTokenView, AuthStatus, BenchmarkView, ClientCommand, CollectionView,
    CreatedApiToken, DatabaseView, DeckView, DecksView, EditResult, LinkInfo, LinkView,
    NewApiToken, OllamaView, PasswordChange, PrintingView, PullView, RecognitionSetup, Redeem,
    RowEdit, ServerEvent, SessionView, SetUp, SettingsChange, SettingsView, SetupView, SignIn,
    UserChange, UserView,
};
use serde::de::DeserializeOwned;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// The server's plain-text error message.
async fn message(resp: Response) -> String {
    resp.text()
        .await
        .ok()
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| format!("HTTP {}", resp.status()))
}

/// The JSON body, or the server's plain-text error message. A request
/// refused because the session ended reloads the page, which then asks to
/// sign in again.
async fn json_or_message<T: DeserializeOwned>(resp: Response) -> Result<T, String> {
    if resp.status() == 401 {
        signed_out();
    }
    if !resp.ok() {
        return Err(message(resp).await);
    }
    resp.json().await.map_err(err)
}

/// Show the sign-in page (the session ended or was revoked).
pub fn signed_out() {
    if let Some(window) = web_sys::window() {
        let _ = window.location().reload();
    }
}

/// An empty answer, or the server's message.
async fn done(resp: Response) -> Result<(), String> {
    if resp.status() == 401 {
        signed_out();
    }
    if resp.ok() {
        Ok(())
    } else {
        Err(message(resp).await)
    }
}

/// JSON for the sign-in requests: a 401 there is a wrong password, not an
/// ended session.
async fn auth_json<T: DeserializeOwned>(resp: Response) -> Result<T, String> {
    if !resp.ok() {
        return Err(message(resp).await);
    }
    resp.json().await.map_err(err)
}

async fn post<B: serde::Serialize>(url: &str, body: &B) -> Result<Response, String> {
    Request::post(url)
        .json(body)
        .map_err(err)?
        .send()
        .await
        .map_err(err)
}

pub async fn auth_status() -> Result<AuthStatus, String> {
    let resp = Request::get("/api/auth").send().await.map_err(err)?;
    auth_json(resp).await
}

pub async fn set_up(form: &SetUp) -> Result<AuthStatus, String> {
    auth_json(post("/api/auth/setup", form).await?).await
}

pub async fn sign_in(form: &SignIn) -> Result<AuthStatus, String> {
    auth_json(post("/api/auth/sign-in", form).await?).await
}

pub async fn sign_out() -> Result<(), String> {
    let resp = Request::post("/api/auth/sign-out")
        .send()
        .await
        .map_err(err)?;
    done(resp).await
}

pub async fn link_info(code: &str) -> Result<LinkInfo, String> {
    let resp = Request::get("/api/auth/link")
        .query([("code", code)])
        .send()
        .await
        .map_err(err)?;
    auth_json(resp).await
}

pub async fn redeem(form: &Redeem) -> Result<AuthStatus, String> {
    auth_json(post("/api/auth/link", form).await?).await
}

pub async fn change_password(change: &PasswordChange) -> Result<(), String> {
    done(post("/api/account/password", change).await?).await
}

pub async fn api_tokens() -> Result<Vec<ApiTokenView>, String> {
    let resp = Request::get("/api/account/tokens")
        .send()
        .await
        .map_err(err)?;
    json_or_message(resp).await
}

pub async fn new_api_token(name: &str) -> Result<CreatedApiToken, String> {
    let body = NewApiToken { name: name.into() };
    json_or_message(post("/api/account/tokens", &body).await?).await
}

pub async fn delete_api_token(id: i64) -> Result<(), String> {
    let resp = Request::delete(&format!("/api/account/tokens/{id}"))
        .send()
        .await
        .map_err(err)?;
    done(resp).await
}

pub async fn users() -> Result<Vec<UserView>, String> {
    let resp = Request::get("/api/users").send().await.map_err(err)?;
    json_or_message(resp).await
}

pub async fn invite() -> Result<LinkView, String> {
    let resp = Request::post("/api/users/invite")
        .send()
        .await
        .map_err(err)?;
    json_or_message(resp).await
}

pub async fn reset_link(id: i64) -> Result<LinkView, String> {
    let resp = Request::post(&format!("/api/users/{id}/reset"))
        .send()
        .await
        .map_err(err)?;
    json_or_message(resp).await
}

pub async fn change_user(id: i64, change: &UserChange) -> Result<UserView, String> {
    json_or_message(post(&format!("/api/users/{id}"), change).await?).await
}

pub async fn setup() -> Result<SetupView, String> {
    let resp = Request::get("/api/setup").send().await.map_err(err)?;
    json_or_message(resp).await
}

/// What the Ollama server at `url` has (empty: the configured one).
pub async fn setup_ollama(url: &str) -> Result<OllamaView, String> {
    let resp = Request::get("/api/setup/ollama")
        .query([("url", url)])
        .send()
        .await
        .map_err(err)?;
    json_or_message(resp).await
}

pub async fn setup_recognition(setup: &RecognitionSetup) -> Result<SetupView, String> {
    json_or_message(post("/api/setup/recognition", setup).await?).await
}

pub async fn start_pull(model: &str) -> Result<PullView, String> {
    json_or_message(post("/api/setup/pull", &serde_json::json!({ "model": model })).await?).await
}

pub async fn pull_progress() -> Result<PullView, String> {
    let resp = Request::get("/api/setup/pull").send().await.map_err(err)?;
    json_or_message(resp).await
}

pub async fn benchmark() -> Result<BenchmarkView, String> {
    let resp = Request::post("/api/setup/benchmark")
        .send()
        .await
        .map_err(err)?;
    json_or_message(resp).await
}

pub async fn setup_telemetry(enabled: bool) -> Result<(), String> {
    done(
        post(
            "/api/setup/telemetry",
            &serde_json::json!({ "enabled": enabled }),
        )
        .await?,
    )
    .await
}

/// Finish the wizard (`false`: show it again).
pub async fn setup_done(finished: bool) -> Result<(), String> {
    done(post("/api/setup/done", &serde_json::json!({ "done": finished })).await?).await
}

pub async fn session() -> Result<SessionView, String> {
    let resp = Request::get("/api/session").send().await.map_err(err)?;
    json_or_message(resp).await
}

pub async fn collection() -> Result<CollectionView, String> {
    let resp = Request::get("/api/collection").send().await.map_err(err)?;
    json_or_message(resp).await
}

/// Look a printing up by Scryfall link or ID.
pub async fn printing(link: &str) -> Result<PrintingView, String> {
    let resp = Request::get("/api/printing")
        .query([("q", link)])
        .send()
        .await
        .map_err(err)?;
    json_or_message(resp).await
}

/// Every paper printing of a card, by exact name.
pub async fn printings(name: &str) -> Result<Vec<PrintingView>, String> {
    let resp = Request::get("/api/printings")
        .query([("name", name)])
        .send()
        .await
        .map_err(err)?;
    json_or_message(resp).await
}

pub async fn edit(edit: &RowEdit) -> Result<EditResult, String> {
    let resp = Request::post("/api/collection/edit")
        .json(edit)
        .map_err(err)?
        .send()
        .await
        .map_err(err)?;
    json_or_message(resp).await
}

pub async fn add(add: &AddCard) -> Result<EditResult, String> {
    let resp = Request::post("/api/collection/add")
        .json(add)
        .map_err(err)?
        .send()
        .await
        .map_err(err)?;
    json_or_message(resp).await
}

/// The Moxfield user's decks; `refresh` skips the server's short-term cache.
pub async fn decks(refresh: bool) -> Result<DecksView, String> {
    let resp = Request::get("/api/decks")
        .query([("refresh", if refresh { "true" } else { "false" })])
        .send()
        .await
        .map_err(err)?;
    json_or_message(resp).await
}

/// Every deck, including those hidden on the Settings page.
pub async fn all_decks() -> Result<DecksView, String> {
    let resp = Request::get("/api/decks")
        .query([("all", "true")])
        .send()
        .await
        .map_err(err)?;
    json_or_message(resp).await
}

pub async fn settings() -> Result<SettingsView, String> {
    let resp = Request::get("/api/settings").send().await.map_err(err)?;
    json_or_message(resp).await
}

/// Save one change; returns the settings as saved.
pub async fn change_settings(change: &SettingsChange) -> Result<SettingsView, String> {
    let resp = Request::post("/api/settings")
        .json(change)
        .map_err(err)?
        .send()
        .await
        .map_err(err)?;
    json_or_message(resp).await
}

pub async fn database() -> Result<DatabaseView, String> {
    let resp = Request::get("/api/settings/database")
        .send()
        .await
        .map_err(err)?;
    json_or_message(resp).await
}

pub async fn deck(id: &str) -> Result<DeckView, String> {
    // Deck IDs are URL-safe (letters, digits, `-`, `_`), as Moxfield issues them.
    let resp = Request::get(&format!("/api/decks/{id}"))
        .send()
        .await
        .map_err(err)?;
    json_or_message(resp).await
}

pub async fn send(cmd: ClientCommand) -> Result<(), String> {
    done(post("/api/command", &cmd).await?).await
}

/// Listen to `/api/events` for the life of the page. `on_connection(false)`
/// is called when the connection drops; the browser reconnects by itself and
/// the next event calls `on_connection(true)`.
pub fn subscribe(
    on_event: impl Fn(ServerEvent) + 'static,
    on_connection: impl Fn(bool) + 'static,
) -> Result<(), String> {
    let mut source = EventSource::new("/api/events").map_err(err)?;
    let mut messages = source.subscribe("message").map_err(err)?;
    spawn_local(async move {
        // Keep the EventSource alive as long as we're reading from it.
        let _source = source;
        while let Some(item) = messages.next().await {
            match item {
                Ok((_, msg)) => {
                    on_connection(true);
                    if let Some(event) = msg
                        .data()
                        .as_string()
                        .and_then(|text| serde_json::from_str(&text).ok())
                    {
                        on_event(event);
                    }
                }
                Err(_) => on_connection(false),
            }
        }
    });
    Ok(())
}
