//! `GET /api/camera`: a WebSocket the page sends its camera's frames over,
//! when the camera is attached to the browser's computer rather than to the
//! server (e.g. Docker Desktop, which can't pass cameras into containers).
//!
//! The page sends one JPEG per binary message and waits for the server's
//! `"next"` before sending another, so a slow server never queues frames.
//! The connection closes when the page stops its camera, or with code 4000
//! when another browser took over.

use std::sync::Arc;

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use super::AppState;

/// A 4K JPEG is a few MB; leave room.
const MAX_FRAME_BYTES: usize = 24 * 1024 * 1024;
/// Close code telling the page that another browser's camera took over.
const REPLACED: u16 = 4000;

#[derive(Deserialize)]
pub struct CameraQuery {
    /// The camera's name as the browser reports it.
    #[serde(default)]
    label: String,
}

pub async fn connect(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<CameraQuery>,
    ws: WebSocketUpgrade,
) -> Response {
    // Only this app's own pages may become the camera: a page from another
    // site could otherwise open the socket from the user's browser.
    if !same_origin(&headers) {
        return (
            StatusCode::FORBIDDEN,
            "camera frames only from this app's pages",
        )
            .into_response();
    }
    let label = match q.label.trim() {
        "" => "Browser camera".to_string(),
        l => l.chars().take(80).collect(),
    };
    ws.max_message_size(MAX_FRAME_BYTES)
        .on_upgrade(move |socket| feed(socket, st, label))
}

async fn feed(mut socket: WebSocket, st: Arc<AppState>, label: String) {
    let feed = st.engine.attach_browser_camera(label);
    let mut shutdown = st.shutdown.clone();
    loop {
        let msg = tokio::select! {
            msg = socket.recv() => msg,
            _ = shutdown.wait_for(|stop| *stop) => break,
        };
        match msg {
            Some(Ok(Message::Binary(jpeg))) => {
                let decoded = tokio::task::spawn_blocking(move || {
                    image::load_from_memory_with_format(&jpeg, image::ImageFormat::Jpeg)
                        .map(|img| img.to_rgb8())
                })
                .await;
                // A bad frame is skipped; the next one may be fine.
                if let Ok(Ok(frame)) = decoded {
                    feed.push(frame);
                }
                if feed.is_closed() {
                    let _ = socket
                        .send(Message::Close(Some(CloseFrame {
                            code: REPLACED,
                            reason: "another browser's camera took over".into(),
                        })))
                        .await;
                    return;
                }
                if socket.send(Message::Text("next".into())).await.is_err() {
                    break;
                }
            }
            Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
            Some(Ok(_)) => {}
        }
    }
    feed.close();
}

/// The `Origin` header names the host the request went to.
fn same_origin(headers: &HeaderMap) -> bool {
    let get = |name| headers.get(name).and_then(|v| v.to_str().ok());
    match (get(header::ORIGIN), get(header::HOST)) {
        (Some(origin), Some(host)) => origin
            .split_once("://")
            .is_some_and(|(_, rest)| rest.eq_ignore_ascii_case(host)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(origin: Option<&str>, host: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::HOST, host.parse().unwrap());
        if let Some(o) = origin {
            h.insert(header::ORIGIN, o.parse().unwrap());
        }
        h
    }

    #[test]
    fn frames_only_from_this_apps_pages() {
        assert!(same_origin(&headers(
            Some("http://localhost:8787"),
            "localhost:8787"
        )));
        assert!(same_origin(&headers(
            Some("https://tome.example.com"),
            "tome.example.com"
        )));
        assert!(!same_origin(&headers(
            Some("https://evil.example"),
            "localhost:8787"
        )));
        assert!(!same_origin(&headers(
            Some("http://localhost:9999"),
            "localhost:8787"
        )));
        assert!(!same_origin(&headers(None, "localhost:8787")));
    }
}
