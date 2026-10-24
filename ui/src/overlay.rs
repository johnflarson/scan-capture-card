//! The guide and detected-card boxes drawn over the camera image, from
//! `StateView::overlay`: the server sends shapes, not a picture with them
//! burnt in, so they work over any camera the page shows.

use leptos::prelude::*;
use mtg_scan_shared::{OverlayBox, OverlayView};

/// Title-line and info-line hints in the guide, so the card goes in the
/// right way up.
const HINT: &str = "#5dd3d3";
const FOIL: &str = "#ffd54f";

/// An SVG the size of the camera frame, laid over a picture shown with
/// `object-fit: contain`: both keep the frame's aspect ratio, so the shapes
/// line up with the picture.
#[component]
pub fn CameraOverlay(#[prop(into)] overlay: Signal<OverlayView>) -> impl IntoView {
    move || {
        let o = overlay.get();
        let (w, h) = (o.width, o.height);
        (w > 0 && h > 0).then(|| {
            // Line widths in camera pixels: about as thick at any resolution.
            let line = (h / 270).max(2);
            let outline = move |b: &OverlayBox| {
                view! {
                    <rect
                        x=b.x
                        y=b.y
                        width=b.w
                        height=b.h
                        fill="none"
                        stroke=b.color.clone()
                        stroke-width=line
                    />
                }
            };
            let guide = o.guide.as_ref().map(|g| {
                let (x2, y2) = (g.x + g.w, g.y + g.h);
                let hint = |frac: f32| {
                    let y = g.y as f32 + g.h as f32 * frac;
                    view! {
                        <line x1=g.x x2=x2 y1=y y2=y stroke=HINT stroke-width=(line / 3).max(1) />
                    }
                };
                view! {
                    // Everything outside the guide, dimmed.
                    <path
                        d=format!("M0 0H{w}V{h}H0Z M{} {}H{x2}V{y2}H{}Z", g.x, g.y, g.x)
                        fill="rgba(0, 0, 0, 0.5)"
                        fill-rule="evenodd"
                    />
                    {outline(g)}
                    {hint(0.13)}
                    {hint(0.88)}
                }
            });
            let foil = o.foil.then(|| {
                let s = h / 12;
                view! {
                    <rect
                        x=w.saturating_sub(s + 8)
                        y=8
                        width=s
                        height=s
                        fill="none"
                        stroke=FOIL
                        stroke-width=line * 2
                    />
                }
            });
            let dot = o.auto_dot.clone().map(|color| {
                let s = (h / 40).max(4);
                view! { <rect x=8 y=8 width=s height=s fill=color /> }
            });
            view! {
                <svg
                    class="overlay"
                    viewBox=format!("0 0 {w} {h}")
                    preserveAspectRatio="xMidYMid meet"
                    aria-hidden="true"
                >
                    {guide}
                    {o.boxes.iter().map(outline).collect_view()}
                    {foil}
                    {dot}
                </svg>
            }
        })
    }
}
