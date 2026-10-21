//! A readable card image while the pointer is over a card name.
//!
//! `App` provides one [`Preview`] and renders [`PreviewLayer`]; card names
//! use [`CardLink`]. The image appears after a short pause (so sweeping over
//! a table doesn't flicker), follows the pointer, stays inside the window,
//! and disappears as soon as the pointer leaves the name. Touch input never
//! shows it: a tap just follows the link.

use gloo_timers::future::TimeoutFuture;
use leptos::ev::PointerEvent;
use leptos::prelude::*;
use leptos::task::spawn_local;

/// Shown size (Scryfall's "normal" image is 488×680).
const WIDTH: f64 = 244.0;
const HEIGHT: f64 = 340.0;
/// Space between pointer and image, and kept from the window edges.
const GAP: f64 = 18.0;
const DELAY_MS: u32 = 150;

#[derive(Clone, Copy)]
pub struct Preview {
    /// Scryfall ID of the card being previewed.
    card: RwSignal<Option<String>>,
    pointer: RwSignal<(f64, f64)>,
    /// Bumped on every enter/leave, so a pending show for a name the pointer
    /// already left is dropped.
    seq: StoredValue<u64>,
}

impl Preview {
    pub fn new() -> Self {
        Self {
            card: RwSignal::new(None),
            pointer: RwSignal::new((0.0, 0.0)),
            seq: StoredValue::new(0),
        }
    }

    fn enter(self, id: String, ev: &PointerEvent) {
        if ev.pointer_type() == "touch" {
            return;
        }
        self.pointer
            .set((ev.client_x() as f64, ev.client_y() as f64));
        let seq = self.seq.get_value() + 1;
        self.seq.set_value(seq);
        spawn_local(async move {
            TimeoutFuture::new(DELAY_MS).await;
            if self.seq.try_get_value() == Some(seq) {
                let _ = self.card.try_set(Some(id));
            }
        });
    }

    fn moved(self, ev: &PointerEvent) {
        if ev.pointer_type() != "touch" {
            self.pointer
                .set((ev.client_x() as f64, ev.client_y() as f64));
        }
    }

    fn leave(self) {
        self.seq.update_value(|s| *s += 1);
        self.card.set(None);
    }
}

/// Where the image goes: right of and centred on the pointer, flipped to the
/// left near the right edge, and kept inside the window vertically.
fn place((x, y): (f64, f64), (width, height): (f64, f64)) -> (f64, f64) {
    let left = if x + GAP + WIDTH + GAP <= width {
        x + GAP
    } else {
        (x - GAP - WIDTH).max(GAP)
    };
    let top = (y - HEIGHT / 2.0).min(height - HEIGHT - GAP).max(GAP);
    (left, top)
}

/// The floating image; rendered once by `App`.
#[component]
pub fn PreviewLayer(preview: Preview) -> impl IntoView {
    move || {
        preview.card.get().map(|id| {
            let style = move || {
                let size = window()
                    .inner_width()
                    .ok()
                    .and_then(|w| w.as_f64())
                    .zip(window().inner_height().ok().and_then(|h| h.as_f64()))
                    .unwrap_or((1280.0, 800.0));
                let (left, top) = place(preview.pointer.get(), size);
                format!("left: {left:.0}px; top: {top:.0}px")
            };
            view! {
                <img
                    class="card-preview"
                    style=style
                    // Scryfall redirects this to the card's image (front face).
                    src=format!("https://api.scryfall.com/cards/{id}?format=image&version=normal")
                    alt=""
                />
            }
        })
    }
}

/// A card name linking to Scryfall that previews the card on hover.
#[component]
pub fn CardLink(
    /// Scryfall ID (the preview image).
    id: String,
    name: String,
    href: String,
) -> impl IntoView {
    let preview = use_context::<Preview>();
    let enter = move |ev: PointerEvent| {
        if let Some(p) = preview {
            p.enter(id.clone(), &ev);
        }
    };
    let moved = move |ev: PointerEvent| {
        if let Some(p) = preview {
            p.moved(&ev);
        }
    };
    let hide = move || {
        if let Some(p) = preview {
            p.leave();
        }
    };
    view! {
        <a
            href=href
            target="_blank"
            rel="noopener"
            on:pointerenter=enter
            on:pointermove=moved
            on:pointerleave=move |_| hide()
            on:click=move |_| hide()
        >
            {name}
        </a>
    }
}
