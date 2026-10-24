//! Preview frames: the camera image scaled for display, with the guide,
//! detected-card boxes and status markers drawn in. Used by both the
//! desktop window and the web UI's MJPEG stream.

use image::{Rgb, RgbImage};
use mtg_scan_shared::{OverlayBox, OverlayView};

use crate::pipeline::Rect;

pub type Color = [u8; 3];

// luminaldata palette, plus status colours that stay readable on a camera feed.
pub const IDLE: Color = [0xCD, 0xBA, 0x9A];
pub const BUSY: Color = [0xE8, 0x96, 0x1E];
pub const ADDED: Color = [0x4C, 0xBF, 0xBF];
pub const PRICED: Color = [0x8A, 0xB4, 0xF8];
pub const UNCHANGED: Color = [0x2E, 0x7D, 0x7D];
pub const REVIEW: Color = [0xF0, 0xA8, 0x32];
pub const FAILED: Color = [0xE5, 0x73, 0x73];
pub const HINT: Color = [0x5D, 0xD3, 0xD3];
pub const FOIL: Color = [0xFF, 0xD5, 0x4F];

/// What to draw on top of the camera image (coordinates in camera pixels).
#[derive(Debug, Clone, Default)]
pub struct Overlay {
    /// Single-card guide; everything outside it is dimmed.
    pub guide: Option<(Rect, Color)>,
    pub boxes: Vec<(Rect, Color)>,
    /// Foil or etched is selected for new cards.
    pub foil: bool,
    /// Auto-scan indicator (top-left dot) and its colour.
    pub auto_dot: Option<Color>,
}

/// `overlay` for the web page to draw over a `size` frame.
pub fn overlay_view((width, height): (u32, u32), overlay: &Overlay) -> OverlayView {
    let shape = |(r, c): &(Rect, Color)| OverlayBox {
        x: r.x,
        y: r.y,
        w: r.w,
        h: r.h,
        color: hex(*c),
    };
    OverlayView {
        width,
        height,
        guide: overlay.guide.as_ref().map(shape),
        boxes: overlay.boxes.iter().map(shape).collect(),
        foil: overlay.foil,
        auto_dot: overlay.auto_dot.map(hex),
    }
}

/// `#rrggbb`.
fn hex([r, g, b]: Color) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// Integer downscale factor that keeps previews around 1600-1920 px wide.
pub fn preview_step(width: u32) -> u32 {
    (width / 1600).max(1)
}

/// Downscale `frame` by `step` (nearest neighbour: cheap enough per frame)
/// and draw `overlay`.
pub fn compose(frame: &RgbImage, step: u32, overlay: &Overlay) -> RgbImage {
    let step = step.max(1);
    let (w, h) = (frame.width() / step, frame.height() / step);
    let mut img = RgbImage::from_fn(w, h, |x, y| *frame.get_pixel(x * step, y * step));
    let scale = |r: Rect| Rect {
        x: r.x / step,
        y: r.y / step,
        w: r.w / step,
        h: r.h / step,
    };
    let t = (h / 250).max(2);

    if let Some((guide, color)) = overlay.guide {
        let g = scale(guide);
        dim_outside(&mut img, g);
        outline(&mut img, g, t, color);
        // Title-line and info-line hints so the card goes in the right way up.
        for frac in [0.13, 0.88] {
            let y = g.y + (g.h as f32 * frac) as u32;
            outline(
                &mut img,
                Rect {
                    x: g.x,
                    y,
                    w: g.w,
                    h: 1,
                },
                1,
                HINT,
            );
        }
    }
    for (r, color) in &overlay.boxes {
        outline(&mut img, scale(*r), t, *color);
    }
    if overlay.foil {
        let s = h / 12;
        outline(
            &mut img,
            Rect {
                x: w.saturating_sub(s + 8),
                y: 8,
                w: s,
                h: s,
            },
            t * 2,
            FOIL,
        );
    }
    if let Some(color) = overlay.auto_dot {
        let s = (h / 40).max(4);
        outline(
            &mut img,
            Rect {
                x: 8,
                y: 8,
                w: s,
                h: s,
            },
            s / 2,
            color,
        );
    }
    img
}

fn dim_outside(img: &mut RgbImage, r: Rect) {
    for (x, y, p) in img.enumerate_pixels_mut() {
        if x < r.x || x >= r.x + r.w || y < r.y || y >= r.y + r.h {
            p.0 = p.0.map(|c| c / 2);
        }
    }
}

/// Rectangle outline of thickness `t`, clipped to the image.
pub fn outline(img: &mut RgbImage, r: Rect, t: u32, color: Color) {
    let (w, h) = img.dimensions();
    let x_end = (r.x + r.w).min(w);
    let y_end = (r.y + r.h).min(h);
    for y in r.y.min(h)..y_end {
        for x in r.x.min(w)..x_end {
            let edge = y < r.y + t || y + t >= r.y + r.h || x < r.x + t || x + t >= r.x + r.w;
            if edge {
                img.put_pixel(x, y, Rgb(color));
            }
        }
    }
}

/// Packed `0x00RRGGBB` pixels, as minifb wants them.
#[cfg(feature = "desktop")]
pub fn to_u32(img: &RgbImage, out: &mut Vec<u32>) {
    out.clear();
    out.extend(
        img.pixels()
            .map(|p| (p.0[0] as u32) << 16 | (p.0[1] as u32) << 8 | p.0[2] as u32),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_scales_and_draws() {
        let frame = RgbImage::from_pixel(400, 200, Rgb([100, 100, 100]));
        let overlay = Overlay {
            guide: Some((
                Rect {
                    x: 100,
                    y: 0,
                    w: 200,
                    h: 200,
                },
                ADDED,
            )),
            ..Overlay::default()
        };
        let img = compose(&frame, 2, &overlay);
        assert_eq!(img.dimensions(), (200, 100));
        // Outside the guide is dimmed, the guide edge is drawn.
        assert_eq!(img.get_pixel(10, 50).0, [50, 50, 50]);
        assert_eq!(img.get_pixel(50, 50).0, ADDED);
        assert_eq!(img.get_pixel(100, 50).0, [100, 100, 100]);
    }

    #[test]
    fn outline_is_clipped() {
        let mut img = RgbImage::new(10, 10);
        outline(
            &mut img,
            Rect {
                x: 5,
                y: 5,
                w: 20,
                h: 20,
            },
            1,
            FAILED,
        );
        assert_eq!(img.get_pixel(5, 9).0, FAILED);
        assert_eq!(img.get_pixel(9, 5).0, FAILED);
    }

    #[test]
    fn overlay_for_the_page_keeps_camera_pixels() {
        let guide = Rect {
            x: 10,
            y: 20,
            w: 30,
            h: 40,
        };
        let view = overlay_view(
            (1920, 1080),
            &Overlay {
                guide: Some((guide, ADDED)),
                boxes: vec![(guide, FAILED)],
                foil: true,
                auto_dot: Some(IDLE),
            },
        );
        assert_eq!((view.width, view.height), (1920, 1080));
        let g = view.guide.unwrap();
        assert_eq!(
            (g.x, g.y, g.w, g.h, g.color.as_str()),
            (10, 20, 30, 40, "#4cbfbf")
        );
        assert_eq!(view.boxes[0].color, "#e57373");
        assert_eq!(view.auto_dot.as_deref(), Some("#cdba9a"));
        assert!(view.foil);
    }

    #[test]
    fn preview_step_targets_display_width() {
        assert_eq!(preview_step(3840), 2);
        assert_eq!(preview_step(1920), 1);
        assert_eq!(preview_step(640), 1);
    }

    #[test]
    #[cfg(feature = "desktop")]
    fn packs_pixels_for_minifb() {
        let img = RgbImage::from_pixel(1, 1, Rgb([0x12, 0x34, 0x56]));
        let mut out = Vec::new();
        to_u32(&img, &mut out);
        assert_eq!(out, vec![0x0012_3456]);
    }
}
