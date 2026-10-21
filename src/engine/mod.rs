//! Scanning engine shared by the desktop window (`live`) and the web UI
//! (`web`). It runs two threads:
//!
//! * **capture**: owns the camera, runs the auto-scan [`Watcher`], turns
//!   commands into scan jobs and publishes preview frames with overlays.
//! * **worker**: owns the [`Pipeline`] (recognizer, prices, collection
//!   store), runs scans one at a time and, when idle, refreshes prices of
//!   cards not yet priced today.
//!
//! Front ends send [`Command`]s and read the shared state, the latest
//! [`Preview`] and the [`EngineEvent`] broadcast.

pub mod draw;
mod watcher;

use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use image::{DynamicImage, RgbImage};
use mtg_scan_shared::{CardOutcome, CardView, Mode, SessionView, StateView};
use tokio::sync::{broadcast, watch};

use crate::camera::{self, Webcam};
use crate::config::Config;
use crate::model::Finish;
use crate::pipeline::{CardResult, Outcome, Pipeline, Rect};
use crate::reprice::Repricer;
use draw::{Color, Overlay};
use watcher::Watcher;

const CARD_ASPECT: f32 = 63.0 / 88.0;
/// Cards kept in the session list (oldest dropped first).
const MAX_SESSION_CARDS: usize = 1000;

#[derive(Debug, Clone, Copy)]
pub struct EngineOptions {
    pub mode: Mode,
    pub finish: Finish,
    pub auto: bool,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    ScanNow,
    /// Single-card mode: re-identify with the vision LLM (replaces the last add).
    RetryLlm,
    Undo,
    /// Table mode: forget the last pass so every card is added again.
    Reset,
    SetFinish(Finish),
    SetAuto(bool),
    SetMode(Mode),
    Shutdown,
}

impl From<mtg_scan_shared::ClientCommand> for Command {
    fn from(c: mtg_scan_shared::ClientCommand) -> Self {
        use mtg_scan_shared::ClientCommand as C;
        match c {
            C::ScanNow => Command::ScanNow,
            C::RetryLlm => Command::RetryLlm,
            C::Undo => Command::Undo,
            C::Reset => Command::Reset,
            C::SetFinish { finish } => Command::SetFinish(finish.into()),
            C::SetAuto { auto } => Command::SetAuto(auto),
            C::SetMode { mode } => Command::SetMode(mode),
        }
    }
}

/// A display-sized camera frame with overlays. The JPEG encoding is done
/// once, on first request, and shared by every viewer.
pub struct Preview {
    pub id: u64,
    pub image: RgbImage,
    jpeg: OnceLock<Vec<u8>>,
}

impl Preview {
    pub fn jpeg(&self) -> &[u8] {
        self.jpeg.get_or_init(|| {
            let mut buf = Vec::new();
            let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 70);
            if enc.encode_image(&self.image).is_err() {
                buf.clear();
            }
            buf
        })
    }
}

#[derive(Debug, Clone)]
pub enum EngineEvent {
    /// Shared state changed; read [`Engine::view`].
    State,
    Ready(String),
    /// Partial (`done == false`, table mode) or final results of a scan.
    Results {
        results: Arc<Vec<CardResult>>,
        done: bool,
        auto: bool,
        mode: Mode,
    },
    /// A card was added (or priced, in a dry run). Boxed: events are cloned per subscriber.
    Card(Box<CardView>),
    /// Undo removed these session cards.
    Undone {
        seqs: Vec<u64>,
        message: String,
    },
    Info(String),
    Error(String),
}

struct State {
    mode: Mode,
    finish: Finish,
    auto: bool,
    dry_run: bool,
    ready: bool,
    busy: bool,
    status: String,
    camera: String,
    recognizer: String,
    collection: String,
    boxes: Vec<(Rect, Color)>,
    guide_color: Color,
    cards: Vec<CardView>,
    next_seq: u64,
}

impl State {
    fn view(&self) -> StateView {
        StateView {
            ready: self.ready,
            busy: self.busy,
            auto: self.auto,
            mode: self.mode,
            finish: self.finish.into(),
            dry_run: self.dry_run,
            status: self.status.clone(),
            camera: self.camera.clone(),
            recognizer: self.recognizer.clone(),
            collection: self.collection.clone(),
        }
    }
}

/// Handles shared by both threads.
#[derive(Clone)]
struct Ctx {
    state: Arc<Mutex<State>>,
    events: broadcast::Sender<EngineEvent>,
}

impl Ctx {
    fn lock(&self) -> MutexGuard<'_, State> {
        // A panicking thread can't leave State half-updated in a harmful way.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Change state and tell front ends about it.
    fn update(&self, f: impl FnOnce(&mut State)) {
        f(&mut self.lock());
        self.emit(EngineEvent::State);
    }

    fn emit(&self, e: EngineEvent) {
        // No subscribers is fine.
        let _ = self.events.send(e);
    }
}

enum Job {
    Single {
        image: DynamicImage,
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

pub struct Engine {
    ctx: Ctx,
    commands: Sender<Command>,
    preview: watch::Receiver<Option<Arc<Preview>>>,
    capture: Option<JoinHandle<Result<()>>>,
    worker: Option<JoinHandle<Result<()>>>,
}

impl Engine {
    /// Open the camera and start both threads. Returns once the camera is
    /// streaming; the recognizer keeps loading in the background (watch for
    /// [`EngineEvent::Ready`]).
    pub fn start(cfg: &Config, opts: EngineOptions) -> Result<Self> {
        let (events, _) = broadcast::channel(512);
        let ctx = Ctx {
            state: Arc::new(Mutex::new(State {
                mode: opts.mode,
                finish: opts.finish,
                auto: opts.auto,
                dry_run: opts.dry_run,
                ready: false,
                busy: true,
                status: "loading…".into(),
                camera: String::new(),
                recognizer: String::new(),
                collection: String::new(),
                boxes: Vec::new(),
                guide_color: draw::IDLE,
                cards: Vec::new(),
                next_seq: 1,
            })),
            events,
        };
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (job_tx, job_rx) = mpsc::channel();
        let (preview_tx, preview_rx) = watch::channel(None);
        let (opened_tx, opened_rx) = mpsc::sync_channel(1);

        let capture = {
            let (cfg, ctx) = (cfg.clone(), ctx.clone());
            thread::Builder::new()
                .name("capture".into())
                .spawn(move || capture_loop(cfg, ctx, cmd_rx, job_tx, preview_tx, opened_tx))?
        };
        // Camera problems should fail `start`, not surface later.
        let camera = opened_rx
            .recv()
            .map_err(|_| anyhow!("camera thread stopped"))??;
        ctx.update(|s| s.camera = camera);

        let worker = {
            let (cfg, ctx) = (cfg.clone(), ctx.clone());
            let dry_run = opts.dry_run;
            thread::Builder::new()
                .name("worker".into())
                .spawn(move || worker_loop(cfg, ctx, dry_run, job_rx))?
        };
        Ok(Self {
            ctx,
            commands: cmd_tx,
            preview: preview_rx,
            capture: Some(capture),
            worker: Some(worker),
        })
    }

    pub fn send(&self, cmd: Command) {
        let _ = self.commands.send(cmd);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<EngineEvent> {
        self.ctx.events.subscribe()
    }

    pub fn preview(&self) -> watch::Receiver<Option<Arc<Preview>>> {
        self.preview.clone()
    }

    pub fn view(&self) -> StateView {
        self.ctx.lock().view()
    }

    pub fn session(&self) -> SessionView {
        let s = self.ctx.lock();
        SessionView {
            state: s.view(),
            cards: s.cards.clone(),
        }
    }

    /// False once either thread has stopped (e.g. the camera was unplugged).
    pub fn is_running(&self) -> bool {
        [&self.capture, &self.worker]
            .iter()
            .all(|h| h.as_ref().is_some_and(|h| !h.is_finished()))
    }

    /// Stop both threads, returning the first error either hit.
    pub fn shutdown(mut self) -> Result<()> {
        let _ = self.commands.send(Command::Shutdown);
        let mut result = Ok(());
        // The capture thread owns the job sender; when it exits the worker's
        // queue closes and the worker finishes its current job, then stops.
        for handle in [self.capture.take(), self.worker.take()]
            .into_iter()
            .flatten()
        {
            let outcome = handle
                .join()
                .unwrap_or_else(|_| Err(anyhow!("engine thread panicked")));
            if result.is_ok() {
                result = outcome;
            }
        }
        result
    }
}

// ----------------------------------------------------------------- capture --

fn resolution(cfg: &Config, mode: Mode) -> (u32, u32) {
    match mode {
        Mode::Single => (cfg.camera.width, cfg.camera.height),
        Mode::Table => (cfg.camera.table_width, cfg.camera.table_height),
    }
}

fn open_camera(cfg: &Config, mode: Mode) -> Result<(Webcam, RgbImage)> {
    let (w, h) = resolution(cfg, mode);
    let mut cam = Webcam::open(cfg.camera.index, w, h)?;
    let frame = camera::warm_frame(&mut cam)?;
    Ok((cam, frame))
}

fn describe_camera(cam: &Webcam) -> String {
    let f = cam.format();
    format!(
        "{} ({}x{} {} @ {}fps)",
        cam.name,
        f.width(),
        f.height(),
        f.format(),
        f.frame_rate()
    )
}

/// The scan region for a mode: the card guide, or the whole frame.
fn region(cfg: &Config, mode: Mode, (fw, fh): (u32, u32)) -> Rect {
    match mode {
        Mode::Single => {
            let h = ((fh as f32 * cfg.camera.guide_height.clamp(0.2, 1.0)) as u32).min(fh);
            let w = ((h as f32 * CARD_ASPECT) as u32).min(fw);
            Rect {
                x: (fw - w) / 2,
                y: (fh - h) / 2,
                w,
                h,
            }
        }
        Mode::Table => Rect {
            x: 0,
            y: 0,
            w: fw,
            h: fh,
        },
    }
}

fn capture_loop(
    cfg: Config,
    ctx: Ctx,
    commands: Receiver<Command>,
    jobs: Sender<Job>,
    preview: watch::Sender<Option<Arc<Preview>>>,
    opened: mpsc::SyncSender<Result<String>>,
) -> Result<()> {
    let mut mode = ctx.lock().mode;
    let (mut cam, mut frame) = match open_camera(&cfg, mode) {
        Ok(c) => c,
        Err(e) => {
            let _ = opened.send(Err(e));
            return Ok(());
        }
    };
    let _ = opened.send(Ok(describe_camera(&cam)));
    let mut area = region(&cfg, mode, frame.dimensions());
    let mut watcher = Watcher::new(area);
    let mut frame_id = 0u64;

    loop {
        // (force_llm, auto)
        let mut trigger: Option<(bool, bool)> = None;
        let (ready, busy) = {
            let s = ctx.lock();
            (s.ready, s.busy)
        };
        let can_scan = ready && !busy;

        loop {
            match commands.try_recv() {
                Ok(Command::Shutdown) | Err(TryRecvError::Disconnected) => return Ok(()),
                Err(TryRecvError::Empty) => break,
                Ok(Command::ScanNow) if can_scan => trigger = Some((false, false)),
                Ok(Command::RetryLlm) if can_scan && mode == Mode::Single => {
                    trigger = Some((true, false))
                }
                Ok(Command::Undo) if can_scan => {
                    ctx.update(|s| {
                        s.busy = true;
                        s.status = "undoing…".into();
                    });
                    let _ = jobs.send(Job::Undo);
                }
                Ok(Command::Reset) if can_scan && mode == Mode::Table => {
                    watcher.forget_scanned();
                    ctx.update(|s| {
                        s.busy = true;
                        s.boxes.clear();
                    });
                    let _ = jobs.send(Job::Reset);
                }
                Ok(Command::SetFinish(f)) => ctx.update(|s| s.finish = f),
                Ok(Command::SetAuto(a)) => ctx.update(|s| {
                    s.auto = a;
                    if s.ready && !s.busy {
                        s.status = if a { "watching for cards" } else { "ready" }.into();
                    }
                }),
                Ok(Command::SetMode(m)) if m != mode && !busy => {
                    ctx.update(|s| {
                        s.busy = true;
                        s.status = "switching camera mode…".into();
                    });
                    drop(cam);
                    let (next, msg) = match open_camera(&cfg, m) {
                        Ok(opened) => (Some((opened, m)), None),
                        // Fall back to the previous mode rather than going dark.
                        Err(e) => match open_camera(&cfg, mode) {
                            Ok(opened) => (Some((opened, mode)), Some(format!("{e:#}"))),
                            Err(e2) => {
                                ctx.emit(EngineEvent::Error(format!("camera lost: {e2:#}")));
                                return Err(e2);
                            }
                        },
                    };
                    let ((c, f), new_mode) = next.expect("checked above");
                    (cam, frame, mode) = (c, f, new_mode);
                    area = region(&cfg, mode, frame.dimensions());
                    watcher = Watcher::new(area);
                    let camera = describe_camera(&cam);
                    ctx.update(|s| {
                        s.mode = mode;
                        s.camera = camera;
                        s.boxes.clear();
                        s.guide_color = draw::IDLE;
                        s.busy = !s.ready;
                        s.status = if !s.ready {
                            "loading…".into()
                        } else if s.auto {
                            "watching for cards".into()
                        } else {
                            "ready".into()
                        };
                    });
                    match msg {
                        Some(e) => ctx.emit(EngineEvent::Error(format!(
                            "could not switch camera mode: {e}"
                        ))),
                        None => ctx.emit(EngineEvent::Info(format!(
                            "Switched to {} mode",
                            match mode {
                                Mode::Single => "single-card",
                                Mode::Table => "table",
                            }
                        ))),
                    }
                }
                // Not possible right now (busy, wrong mode): ignore.
                Ok(_) => {}
            }
        }

        match cam.frame() {
            Ok(f) if f.dimensions() == frame.dimensions() => frame = f,
            Ok(_) => {}
            Err(e) => {
                ctx.emit(EngineEvent::Error(format!("camera: {e:#}")));
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
        }

        let auto = ctx.lock().auto;
        let thumb = watcher.thumb(&frame, area);
        if watcher.observe(thumb, auto && can_scan && trigger.is_none(), Instant::now()) {
            trigger = Some((false, true));
        } else if trigger.is_some() {
            watcher.mark_scanned();
        }

        if let Some((force_llm, is_auto)) = trigger {
            let finish = ctx.lock().finish;
            ctx.update(|s| {
                s.busy = true;
                s.status = "scanning…".into();
                s.guide_color = draw::BUSY;
                if mode == Mode::Table {
                    s.boxes.clear();
                }
            });
            let job = match mode {
                Mode::Single => Job::Single {
                    image: area.crop(&frame),
                    finish,
                    force_llm,
                    auto: is_auto,
                },
                Mode::Table => {
                    ctx.emit(EngineEvent::Info("Scanning table…".into()));
                    Job::Table {
                        frame: frame.clone(),
                        finish,
                    }
                }
            };
            if jobs.send(job).is_err() {
                return Err(anyhow!("scanner worker stopped"));
            }
        }

        let overlay = {
            let s = ctx.lock();
            Overlay {
                guide: (mode == Mode::Single)
                    .then_some((area, if s.busy { draw::BUSY } else { s.guide_color })),
                boxes: s.boxes.clone(),
                foil: s.finish != Finish::Nonfoil,
                auto_dot: s
                    .auto
                    .then_some(if s.busy { draw::BUSY } else { draw::IDLE }),
            }
        };
        frame_id += 1;
        let image = draw::compose(&frame, draw::preview_step(frame.width()), &overlay);
        preview.send_replace(Some(Arc::new(Preview {
            id: frame_id,
            image,
            jpeg: OnceLock::new(),
        })));
    }
}

// ------------------------------------------------------------------ worker --

fn worker_loop(cfg: Config, ctx: Ctx, dry_run: bool, jobs: Receiver<Job>) -> Result<()> {
    let mut pipeline = match Pipeline::new(&cfg) {
        Ok(p) => p,
        Err(e) => {
            let msg = format!("{e:#}");
            ctx.update(|s| s.status = format!("failed to start: {msg}"));
            ctx.emit(EngineEvent::Error(msg));
            return Err(e);
        }
    };
    // Warm the Card Kingdom cache now rather than during the first scan.
    let _ = pipeline.app.prices.cardkingdom.price("", Finish::Nonfoil);
    let recognizer = pipeline.recognizer.describe();
    let collection = pipeline.app.store.location();
    ctx.update(|s| {
        s.ready = true;
        s.busy = false;
        s.recognizer = recognizer.clone();
        s.collection = collection.clone();
        s.status = if s.auto {
            "watching for cards"
        } else {
            "ready"
        }
        .into();
    });
    ctx.emit(EngineEvent::Ready(format!(
        "Recognizer ready ({recognizer}). Saving to {collection}.
         Lay out cards and hold still — scanning is automatic."
    )));

    // Background reprice of rows not yet priced today, one row whenever no
    // scan is waiting, so price history builds up while the app runs.
    let today = chrono::Local::now().date_naive();
    let mut repricer = match (dry_run, pipeline.app.store.entries()) {
        (false, Ok(entries)) => Repricer::plan(&entries, today, false),
        (true, _) => Repricer::plan(&[], today, false),
        (false, Err(e)) => {
            ctx.emit(EngineEvent::Error(format!(
                "background reprice skipped: {e:#}"
            )));
            Repricer::plan(&[], today, false)
        }
    };
    if !repricer.is_done() {
        ctx.emit(EngineEvent::Info(format!(
            "Refreshing prices in the background: {} card(s) not priced today.",
            repricer.total()
        )));
    }

    // Session cards added by the most recent scan, for Undo.
    let mut last_scan: Vec<u64> = Vec::new();
    loop {
        let job = if repricer.is_done() {
            match jobs.recv() {
                Ok(job) => job,
                Err(_) => break,
            }
        } else {
            match jobs.try_recv() {
                Ok(job) => job,
                Err(TryRecvError::Disconnected) => break,
                Err(TryRecvError::Empty) => {
                    match repricer.step(&mut pipeline.app) {
                        Ok(_) if repricer.is_done() => ctx.emit(EngineEvent::Info(format!(
                            "Background price refresh finished ({} card(s)).",
                            repricer.total()
                        ))),
                        Ok(_) => {}
                        Err(e) => {
                            ctx.emit(EngineEvent::Error(format!(
                                "background price refresh stopped ({} left): {e:#}",
                                repricer.remaining()
                            )));
                            repricer.abandon();
                        }
                    }
                    continue;
                }
            }
        };

        match job {
            Job::Single {
                image,
                finish,
                force_llm,
                auto,
            } => match pipeline.scan_single(&image, finish, force_llm, dry_run) {
                Ok(r) => {
                    if force_llm {
                        // The retry replaced the previous add.
                        mark_undone(&ctx, &last_scan);
                    }
                    last_scan = record_card(&ctx, &r).into_iter().collect();
                    finish_scan(&ctx, vec![r], auto, Mode::Single);
                }
                Err(e) => fail(&ctx, e),
            },
            Job::Table { frame, finish } => {
                last_scan.clear();
                let result = pipeline.scan_table(&frame, finish, dry_run, |partial| {
                    if let Some(newest) = partial.last()
                        && let Some(seq) = record_card(&ctx, newest)
                    {
                        last_scan.push(seq);
                    }
                    let boxes = boxes_for(partial);
                    ctx.update(|s| s.boxes = boxes);
                    ctx.emit(EngineEvent::Results {
                        results: Arc::new(partial.to_vec()),
                        done: false,
                        auto: true,
                        mode: Mode::Table,
                    });
                });
                match result {
                    Ok(results) => finish_scan(&ctx, results, true, Mode::Table),
                    Err(e) => fail(&ctx, e),
                }
            }
            Job::Undo => match pipeline.undo() {
                Ok(undone) => {
                    let message = if undone.is_empty() {
                        "nothing to undo".to_string()
                    } else {
                        format!("↶ removed {}", undone.join(", "))
                    };
                    let seqs = std::mem::take(&mut last_scan);
                    mark_undone(&ctx, &seqs);
                    ctx.update(|s| {
                        s.busy = false;
                        s.status = message.clone();
                        s.boxes.clear();
                    });
                    ctx.emit(EngineEvent::Undone { seqs, message });
                }
                Err(e) => fail(&ctx, e.context("undo failed")),
            },
            Job::Reset => {
                pipeline.reset_memory();
                let message = "memory cleared — every card will be added on the next pass";
                ctx.update(|s| {
                    s.busy = false;
                    s.status = message.into();
                });
                ctx.emit(EngineEvent::Info(message.into()));
            }
        }
    }
    Ok(())
}

/// Add an Added/Priced result to the session list; returns its sequence number.
fn record_card(ctx: &Ctx, r: &CardResult) -> Option<u64> {
    let mut s = ctx.lock();
    let seq = s.next_seq;
    let view = card_view(r, seq)?;
    s.next_seq += 1;
    s.cards.push(view.clone());
    if s.cards.len() > MAX_SESSION_CARDS {
        s.cards.remove(0);
    }
    drop(s);
    ctx.emit(EngineEvent::Card(Box::new(view)));
    Some(seq)
}

fn mark_undone(ctx: &Ctx, seqs: &[u64]) {
    let mut s = ctx.lock();
    for c in s.cards.iter_mut().filter(|c| seqs.contains(&c.seq)) {
        c.undone = true;
    }
}

fn finish_scan(ctx: &Ctx, results: Vec<CardResult>, auto: bool, mode: Mode) {
    let status = summarize(&results, mode, auto);
    let boxes = if mode == Mode::Table {
        boxes_for(&results)
    } else {
        Vec::new()
    };
    let guide_color = results
        .first()
        .map(|r| outcome_color(&r.outcome))
        .unwrap_or(draw::IDLE);
    ctx.update(|s| {
        s.busy = false;
        s.status = status;
        s.boxes = boxes;
        s.guide_color = guide_color;
    });
    ctx.emit(EngineEvent::Results {
        results: Arc::new(results),
        done: true,
        auto,
        mode,
    });
}

fn fail(ctx: &Ctx, e: anyhow::Error) {
    let msg = format!("{e:#}");
    ctx.update(|s| {
        s.busy = false;
        s.status = "error — see the message".into();
        s.guide_color = draw::FAILED;
    });
    ctx.emit(EngineEvent::Error(msg));
}

fn boxes_for(results: &[CardResult]) -> Vec<(Rect, Color)> {
    results
        .iter()
        .map(|r| (r.rect, outcome_color(&r.outcome)))
        .collect()
}

pub fn outcome_color(o: &Outcome) -> Color {
    match o {
        Outcome::Added { id, .. } if id.quality.needs_review() => draw::REVIEW,
        Outcome::Added { .. } => draw::ADDED,
        Outcome::Priced { .. } => draw::PRICED,
        Outcome::Unchanged { .. } => draw::UNCHANGED,
        Outcome::Failed(_) => draw::FAILED,
    }
}

/// One-line status for a finished scan.
pub fn summarize(results: &[CardResult], mode: Mode, auto: bool) -> String {
    if mode == Mode::Single {
        return match results.first().map(|r| &r.outcome) {
            Some(Outcome::Failed(_)) if auto => "watching — nothing recognised".into(),
            Some(_) => results[0]
                .summary()
                .lines()
                .next()
                .unwrap_or_default()
                .to_string(),
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
    format!(
        "{} cards: {new} new (TCG ${tcg:.2} · CK ${ck:.2}), {same} unchanged, {failed} unrecognised",
        results.len()
    )
}

/// The UI's view of a scan result (only added or priced cards have one).
fn card_view(r: &CardResult, seq: u64) -> Option<CardView> {
    let (id, outcome, finish, quantity, prices) = match &r.outcome {
        Outcome::Added { id, entry } => (
            id,
            CardOutcome::Added,
            entry.finish,
            Some(entry.quantity),
            &entry.prices,
        ),
        Outcome::Priced { id, finish, prices } => (id, CardOutcome::Priced, *finish, None, prices),
        Outcome::Unchanged { .. } | Outcome::Failed(_) => return None,
    };
    let c = &id.card;
    Some(CardView {
        seq,
        time: chrono::Local::now().format("%H:%M:%S").to_string(),
        outcome,
        name: c.name.clone(),
        set_code: c.set_code.to_uppercase(),
        set_name: c.set_name.clone(),
        number: c.collector_number.clone(),
        rarity: c.rarity.clone(),
        finish: finish.into(),
        quantity,
        tcgplayer: prices.tcgplayer,
        cardkingdom: prices.cardkingdom,
        cardkingdom_buylist: prices.cardkingdom_buylist,
        tcgplayer_url: prices.tcgplayer_url.clone(),
        cardkingdom_url: prices.cardkingdom_url.clone(),
        scryfall_url: format!(
            "https://scryfall.com/card/{}/{}",
            c.set_code.to_lowercase(),
            c.collector_number
        ),
        // Scryfall's "normal" size (488x680) is sharper than "small" in the UI.
        image_url: c
            .image_small
            .as_ref()
            .map(|u| u.replacen("/small/", "/normal/", 1)),
        method: id.method.to_string(),
        quality: id.quality.label().to_string(),
        needs_review: id.quality.needs_review(),
        note: id.note.clone(),
        undone: false,
    })
}

/// Start the engine with a friendlier error for the most common problem.
pub fn start(cfg: &Config, opts: EngineOptions) -> Result<Engine> {
    Engine::start(cfg, opts)
        .context("could not start the scanner (is the camera connected and free?)")
}
