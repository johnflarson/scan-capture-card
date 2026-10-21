//! Auto-scan trigger. A [`Watcher`] compares downscaled frames: once the
//! view has been still for a moment *and* differs from what was last
//! scanned, it fires. Moving a hand in or swapping a card therefore causes
//! exactly one new scan, and a card left in place is never scanned twice.

use std::time::{Duration, Instant};

use image::RgbImage;

use crate::pipeline::Rect;

/// How long the view must stay still before an auto-scan.
const STABLE_FOR: Duration = Duration::from_millis(800);
/// Max per-block mean grey difference (0-255) still counted as "still".
const MOTION_THRESHOLD: f32 = 10.0;
/// Min per-block difference from the last scan to count as a new scene.
const CHANGE_THRESHOLD: f32 = 18.0;
const BLOCK: usize = 8;

pub struct Watcher {
    tw: usize,
    th: usize,
    prev: Option<Vec<f32>>,
    still_since: Option<Instant>,
    last_scanned: Option<Vec<f32>>,
}

impl Watcher {
    pub fn new(region: Rect) -> Self {
        // ~8 thumbnail pixels per block, aiming for blocks smaller than a card.
        let tw = 160.min(region.w as usize).max(BLOCK);
        let th = ((tw as f32 * region.h as f32 / region.w as f32) as usize).max(BLOCK);
        Self {
            tw,
            th,
            prev: None,
            still_since: None,
            last_scanned: None,
        }
    }

    pub fn thumb(&self, frame: &RgbImage, r: Rect) -> Vec<f32> {
        let mut out = Vec::with_capacity(self.tw * self.th);
        for ty in 0..self.th {
            for tx in 0..self.tw {
                // Average a 3x3 sample grid inside the cell: cheap and noise-robust.
                let mut sum = 0.0;
                for sy in 0..3 {
                    for sx in 0..3 {
                        let x = r.x
                            + ((tx as f32 + (sx as f32 + 0.5) / 3.0) * r.w as f32 / self.tw as f32)
                                as u32;
                        let y = r.y
                            + ((ty as f32 + (sy as f32 + 0.5) / 3.0) * r.h as f32 / self.th as f32)
                                as u32;
                        let p = frame
                            .get_pixel(x.min(frame.width() - 1), y.min(frame.height() - 1))
                            .0;
                        sum += 0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32;
                    }
                }
                out.push(sum / 9.0);
            }
        }
        // Remove overall brightness so auto-exposure drift isn't "change".
        let mean = out.iter().sum::<f32>() / out.len() as f32;
        out.iter_mut().for_each(|v| *v -= mean);
        out
    }

    /// Largest mean absolute difference over BLOCKxBLOCK blocks, so a change
    /// confined to one card in a table full of cards is still noticed.
    fn block_diff(&self, a: &[f32], b: &[f32]) -> f32 {
        let mut worst = 0f32;
        for by in (0..self.th).step_by(BLOCK) {
            for bx in (0..self.tw).step_by(BLOCK) {
                let (mut sum, mut n) = (0f32, 0f32);
                for y in by..(by + BLOCK).min(self.th) {
                    for x in bx..(bx + BLOCK).min(self.tw) {
                        let i = y * self.tw + x;
                        sum += (a[i] - b[i]).abs();
                        n += 1.0;
                    }
                }
                worst = worst.max(sum / n);
            }
        }
        worst
    }

    /// Feed a frame; returns true when an auto-scan should start.
    pub fn observe(&mut self, thumb: Vec<f32>, can_scan: bool, now: Instant) -> bool {
        let moving = self
            .prev
            .as_ref()
            .is_none_or(|p| self.block_diff(p, &thumb) > MOTION_THRESHOLD);
        if moving {
            self.still_since = None;
        } else {
            self.still_since.get_or_insert(now);
        }
        self.prev = Some(thumb);
        let settled = self
            .still_since
            .is_some_and(|t| now.duration_since(t) >= STABLE_FOR);
        if !can_scan || !settled {
            return false;
        }
        let current = self.prev.as_ref().unwrap();
        let changed = self
            .last_scanned
            .as_ref()
            .is_none_or(|last| self.block_diff(last, current) > CHANGE_THRESHOLD);
        if changed {
            self.last_scanned = self.prev.clone();
        }
        changed
    }

    /// Record a manual scan so the same scene isn't auto-scanned again.
    pub fn mark_scanned(&mut self) {
        self.last_scanned = self.prev.clone();
    }

    /// Forget the last scan so the current scene triggers again.
    pub fn forget_scanned(&mut self) {
        self.last_scanned = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_with_card(card_at: Option<(u32, u32)>, shade: u8) -> RgbImage {
        RgbImage::from_fn(640, 360, |x, y| match card_at {
            Some((cx, cy)) if x >= cx && x < cx + 60 && y >= cy && y < cy + 84 => {
                image::Rgb([shade, shade, shade])
            }
            _ => image::Rgb([200, 200, 200]),
        })
    }

    /// Feeds frames 100ms apart, returning how many scans were triggered.
    struct Clock(Instant);
    impl Clock {
        fn tick(&mut self) -> Instant {
            self.0 += Duration::from_millis(100);
            self.0
        }
    }

    #[test]
    fn watcher_triggers_once_per_stable_new_scene() {
        let region = Rect {
            x: 0,
            y: 0,
            w: 640,
            h: 360,
        };
        let mut w = Watcher::new(region);
        let mut clock = Clock(Instant::now());
        let empty = frame_with_card(None, 0);
        let card = frame_with_card(Some((100, 100)), 30);
        let other = frame_with_card(Some((100, 100)), 120);

        let mut triggers = 0;
        for _ in 0..20 {
            triggers += w.observe(w.thumb(&empty, region), true, clock.tick()) as u32;
        }
        assert_eq!(triggers, 1, "first stable scene scans once");

        for _ in 0..20 {
            triggers += w.observe(w.thumb(&card, region), true, clock.tick()) as u32;
        }
        assert_eq!(triggers, 2, "a single new card is noticed");

        for _ in 0..20 {
            triggers += w.observe(w.thumb(&card, region), true, clock.tick()) as u32;
        }
        assert_eq!(triggers, 2, "unchanged scene is not rescanned");

        // Alternating frames = motion: never triggers.
        for i in 0..20 {
            let f = if i % 2 == 0 { &card } else { &other };
            triggers += w.observe(w.thumb(f, region), true, clock.tick()) as u32;
        }
        assert_eq!(triggers, 2, "no scan while things are moving");

        for _ in 0..20 {
            triggers += w.observe(w.thumb(&other, region), true, clock.tick()) as u32;
        }
        assert_eq!(triggers, 3, "swapped card scans once it settles");
    }

    #[test]
    fn watcher_waits_while_busy() {
        let region = Rect {
            x: 0,
            y: 0,
            w: 640,
            h: 360,
        };
        let mut w = Watcher::new(region);
        let mut clock = Clock(Instant::now());
        let card = frame_with_card(Some((100, 100)), 30);
        for _ in 0..20 {
            assert!(!w.observe(w.thumb(&card, region), false, clock.tick()));
        }
        assert!(
            w.observe(w.thumb(&card, region), true, clock.tick()),
            "scans as soon as it is free"
        );
    }

    #[test]
    fn watcher_ignores_global_brightness_change() {
        let region = Rect {
            x: 0,
            y: 0,
            w: 640,
            h: 360,
        };
        let mut w = Watcher::new(region);
        let mut clock = Clock(Instant::now());
        let card = frame_with_card(Some((100, 100)), 30);
        let brighter = RgbImage::from_fn(640, 360, |x, y| {
            let p = card.get_pixel(x, y).0;
            image::Rgb([p[0] + 25, p[1] + 25, p[2] + 25])
        });
        let mut triggers = 0;
        for _ in 0..20 {
            triggers += w.observe(w.thumb(&card, region), true, clock.tick()) as u32;
        }
        for _ in 0..20 {
            triggers += w.observe(w.thumb(&brighter, region), true, clock.tick()) as u32;
        }
        assert_eq!(triggers, 1);
    }
}
