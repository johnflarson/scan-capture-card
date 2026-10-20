//! Live camera window with continuous, hands-free scanning.
//!
//! A [`Watcher`] compares downscaled frames: once the view has been still for
//! a moment *and* differs from what was last scanned, a scan is triggered
//! automatically. Moving a hand in or swapping a card therefore causes
//! exactly one new scan, and a card left in place is never scanned twice.
//!
//! Two modes:
//! * `Single` — one card inside an on-screen guide (OCR first, LLM fallback).
//! * `Table`  — any number of cards laid out in view (LLM detects them).
//!
//! All recognition runs on a worker thread so the preview stays smooth.

use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use image::RgbImage;
use minifb::{Key, KeyRepeat, ScaleMode, Window, WindowOptions};

use crate::camera::{self, Webcam};
use crate::config::Config;
use crate::model::{Finish, format_usd};
use crate::pipeline::{CardResult, Outcome, Pipeline, Rect};
use crate::store::totals;

const CARD_ASPECT: f32 = 63.0 / 88.0;
/// How long the view must stay still before an auto-scan.
const STABLE_FOR: Duration = Duration::from_millis(800);
/// Max per-block mean grey difference (0-255) still counted as "still".
const MOTION_THRESHOLD: f32 = 10.0;
/// Min per-block difference from the last scan to count as a new scene.
const CHANGE_THRESHOLD: f32 = 18.0;
const BLOCK: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveMode {
    Single,
    Table,
}

pub struct LiveOptions {
    pub mode: LiveMode,
    pub finish: Finish,
    pub auto: bool,
    pub dry_run: bool,
}

enum Job {
    Single {
        image: image::DynamicImage,
        finish: Finish,
        force_llm: bool,
        auto: bool,
    },
    Table {
        frame: RgbImage,
        finish: Finish,
    },
    Undo,
    Reset,
}

enum Event {
    Ready(String),
    /// Partial (`done == false`) or final results of a scan.
    Results {
        results: Vec<CardResult>,
        done: bool,
        auto: bool,
    },
    Message(String),
    Failed(String),
}

// ---------------------------------------------------------------- watcher --

struct Watcher {
    tw: usize,
    th: usize,
    prev: Option<Vec<f32>>,
    still_since: Option<Instant>,
    last_scanned: Option<Vec<f32>>,
}

impl Watcher {
    fn new(region: Rect) -> Self {
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

    fn thumb(&self, frame: &RgbImage, r: Rect) -> Vec<f32> {
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
    fn observe(&mut self, thumb: Vec<f32>, can_scan: bool, now: Instant) -> bool {
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
    fn mark_scanned(&mut self) {
        self.last_scanned = self.prev.clone();
    }
}

// ------------------------------------------------------------------- run --

pub fn run(cfg: &Config, opts: LiveOptions) -> Result<()> {
    let mut cam = Webcam::open(cfg.camera.index, cfg.camera.width, cfg.camera.height)?;
    let fmt = cam.format();
    println!(
        "Camera: {} ({}x{} {} @ {}fps)",
        cam.name,
        fmt.width(),
        fmt.height(),
        fmt.format(),
        fmt.frame_rate()
    );
    println!(
        "Collection: {}{}",
        cfg.collection.display(),
        if opts.dry_run {
            " (dry run: nothing is saved)"
        } else {
            ""
        }
    );
    let keys = match opts.mode {
        LiveMode::Single => {
            "SPACE scan now · A auto on/off · F finish · L retry with LLM · U undo · Q quit"
        }
        LiveMode::Table => {
            "SPACE scan now · A auto on/off · F finish · R rescan all · U undo last pass · Q quit"
        }
    };
    println!("Keys: {keys}");
    println!(
        "Tip: turn off AI tracking / auto-framing in Insta360 Link Controller so the camera stays still.\n"
    );

    let (job_tx, job_rx) = mpsc::channel::<Job>();
    let (event_tx, event_rx) = mpsc::channel::<Event>();
    let worker_cfg = cfg.clone();
    let dry_run = opts.dry_run;
    let worker = thread::spawn(move || worker(worker_cfg, dry_run, job_rx, event_tx));

    let mut frame = camera::warm_frame(&mut cam)?;
    let (fw, fh) = frame.dimensions();
    // Show large frames at half size to keep the preview fast.
    let step = if fw > 2000 { 2 } else { 1 };
    let (dw, dh) = (fw / step, fh / step);

    let guide = match opts.mode {
        LiveMode::Single => {
            let h = ((fh as f32 * cfg.camera.guide_height.clamp(0.2, 1.0)) as u32).min(fh);
            let w = ((h as f32 * CARD_ASPECT) as u32).min(fw);
            Rect {
                x: (fw - w) / 2,
                y: (fh - h) / 2,
                w,
                h,
            }
        }
        LiveMode::Table => Rect {
            x: 0,
            y: 0,
            w: fw,
            h: fh,
        },
    };
    let mut watcher = Watcher::new(guide);

    let mut window = Window::new(
        "mtg-scan",
        1280,
        720,
        WindowOptions {
            resize: true,
            scale_mode: ScaleMode::AspectRatioStretch,
            ..WindowOptions::default()
        },
    )
    .context("failed to open preview window")?;
    window.set_target_fps(30);

    let mut finish = opts.finish;
    let mut auto = opts.auto;
    let mut ready = false;
    let mut busy = true;
    let mut status = String::from("loading…");
    let mut overlays: Vec<(Rect, u32)> = Vec::new();
    let mut guide_color = COLOR_IDLE;
    let mut buffer = vec![0u32; (dw * dh) as usize];

    while window.is_open() && !window.is_key_down(Key::Escape) && !window.is_key_down(Key::Q) {
        // --- worker events
        loop {
            match event_rx.try_recv() {
                Ok(Event::Ready(msg)) => {
                    ready = true;
                    busy = false;
                    status = if auto {
                        "watching for cards".into()
                    } else {
                        "ready".into()
                    };
                    println!("{msg}");
                }
                Ok(Event::Results {
                    results,
                    done,
                    auto: was_auto,
                }) => {
                    overlays = results
                        .iter()
                        .map(|r| (r.rect, outcome_color(&r.outcome)))
                        .collect();
                    if done {
                        busy = false;
                        let summary = report(&results, opts.mode, was_auto);
                        status = summary.clone();
                        guide_color = results
                            .first()
                            .map(|r| outcome_color(&r.outcome))
                            .unwrap_or(COLOR_IDLE);
                        if opts.mode == LiveMode::Single {
                            overlays.clear();
                        }
                    } else if let Some(last) = results.last() {
                        println!("  [{}] {}", results.len(), last.summary());
                    }
                }
                Ok(Event::Message(msg)) => {
                    busy = false;
                    status = msg.clone();
                    println!("{msg}");
                }
                Ok(Event::Failed(msg)) => {
                    busy = false;
                    guide_color = COLOR_FAILED;
                    status = "error — see console".into();
                    println!("✗ {msg}");
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    return match worker.join() {
                        Ok(Ok(())) => Err(anyhow!("scanner worker stopped unexpectedly")),
                        Ok(Err(e)) => Err(e),
                        Err(_) => Err(anyhow!("scanner worker panicked")),
                    };
                }
            }
        }

        // --- keys
        let can_scan = ready && !busy;
        let mut trigger: Option<(bool, bool)> = None; // (force_llm, auto)
        if can_scan {
            if window.is_key_pressed(Key::Space, KeyRepeat::No)
                || window.is_key_pressed(Key::Enter, KeyRepeat::No)
            {
                trigger = Some((false, false));
            } else if opts.mode == LiveMode::Single && window.is_key_pressed(Key::L, KeyRepeat::No)
            {
                trigger = Some((true, false));
            } else if window.is_key_pressed(Key::U, KeyRepeat::No) {
                job_tx.send(Job::Undo)?;
                busy = true;
            } else if opts.mode == LiveMode::Table && window.is_key_pressed(Key::R, KeyRepeat::No) {
                job_tx.send(Job::Reset)?;
                watcher.last_scanned = None;
                busy = true;
            }
        }
        if window.is_key_pressed(Key::F, KeyRepeat::No) {
            finish = finish.next();
            println!("Finish for new cards: {finish}");
        }
        if window.is_key_pressed(Key::A, KeyRepeat::No) {
            auto = !auto;
            println!("Auto-scan {}", if auto { "on" } else { "off" });
        }

        // --- camera + motion watch
        match cam.frame() {
            Ok(f) if f.dimensions() == (fw, fh) => frame = f,
            Ok(_) => {}
            Err(e) => eprintln!("  ! {e:#}"),
        }
        let thumb = watcher.thumb(&frame, guide);
        if watcher.observe(thumb, auto && can_scan && trigger.is_none(), Instant::now()) {
            trigger = Some((false, true));
        } else if trigger.is_some() {
            watcher.mark_scanned();
        }

        if let Some((force_llm, is_auto)) = trigger {
            busy = true;
            guide_color = COLOR_BUSY;
            status = "scanning…".into();
            match opts.mode {
                LiveMode::Single => job_tx.send(Job::Single {
                    image: guide.crop(&frame),
                    finish,
                    force_llm,
                    auto: is_auto,
                })?,
                LiveMode::Table => {
                    println!("\nScanning table…");
                    overlays.clear();
                    job_tx.send(Job::Table {
                        frame: frame.clone(),
                        finish,
                    })?
                }
            }
        }

        // --- draw
        let mode = match opts.mode {
            LiveMode::Single => "SINGLE",
            LiveMode::Table => "TABLE",
        };
        window.set_title(&format!(
            "mtg-scan [{mode}{}{}] [{}] — {status}",
            if auto { " AUTO" } else { "" },
            if opts.dry_run { " DRY-RUN" } else { "" },
            finish.as_str().to_uppercase()
        ));
        draw_frame(&frame, &mut buffer, step);
        let scale = |r: Rect| Rect {
            x: r.x / step,
            y: r.y / step,
            w: r.w / step,
            h: r.h / step,
        };
        let t = (dh / 250).max(2);
        if opts.mode == LiveMode::Single {
            let g = scale(guide);
            dim_outside(&mut buffer, dw, g);
            draw_rect(
                &mut buffer,
                dw,
                g,
                t,
                if busy { COLOR_BUSY } else { guide_color },
            );
            // Title / info-line hints so the card is aligned the right way up.
            let band = |frac: f32| g.y + (g.h as f32 * frac) as u32;
            draw_rect(
                &mut buffer,
                dw,
                Rect {
                    x: g.x,
                    y: band(0.13),
                    w: g.w,
                    h: 1,
                },
                1,
                COLOR_HINT,
            );
            draw_rect(
                &mut buffer,
                dw,
                Rect {
                    x: g.x,
                    y: band(0.88),
                    w: g.w,
                    h: 1,
                },
                1,
                COLOR_HINT,
            );
        }
        for (r, color) in &overlays {
            draw_rect(&mut buffer, dw, scale(*r), t, *color);
        }
        if finish != Finish::Nonfoil {
            draw_rect(
                &mut buffer,
                dw,
                Rect {
                    x: dw - dh / 12 - 8,
                    y: 8,
                    w: dh / 12,
                    h: dh / 12,
                },
                t * 2,
                COLOR_FOIL,
            );
        }
        if auto {
            // Small status dot: grey = watching, yellow = scanning.
            let dot = Rect {
                x: 8,
                y: 8,
                w: dh / 40,
                h: dh / 40,
            };
            draw_rect(
                &mut buffer,
                dw,
                dot,
                dh / 80,
                if busy { COLOR_BUSY } else { COLOR_IDLE },
            );
        }
        window.update_with_buffer(&buffer, dw as usize, dh as usize)?;
    }

    drop(job_tx);
    let _ = worker.join();
    Ok(())
}

/// Print a scan's results; returns a one-line status for the window title.
fn report(results: &[CardResult], mode: LiveMode, auto: bool) -> String {
    if mode == LiveMode::Single {
        return match results.first() {
            Some(
                r @ CardResult {
                    outcome: Outcome::Failed(_),
                    ..
                },
            ) if auto => {
                // Quiet for auto-scans of an empty guide.
                println!("· nothing recognised in the guide");
                let _ = r;
                "watching — nothing recognised".into()
            }
            Some(r) => {
                println!("{}", r.summary());
                r.summary().lines().next().unwrap_or_default().to_string()
            }
            None => "nothing scanned".into(),
        };
    }
    let (mut new, mut same, mut failed) = (0, 0, 0);
    let (mut tcg, mut ck) = (0.0, 0.0);
    for r in results {
        match &r.outcome {
            Outcome::Added { entry, .. } => {
                new += 1;
                tcg += entry.prices.tcgplayer.unwrap_or(0.0);
                ck += entry.prices.cardkingdom.unwrap_or(0.0);
            }
            Outcome::Priced { prices, .. } => {
                new += 1;
                tcg += prices.tcgplayer.unwrap_or(0.0);
                ck += prices.cardkingdom.unwrap_or(0.0);
            }
            Outcome::Unchanged { .. } => same += 1,
            Outcome::Failed(_) => failed += 1,
        }
    }
    let line = format!(
        "{} cards: {new} new (TCG {} · CK {}), {same} unchanged, {failed} unrecognised",
        results.len(),
        format_usd(Some(tcg)),
        format_usd(Some(ck))
    );
    println!("{line}");
    line
}

// ---------------------------------------------------------------- worker --

fn worker(cfg: Config, dry_run: bool, jobs: Receiver<Job>, events: Sender<Event>) -> Result<()> {
    let mut pipeline = Pipeline::new(&cfg)?;
    // Warm the Card Kingdom cache now rather than during the first scan.
    let _ = pipeline.app.prices.cardkingdom.price("", Finish::Nonfoil);
    let _ = events.send(Event::Ready(format!(
        "Recognizer ready ({}). Lay out cards and hold still — scanning is automatic.",
        pipeline.recognizer.describe()
    )));

    for job in jobs {
        let event = match job {
            Job::Single {
                image,
                finish,
                force_llm,
                auto,
            } => match pipeline.scan_single(&image, finish, force_llm, dry_run) {
                Ok(r) => Event::Results {
                    results: vec![r],
                    done: true,
                    auto,
                },
                Err(e) => Event::Failed(format!("{e:#}")),
            },
            Job::Table { frame, finish } => {
                let progress_tx = events.clone();
                let result = pipeline.scan_table(&frame, finish, dry_run, |partial| {
                    let _ = progress_tx.send(Event::Results {
                        results: partial.to_vec(),
                        done: false,
                        auto: true,
                    });
                });
                match result {
                    Ok(results) => Event::Results {
                        results,
                        done: true,
                        auto: true,
                    },
                    Err(e) => Event::Failed(format!("{e:#}")),
                }
            }
            Job::Undo => match pipeline.undo() {
                Ok(undone) if undone.is_empty() => Event::Message("nothing to undo".into()),
                Ok(undone) => Event::Message(format!("↶ removed {}", undone.join(", "))),
                Err(e) => Event::Failed(format!("undo failed: {e:#}")),
            },
            Job::Reset => {
                pipeline.reset_memory();
                Event::Message("memory cleared — every card will be added on the next pass".into())
            }
        };
        let _ = events.send(event);
    }
    Ok(())
}

// --------------------------------------------------------------- one-shot --

/// Scan a table once (from the camera or an image file), print results.
pub fn scan_table_once(
    cfg: &Config,
    image: Option<&Path>,
    finish: Finish,
    dry_run: bool,
) -> Result<()> {
    let frame = match image {
        Some(path) => image::open(path)
            .with_context(|| format!("failed to open {}", path.display()))?
            .to_rgb8(),
        None => {
            let mut cam = Webcam::open(cfg.camera.index, cfg.camera.width, cfg.camera.height)?;
            println!("Capturing from {}…", cam.name);
            camera::warm_frame(&mut cam)?
        }
    };
    let mut pipeline = Pipeline::new(cfg)?;
    println!("Finding cards ({})…", pipeline.recognizer.describe());
    let mut printed = 0;
    let results = pipeline.scan_table(&frame, finish, dry_run, |partial| {
        for (i, r) in partial.iter().enumerate().skip(printed) {
            println!("  [{}] {}", i + 1, r.summary());
        }
        printed = partial.len();
    })?;
    println!();
    report(&results, LiveMode::Table, false);
    if !dry_run {
        let all = pipeline.app.store.entries()?;
        let t = totals(&all);
        println!(
            "Collection ({}): {} cards · TCGplayer ${:.2} · Card Kingdom ${:.2}",
            pipeline.app.store.location(),
            t.cards,
            t.tcgplayer,
            t.cardkingdom
        );
    }
    Ok(())
}

// ---------------------------------------------------------------- drawing --

const COLOR_IDLE: u32 = 0x00_9a_9a_9a;
const COLOR_BUSY: u32 = 0x00_ff_c8_00;
const COLOR_ADDED: u32 = 0x00_20_d0_40;
const COLOR_PRICED: u32 = 0x00_30_c0_ff;
const COLOR_UNCHANGED: u32 = 0x00_1a_80_30;
const COLOR_REVIEW: u32 = 0x00_ff_80_00;
const COLOR_FAILED: u32 = 0x00_e0_30_30;
const COLOR_HINT: u32 = 0x00_40_a0_ff;
const COLOR_FOIL: u32 = 0x00_ff_d7_00;

fn outcome_color(o: &Outcome) -> u32 {
    match o {
        Outcome::Added { id, .. } if id.quality.needs_review() => COLOR_REVIEW,
        Outcome::Added { .. } => COLOR_ADDED,
        Outcome::Priced { .. } => COLOR_PRICED,
        Outcome::Unchanged { .. } => COLOR_UNCHANGED,
        Outcome::Failed(_) => COLOR_FAILED,
    }
}

fn draw_frame(frame: &RgbImage, buf: &mut [u32], step: u32) {
    let dw = frame.width() / step;
    for (i, dst) in buf.iter_mut().enumerate() {
        let (x, y) = ((i as u32 % dw) * step, (i as u32 / dw) * step);
        let [r, g, b] = frame.get_pixel(x, y).0;
        *dst = (r as u32) << 16 | (g as u32) << 8 | b as u32;
    }
}

fn dim_outside(buf: &mut [u32], stride: u32, r: Rect) {
    for (i, p) in buf.iter_mut().enumerate() {
        let (x, y) = (i as u32 % stride, i as u32 / stride);
        if x < r.x || x >= r.x + r.w || y < r.y || y >= r.y + r.h {
            *p = (*p >> 1) & 0x00_7f_7f_7f;
        }
    }
}

fn draw_rect(buf: &mut [u32], stride: u32, r: Rect, t: u32, color: u32) {
    let height = buf.len() as u32 / stride;
    let mut put = |x: u32, y: u32| {
        if x < stride && y < height {
            buf[(y * stride + x) as usize] = color;
        }
    };
    for y in r.y..r.y + r.h {
        for x in r.x..r.x + r.w {
            let edge = y < r.y + t || y + t >= r.y + r.h || x < r.x + t || x + t >= r.x + r.w;
            if edge {
                put(x, y);
            }
        }
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
