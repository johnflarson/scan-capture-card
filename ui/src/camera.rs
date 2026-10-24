//! The camera of the computer this browser runs on, as the scanner's camera.
//! For servers without a camera of their own, e.g. Docker Desktop on Windows
//! or macOS, which can't pass cameras into containers.
//!
//! The page opens the camera (getUserMedia), plays it in a `<video>`, and
//! sends a JPEG of the current picture over `/api/camera` each time the
//! server asks for the next one. Browsers only allow cameras on HTTPS pages
//! and on `http://localhost`.
//!
//! The controller lives as long as the app, so scanning goes on while other
//! tabs are open, and the chosen camera is remembered for the next visit.

use futures::{SinkExt, StreamExt};
use gloo_net::websocket::futures::WebSocket;
use gloo_net::websocket::{Message, WebSocketError};
use gloo_timers::future::TimeoutFuture;
use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    Blob, CanvasRenderingContext2d, HtmlCanvasElement, HtmlVideoElement, MediaDeviceInfo,
    MediaDeviceKind, MediaDevices, MediaStream, MediaStreamConstraints, MediaStreamTrack,
};

/// localStorage: the camera used last ("" = the browser's default), so the
/// next visit starts it again.
const REMEMBERED: &str = "mtg-scan.browser-camera";
/// At most one frame this often (the server's auto-scan needs a few a second).
const MIN_INTERVAL_MS: f64 = 150.0;
const JPEG_QUALITY: f64 = 0.85;
/// The server closes with this code when another browser's camera took over.
const REPLACED: u16 = 4000;

#[derive(Clone, Copy)]
pub struct BrowserCamera {
    /// Frames are going to the server.
    pub active: RwSignal<bool>,
    pub starting: RwSignal<bool>,
    /// Why the camera stopped or couldn't start.
    pub error: RwSignal<Option<String>>,
    /// Video inputs as (device id, label), known once access was granted.
    pub devices: RwSignal<Vec<(String, String)>>,
    /// The device in use ("" = the browser's default).
    pub device: RwSignal<String>,
    /// Bumped whenever the stream changes, so `<video>` elements reattach.
    pub epoch: RwSignal<u32>,
    stream: StoredValue<Option<MediaStream>, LocalStorage>,
    /// Bumped by every start and stop; a sending loop from an older run ends.
    run: StoredValue<u32>,
}

impl BrowserCamera {
    pub fn new() -> Self {
        Self {
            active: RwSignal::new(false),
            starting: RwSignal::new(false),
            error: RwSignal::new(None),
            devices: RwSignal::new(Vec::new()),
            device: RwSignal::new(String::new()),
            epoch: RwSignal::new(0),
            stream: StoredValue::new_local(None),
            run: StoredValue::new(0),
        }
    }

    /// Whether this page may use a camera at all.
    pub fn available() -> bool {
        window().is_secure_context() && media_devices().is_some()
    }

    /// The camera used last time, if the camera of this browser was in use.
    pub fn remembered() -> Option<String> {
        storage()?.get_item(REMEMBERED).ok().flatten()
    }

    pub fn stream(&self) -> Option<MediaStream> {
        self.stream.get_value()
    }

    /// Start (or switch to) `device` at about `size`, and send its frames.
    pub fn start(self, device: String, size: (u32, u32)) {
        let run = self.run.get_value().wrapping_add(1);
        self.run.set_value(run);
        self.release();
        self.starting.set(true);
        self.error.set(None);
        spawn_local(async move {
            let ended = self.send_frames(run, device, size).await;
            if self.run.get_value() == run {
                self.release();
                self.active.set(false);
                self.starting.set(false);
                self.error.set(Some(ended));
            }
        });
    }

    /// Stop using this browser's camera (also on the next visit).
    pub fn stop(self) {
        self.run.set_value(self.run.get_value().wrapping_add(1));
        self.release();
        self.active.set(false);
        self.starting.set(false);
        if let Some(s) = storage() {
            let _ = s.remove_item(REMEMBERED);
        }
    }

    /// Reopen the camera at another size (the server wants a different one
    /// for the other scanning mode). Reopening, because browsers often keep
    /// the current mode when only asked through `applyConstraints`.
    pub fn resize(self, size: (u32, u32)) {
        self.start(self.device.get_untracked(), size);
    }

    fn release(&self) {
        if let Some(stream) = self.stream.get_value() {
            for track in stream.get_tracks().iter() {
                if let Ok(track) = track.dyn_into::<MediaStreamTrack>() {
                    track.stop();
                }
            }
        }
        self.stream.set_value(None);
        self.epoch.update(|e| *e += 1);
    }

    /// Open the camera and send frames until stopped; returns why it ended.
    async fn send_frames(self, run: u32, device: String, (w, h): (u32, u32)) -> String {
        let stream = match open(&device, w, h).await {
            Ok(stream) => stream,
            Err(e) => return e,
        };
        if self.run.get_value() != run {
            stop_tracks(&stream);
            return String::new();
        }
        let label = video_track(&stream)
            .map(|t| t.label())
            .filter(|l| !l.is_empty())
            .unwrap_or_else(|| "Browser camera".into());
        self.stream.set_value(Some(stream.clone()));
        self.epoch.update(|e| *e += 1);
        self.device.set(device.clone());
        if let Some(s) = storage() {
            let _ = s.set_item(REMEMBERED, &device);
        }
        self.devices.set(video_inputs().await);

        // A video element of our own: the Scan page's may come and go.
        let Ok(video) = document()
            .create_element("video")
            .map(|v| v.unchecked_into::<HtmlVideoElement>())
        else {
            return "could not show the camera".into();
        };
        video.set_muted(true);
        video.set_src_object(Some(&stream));
        if let Ok(playing) = video.play() {
            // Rejected when stopped before it starts: nothing to do then.
            spawn_local(async move {
                let _ = JsFuture::from(playing).await;
            });
        }
        let Ok(canvas) = document()
            .create_element("canvas")
            .map(|c| c.unchecked_into::<HtmlCanvasElement>())
        else {
            return "could not read the camera".into();
        };

        let socket = match WebSocket::open(&socket_url(&label)) {
            Ok(s) => s,
            Err(e) => return format!("could not connect to mtg-scan: {e}"),
        };
        let (mut tx, mut rx) = socket.split();
        self.active.set(true);
        self.starting.set(false);

        let ended = loop {
            if self.run.get_value() != run {
                break String::new();
            }
            let started = js_sys::Date::now();
            let (vw, vh) = (video.video_width(), video.video_height());
            if vw == 0 || vh == 0 {
                // Not playing yet.
                TimeoutFuture::new(100).await;
                continue;
            }
            let jpeg = match snapshot(&video, &canvas, vw, vh).await {
                Ok(jpeg) => jpeg,
                Err(e) => break e,
            };
            if tx.send(Message::Bytes(jpeg)).await.is_err() {
                break "lost the connection to mtg-scan".into();
            }
            match rx.next().await {
                Some(Ok(_)) => {}
                Some(Err(WebSocketError::ConnectionClose(e))) if e.code == REPLACED => {
                    break "another browser's camera took over".into();
                }
                _ => break "lost the connection to mtg-scan".into(),
            }
            let elapsed = js_sys::Date::now() - started;
            if elapsed < MIN_INTERVAL_MS {
                TimeoutFuture::new((MIN_INTERVAL_MS - elapsed) as u32).await;
            }
        };
        if let Ok(socket) = tx.reunite(rx) {
            let _ = socket.close(Some(1000), None);
        }
        video.set_src_object(None);
        ended
    }
}

fn media_devices() -> Option<MediaDevices> {
    window().navigator().media_devices().ok()
}

fn storage() -> Option<web_sys::Storage> {
    window().local_storage().ok().flatten()
}

fn video_track(stream: &MediaStream) -> Option<MediaStreamTrack> {
    stream.get_video_tracks().get(0).dyn_into().ok()
}

fn stop_tracks(stream: &MediaStream) {
    for track in stream.get_tracks().iter() {
        if let Ok(track) = track.dyn_into::<MediaStreamTrack>() {
            track.stop();
        }
    }
}

/// `{ width: { ideal: w }, height: { ideal: h } }`
fn size_constraints(w: u32, h: u32) -> js_sys::Object {
    let ideal = |v: u32| {
        let o = js_sys::Object::new();
        let _ = js_sys::Reflect::set(&o, &"ideal".into(), &JsValue::from(v));
        o
    };
    let video = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&video, &"width".into(), &ideal(w));
    let _ = js_sys::Reflect::set(&video, &"height".into(), &ideal(h));
    video
}

/// Ask for the camera; the browser may ask the user first.
async fn open(device: &str, w: u32, h: u32) -> Result<MediaStream, String> {
    let devices = media_devices().ok_or("this browser has no camera access")?;
    let video = size_constraints(w, h);
    if !device.is_empty() {
        let exact = js_sys::Object::new();
        let _ = js_sys::Reflect::set(&exact, &"exact".into(), &device.into());
        let _ = js_sys::Reflect::set(&video, &"deviceId".into(), &exact);
    }
    let constraints = MediaStreamConstraints::new();
    constraints.set_video(&video);
    constraints.set_audio(&JsValue::FALSE);
    let promise = devices
        .get_user_media_with_constraints(&constraints)
        .map_err(camera_error)?;
    JsFuture::from(promise)
        .await
        .map(|s| s.unchecked_into())
        .map_err(camera_error)
}

/// The cameras the browser can see, as (device id, label).
async fn video_inputs() -> Vec<(String, String)> {
    let Some(promise) = media_devices().and_then(|d| d.enumerate_devices().ok()) else {
        return Vec::new();
    };
    let Ok(list) = JsFuture::from(promise).await else {
        return Vec::new();
    };
    js_sys::Array::from(&list)
        .iter()
        .filter_map(|d| d.dyn_into::<MediaDeviceInfo>().ok())
        .filter(|d| d.kind() == MediaDeviceKind::Videoinput)
        .enumerate()
        .map(|(i, d)| {
            let label = d.label();
            let label = if label.is_empty() {
                format!("Camera {}", i + 1)
            } else {
                label
            };
            (d.device_id(), label)
        })
        .collect()
}

/// The current picture as a JPEG.
async fn snapshot(
    video: &HtmlVideoElement,
    canvas: &HtmlCanvasElement,
    w: u32,
    h: u32,
) -> Result<Vec<u8>, String> {
    if canvas.width() != w || canvas.height() != h {
        canvas.set_width(w);
        canvas.set_height(h);
    }
    let ctx: CanvasRenderingContext2d = canvas
        .get_context("2d")
        .ok()
        .flatten()
        .ok_or("could not read the camera")?
        .unchecked_into();
    ctx.draw_image_with_html_video_element(video, 0.0, 0.0)
        .map_err(js_message)?;

    let (send, receive) = futures::channel::oneshot::channel::<Option<Blob>>();
    let mut send = Some(send);
    let done = Closure::<dyn FnMut(Option<Blob>)>::new(move |blob: Option<Blob>| {
        if let Some(send) = send.take() {
            let _ = send.send(blob);
        }
    });
    canvas
        .to_blob_with_type_and_encoder_options(
            done.as_ref().unchecked_ref(),
            "image/jpeg",
            &JsValue::from_f64(JPEG_QUALITY),
        )
        .map_err(js_message)?;
    let blob = receive
        .await
        .ok()
        .flatten()
        .ok_or("could not encode the camera picture")?;
    drop(done);
    let buffer = JsFuture::from(blob.array_buffer())
        .await
        .map_err(js_message)?;
    Ok(js_sys::Uint8Array::new(&buffer).to_vec())
}

fn socket_url(label: &str) -> String {
    let location = window().location();
    let scheme = match location.protocol().as_deref() {
        Ok("https:") => "wss",
        _ => "ws",
    };
    format!(
        "{scheme}://{}/api/camera?label={}",
        location.host().unwrap_or_default(),
        js_sys::encode_uri_component(label)
    )
}

/// getUserMedia's errors in words.
fn camera_error(e: JsValue) -> String {
    let name = js_sys::Reflect::get(&e, &"name".into())
        .ok()
        .and_then(|n| n.as_string())
        .unwrap_or_default();
    match name.as_str() {
        "NotAllowedError" => {
            "camera access was blocked: allow it in the browser's settings for this site".into()
        }
        "NotFoundError" | "OverconstrainedError" => "no camera found on this device".into(),
        "NotReadableError" => "the camera is in use by another program".into(),
        _ => js_message(e),
    }
}

fn js_message(e: JsValue) -> String {
    js_sys::Reflect::get(&e, &"message".into())
        .ok()
        .and_then(|m| m.as_string())
        .filter(|m| !m.is_empty())
        .or_else(|| e.as_string())
        .unwrap_or_else(|| "unknown error".into())
}
