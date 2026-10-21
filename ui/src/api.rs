//! Talking to the mtg-scan server: JSON requests and the live event stream.

use futures::StreamExt;
use gloo_net::eventsource::futures::EventSource;
use gloo_net::http::Request;
use leptos::task::spawn_local;
use mtg_scan_shared::{ClientCommand, CollectionView, ServerEvent, SessionView};

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

pub async fn session() -> Result<SessionView, String> {
    let resp = Request::get("/api/session").send().await.map_err(err)?;
    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }
    resp.json().await.map_err(err)
}

pub async fn collection() -> Result<CollectionView, String> {
    let resp = Request::get("/api/collection").send().await.map_err(err)?;
    if !resp.ok() {
        // The server sends the error message as plain text.
        return Err(resp
            .text()
            .await
            .unwrap_or_else(|_| format!("HTTP {}", resp.status())));
    }
    resp.json().await.map_err(err)
}

pub async fn send(cmd: ClientCommand) -> Result<(), String> {
    let resp = Request::post("/api/command")
        .json(&cmd)
        .map_err(err)?
        .send()
        .await
        .map_err(err)?;
    if resp.ok() {
        Ok(())
    } else {
        Err(format!("HTTP {}", resp.status()))
    }
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
