//! Find cards on the table without a vision LLM.
//!
//! Magic cards have a thick black border. The detector:
//! 1. marks *dark grey* pixels: dark outright, or (under glare) much darker
//!    than their surroundings; coloured artwork and a coloured mat don't
//!    count because the pixel must be grey;
//! 2. removes thin dark lines (a playmat's line art, text) with a
//!    morphological opening and bridges small gaps in borders with a closing;
//! 3. scores every card-shaped rectangle (63:88, upright or sideways, many
//!    sizes) on a coarse grid: along at least three of its sides most of the
//!    length must run over dark pixels (a border), and its inside must not be
//!    mostly dark (a mouse, a phone);
//! 4. keeps the best rectangle where several overlap.

use image::{GrayImage, RgbImage, imageops};

use crate::llm::NormBox;

/// The mask is made at this width (a card is then ~90-300 px wide)…
const WORK_WIDTH: u32 = 1920;
/// …and rectangles are searched on a grid this many times coarser.
const STEP: u32 = 4;
/// A pixel is dark when its brightest channel is below this…
const DARK: u8 = 110;
/// …or below `GLARE_DARK` and under `RELATIVE` times its neighbourhood…
const GLARE_DARK: u8 = 170;
const RELATIVE: f32 = 0.7;
/// …(the neighbourhood: a square this many pixels from the centre)…
const LOCAL_RADIUS: u32 = 12;
/// …and it is grey rather than coloured (channel spread below this).
const GREY: u8 = 45;
const CARD_ASPECT: f32 = 63.0 / 88.0;
/// Card widths tried, as a share of the frame's short side.
const MIN_WIDTH: f32 = 0.05;
const MAX_WIDTH: f32 = 0.35;
/// A side counts when this share of its length runs over a border.
const SIDE_HIT: f32 = 0.75;
/// The inside may be at most this dark (dark artwork is fine, a solid
/// object isn't).
const MAX_INSIDE: f32 = 0.15;

/// Card boxes in `frame`, normalised 0..1, in reading order (rows, then left
/// to right).
// Not used by the table pipeline yet: it doesn't meet the desk-photo bar
// (9 of 10 cards) — see issue #5. `mtg-scan detect-dump` exercises it.
#[allow(dead_code)]
pub fn find_cards(frame: &RgbImage) -> Vec<NormBox> {
    analyse(frame).boxes
}

/// What [`find_cards`] saw, for `mtg-scan detect-dump`.
pub struct Analysis {
    pub boxes: Vec<NormBox>,
    /// The border mask the rectangles were scored on.
    pub mask: GrayImage,
}

pub fn analyse(frame: &RgbImage) -> Analysis {
    let (w0, h0) = frame.dimensions();
    if w0 < 16 || h0 < 16 {
        return Analysis {
            boxes: Vec::new(),
            mask: GrayImage::new(1, 1),
        };
    }
    let scale = (WORK_WIDTH as f32 / w0 as f32).min(1.0);
    let (w, h) = (
        ((w0 as f32 * scale).round() as u32).max(1),
        ((h0 as f32 * scale).round() as u32).max(1),
    );
    let small = imageops::resize(frame, w, h, imageops::FilterType::Triangle);
    let mask = border_mask(&small);
    let grid = max_pool(&mask, STEP);
    let found = search(&grid);
    let (gw, gh) = grid.dimensions();
    let boxes = found
        .into_iter()
        .map(|r| NormBox {
            x1: r.x as f32 / gw as f32,
            y1: r.y as f32 / gh as f32,
            x2: (r.x + r.w) as f32 / gw as f32,
            y2: (r.y + r.h) as f32 / gh as f32,
        })
        .collect();
    let mask = GrayImage::from_fn(w, h, |x, y| {
        image::Luma([if mask.get_pixel(x, y).0[0] != 0 {
            255
        } else {
            0
        }])
    });
    Analysis { boxes, mask }
}

/// Dark grey pixels that survive removing thin lines; gaps bridged.
fn border_mask(img: &RgbImage) -> GrayImage {
    let (w, h) = img.dimensions();
    let brightest = GrayImage::from_fn(w, h, |x, y| {
        let p = img.get_pixel(x, y).0;
        image::Luma([p[0].max(p[1]).max(p[2])])
    });
    let around = box_mean(&brightest, LOCAL_RADIUS);
    let dark = GrayImage::from_fn(w, h, |x, y| {
        let p = img.get_pixel(x, y).0;
        let (hi, lo) = (p[0].max(p[1]).max(p[2]), p[0].min(p[1]).min(p[2]));
        let local = around.get_pixel(x, y).0[0] as f32;
        let dark = hi < DARK || (hi < GLARE_DARK && (hi as f32) < local * RELATIVE);
        image::Luma([u8::from(dark && hi - lo < GREY)])
    });
    // Lines up to ~2 px wide go; borders (~4-8 px) stay; small gaps close.
    erode(&dilate(&dilate(&erode(&dark, 1), 1), 2), 2)
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Found {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    score: f32,
}

impl Found {
    fn intersection(&self, o: &Found) -> u32 {
        let ix = (self.x + self.w)
            .min(o.x + o.w)
            .saturating_sub(self.x.max(o.x));
        let iy = (self.y + self.h)
            .min(o.y + o.h)
            .saturating_sub(self.y.max(o.y));
        ix * iy
    }

    /// Overlap as a share of the smaller box.
    fn overlap(&self, o: &Found) -> f32 {
        self.intersection(o) as f32 / (self.w * self.h).min(o.w * o.h) as f32
    }
}

/// Sum of set pixels in any rectangle in O(1).
struct Integral {
    w: usize,
    sum: Vec<u32>,
}

impl Integral {
    fn new(mask: &GrayImage) -> Self {
        let (w, h) = (mask.width() as usize, mask.height() as usize);
        let mut sum = vec![0u32; (w + 1) * (h + 1)];
        for y in 0..h {
            let mut row = 0;
            for x in 0..w {
                row += u32::from(mask.get_pixel(x as u32, y as u32).0[0] != 0);
                sum[(y + 1) * (w + 1) + x + 1] = sum[y * (w + 1) + x + 1] + row;
            }
        }
        Self { w, sum }
    }

    /// Set pixels in `[x0, x1) × [y0, y1)`.
    fn count(&self, x0: u32, y0: u32, x1: u32, y1: u32) -> u32 {
        let at = |x: u32, y: u32| self.sum[y as usize * (self.w + 1) + x as usize];
        at(x1, y1) + at(x0, y0) - at(x1, y0) - at(x0, y1)
    }
}

/// Every card-shaped rectangle scored; the best non-overlapping ones kept,
/// in reading order.
fn search(grid: &GrayImage) -> Vec<Found> {
    let (gw, gh) = grid.dimensions();
    let ii = Integral::new(grid);
    let short = gw.min(gh) as f32;
    let mut candidates = Vec::new();
    let mut width = (short * MIN_WIDTH).max(6.0);
    while width <= short * MAX_WIDTH {
        let cw = width.round() as u32;
        let ch = (width / CARD_ASPECT).round() as u32;
        for (rw, rh) in [(cw, ch), (ch, cw)] {
            if rw >= gw || rh >= gh {
                continue;
            }
            for y in 0..=gh - rh {
                for x in 0..=gw - rw {
                    if let Some(score) = score(&ii, x, y, rw, rh, gw, gh) {
                        candidates.push(Found {
                            x,
                            y,
                            w: rw,
                            h: rh,
                            score,
                        });
                    }
                }
            }
        }
        width *= 1.04;
    }
    // Best first; between equal scores the larger box (the card's outline
    // rather than its art frame).
    candidates.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| (b.w * b.h).cmp(&(a.w * a.h)))
    });
    let mut kept: Vec<Found> = Vec::new();
    for c in candidates {
        if kept.iter().all(|k| k.overlap(&c) < 0.3) {
            kept.push(c);
        }
    }
    // Reading order: rows (half a card's height apart), then left to right.
    let row = kept.iter().map(|b| b.h).max().unwrap_or(1) as f32 * 0.5;
    kept.sort_by_key(|b| ((b.y as f32 / row) as u32, b.x));
    kept
}

/// How much the rectangle looks like a card border, or `None` if it doesn't.
fn score(ii: &Integral, x: u32, y: u32, w: u32, h: u32, gw: u32, gh: u32) -> Option<f32> {
    // Bands along each side, thick enough for a slight tilt.
    let t = ((w.min(h) as f32 * 0.1).round() as u32).max(2);
    let (x1, y1) = (x + w, y + h);
    // An edge has table, not more border, just outside it. Share of dark
    // pixels in the strip outside each side (1.0 at the frame's edge).
    let outside = |horizontal: bool, before: bool| -> f32 {
        let (x0, y0, x1o, y1o) = match (horizontal, before) {
            (true, true) if y >= t => (x, y - t, x1, y),
            (true, false) if y1 + t <= gh => (x, y1, x1, y1 + t),
            (false, true) if x >= t => (x - t, y, x, y1),
            (false, false) if x1 + t <= gw => (x1, y, x1 + t, y1),
            _ => return 1.0,
        };
        ii.count(x0, y0, x1o, y1o) as f32 / ((x1o - x0) * (y1o - y0)) as f32
    };
    let clear = [
        outside(true, true),
        outside(true, false),
        outside(false, true),
        outside(false, false),
    ]
    .iter()
    .filter(|&&d| d < 0.2)
    .count();
    if clear < 3 {
        return None;
    }
    // A side's coverage: the share of short segments along it that contain
    // about a line's worth of border pixels.
    let line = (w.min(h) as f32 * 0.045 * 0.5).max(0.7);
    let side = |horizontal: bool, at: u32| -> f32 {
        let len = if horizontal { w } else { h };
        let parts = 8;
        let seg = (len / parts).max(1);
        let hits = (0..parts)
            .filter(|&i| {
                let s0 = (i * seg).min(len - 1);
                let s1 = if i == parts - 1 {
                    len
                } else {
                    (s0 + seg).min(len)
                };
                let n = if horizontal {
                    ii.count(x + s0, at, x + s1, at + t)
                } else {
                    ii.count(at, y + s0, at + t, y + s1)
                };
                // A border is ~4.5% of the card's width thick; at least
                // half of that along the whole segment.
                n as f32 >= (s1 - s0) as f32 * line
            })
            .count();
        hits as f32 / parts as f32
    };
    let mut sides = [
        side(true, y),
        side(true, y1 - t),
        side(false, x),
        side(false, x1 - t),
    ];
    sides.sort_by(|a, b| b.total_cmp(a));
    // Three solid sides (glare can wash out the fourth).
    if sides[2] < SIDE_HIT {
        return None;
    }
    let inside_area = ((w - 2 * t) * (h - 2 * t)).max(1) as f32;
    let inside = ii.count(x + t, y + t, x1 - t, y1 - t) as f32 / inside_area;
    if inside > MAX_INSIDE {
        return None;
    }
    Some(sides[0] + sides[1] + sides[2] + 0.5 * sides[3] - inside)
}

/// Maximum over `step`×`step` blocks, so thin borders survive shrinking.
fn max_pool(mask: &GrayImage, step: u32) -> GrayImage {
    let (w, h) = mask.dimensions();
    let (gw, gh) = (w.div_ceil(step), h.div_ceil(step));
    GrayImage::from_fn(gw, gh, |gx, gy| {
        let any = (gy * step..((gy + 1) * step).min(h))
            .any(|y| (gx * step..((gx + 1) * step).min(w)).any(|x| mask.get_pixel(x, y).0[0] != 0));
        image::Luma([u8::from(any)])
    })
}

/// Mean over a (2r+1)² square (clamped at the edges), via an integral image.
fn box_mean(img: &GrayImage, r: u32) -> GrayImage {
    let (w, h) = img.dimensions();
    let (wu, hu) = (w as usize, h as usize);
    let mut sum = vec![0u64; (wu + 1) * (hu + 1)];
    for y in 0..hu {
        let mut row = 0u64;
        for x in 0..wu {
            row += img.get_pixel(x as u32, y as u32).0[0] as u64;
            sum[(y + 1) * (wu + 1) + x + 1] = sum[y * (wu + 1) + x + 1] + row;
        }
    }
    GrayImage::from_fn(w, h, |x, y| {
        let (x0, y0) = (x.saturating_sub(r) as usize, y.saturating_sub(r) as usize);
        let (x1, y1) = ((x + r + 1).min(w) as usize, (y + r + 1).min(h) as usize);
        let total = sum[y1 * (wu + 1) + x1] + sum[y0 * (wu + 1) + x0]
            - sum[y0 * (wu + 1) + x1]
            - sum[y1 * (wu + 1) + x0];
        image::Luma([(total / ((x1 - x0) * (y1 - y0)) as u64) as u8])
    })
}

/// Minimum (`erode`) or maximum (`dilate`) over a (2r+1)² square, done as a
/// horizontal then a vertical pass.
fn morph(mask: &GrayImage, r: u32, keep: fn(bool, bool) -> bool, start: bool) -> GrayImage {
    let (w, h) = mask.dimensions();
    let get = |m: &GrayImage, x: u32, y: u32| m.get_pixel(x, y).0[0] != 0;
    let pass = |src: &GrayImage, horizontal: bool| {
        GrayImage::from_fn(w, h, |x, y| {
            let mut v = start;
            for d in -(r as i64)..=r as i64 {
                let (nx, ny) = if horizontal {
                    (x as i64 + d, y as i64)
                } else {
                    (x as i64, y as i64 + d)
                };
                // Outside the image counts as "not dark".
                let p = nx >= 0
                    && ny >= 0
                    && nx < w as i64
                    && ny < h as i64
                    && get(src, nx as u32, ny as u32);
                v = keep(v, p);
            }
            image::Luma([u8::from(v)])
        })
    };
    pass(&pass(mask, true), false)
}

fn erode(mask: &GrayImage, r: u32) -> GrayImage {
    morph(mask, r, |a, b| a && b, true)
}

fn dilate(mask: &GrayImage, r: u32) -> GrayImage {
    morph(mask, r, |a, b| a || b, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    /// A light "table" with thin dark lines, a solid dark blob and cards
    /// (black border, coloured inside) at given pixel boxes.
    fn scene(cards: &[(u32, u32, u32, u32)]) -> RgbImage {
        let mut img = RgbImage::from_pixel(1280, 720, Rgb([230, 225, 215]));
        // Line art: 2 px dark lines across the table, touching the cards.
        for x in 0..1280 {
            for t in 0..2 {
                img.put_pixel(x, 300 + t, Rgb([20, 20, 20]));
                img.put_pixel(x, (x / 3 + 50 + t).min(719), Rgb([20, 20, 20]));
            }
        }
        // A mouse: solid dark, roughly card-shaped.
        for y in 500..640 {
            for x in 1100..1200 {
                img.put_pixel(x, y, Rgb([15, 15, 15]));
            }
        }
        for &(x0, y0, cw, ch) in cards {
            let border = cw.min(ch) / 18;
            for y in y0..y0 + ch {
                for x in x0..x0 + cw {
                    let edge = x < x0 + border
                        || x >= x0 + cw - border
                        || y < y0 + border
                        || y >= y0 + ch - border;
                    let c = if edge {
                        Rgb([10, 10, 10])
                    } else {
                        Rgb([60, 110, 190])
                    };
                    img.put_pixel(x, y, c);
                }
            }
        }
        img
    }

    #[test]
    fn finds_bordered_cards_and_ignores_lines_and_blobs() {
        let cards = [
            (100, 100, 126, 176),
            (300, 120, 126, 176),
            (500, 380, 126, 176),
            // Sideways.
            (750, 400, 176, 126),
        ];
        let found = find_cards(&scene(&cards));
        assert_eq!(found.len(), 4, "{found:?}");
        for (b, &(x, y, cw, ch)) in found.iter().zip(&cards) {
            // Within about a grid step.
            let tol = STEP as f32 + 4.0;
            assert!((b.x1 * 1280.0 - x as f32).abs() <= tol, "{b:?}");
            assert!((b.y1 * 720.0 - y as f32).abs() <= tol, "{b:?}");
            assert!((b.x2 * 1280.0 - (x + cw) as f32).abs() <= tol, "{b:?}");
            assert!((b.y2 * 720.0 - (y + ch) as f32).abs() <= tol, "{b:?}");
        }
    }

    #[test]
    fn empty_table_has_no_cards() {
        assert!(find_cards(&scene(&[])).is_empty());
    }
}
