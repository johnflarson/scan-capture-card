//! Scanning engine shared by the desktop window (`live`) and the web UI
//! (`web`).
//!
//! * One [`Scanner`]: a **worker** thread that owns the [`Pipeline`]
//!   (recognizer, prices, collection store) and runs scans one at a time,
//!   for every session, in the order they arrive.
//! * One [`Engine`] per scan session (the desktop window has one; the web
//!   UI one per signed-in account): a **capture** thread that owns the
//!   session's camera, runs the auto-scan [`Watcher`], turns commands into
//!   scan jobs for the worker and publishes preview frames.
//!
//! Each job carries its session's [`ScanSetup`] (whose collection, card
//! language, the account's own LLM key), and the worker keeps each
//! session's memory (the last table pass, what Undo removes) apart.
//!
//! Front ends send [`Command`]s and read a session's state, its latest
//! [`Preview`] and its [`EngineEvent`] broadcast.

pub mod draw;
mod source;
mod watcher;

pub use source::BrowserFeed;
use source::FrameSource;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use image::{DynamicImage, RgbImage};
use mtg_scan_shared::{CardOutcome, CardView, Mode, OverlayView, SessionView, StateView};
use tokio::sync::{broadcast, watch};

use crate::config::{Config, LlmConfig, RecognitionConfig};
use crate::llm::VisionLlm;
use crate::model::Finish;
use crate::pipeline::{CardResult, Outcome, Pipeline, Rect};
use draw::{Color, Overlay};
use watcher::Watcher;

const CARD_ASPECT: f32 = 63.0 / 88.0;
/// Cards kept in the session list (oldest dropped first).
const MAX_SESSION_CARDS: usize = 1000;
/// How often to look for a camera while there is none.
const CAMERA_RETRY: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy)]
pub struct EngineOptions {
    pub mode: Mode,
    pub finish: Finish,
    pub auto: bool,
    pub dry_run: bool,
    /// Hold uncertain matches for review instead of saving them. Only for
    /// front ends that show a review list (the web UI).
    pub review_uncertain: bool,
    /// Start without a camera when none can be opened, and keep looking for
    /// one (the web UI: the collection, decks and settings don't need it).
    /// Otherwise a missing camera fails [`Engine::start`].
    pub wait_for_camera: bool,
    /// Draw the guide and boxes into the preview frames (the desktop window).
    /// The web UI draws them in the page from [`StateView::overlay`] instead.
    pub draw_overlay: bool,
    /// Open the camera attached to this computer. One session has it; the
    /// others scan with their browser's camera.
    pub server_camera: bool,
}

/// What a session's scans need from its account, read for every job.
#[derive(Debug, Clone, Default)]
pub struct ScanSetup {
    /// Whose collection scans go into (`None`: the first admin's).
    pub user: Option<i64>,
    /// Record printings in this language when Scryfall has them (empty:
    /// the settings file's).
    pub card_language: String,
    /// The account's own vision-LLM key (`None`: the server's).
    pub llm_key: Option<String>,
}

/// Reads a session's [`ScanSetup`] (e.g. from its account's settings).
pub type Setup = Arc<dyn Fn() -> ScanSetup + Send + Sync>;

/// A session without the server's camera, until its browser's camera
/// connects.
const NO_SESSION_CAMERA: &str = "No camera in this session yet: use this device's camera, or scan where the server's camera is.";

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
    /// Add scans to the collection, or only identify and price them.
    SetSave(bool),
    /// Empty the session list and forget the last scan.
    Clear,
    /// Save a held card as this printing and finish.
    Accept {
        seq: u64,
        scryfall_id: String,
        finish: Finish,
    },
    /// Drop a held card.
    Discard(u64),
    /// Take frames from a browser's camera (replacing any other camera).
    UseBrowser(BrowserFeed),
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
            C::SetSave { save } => Command::SetSave(save),
            C::Clear => Command::Clear,
            C::Accept {
                seq,
                scryfall_id,
                finish,
            } => Command::Accept {
                seq,
                scryfall_id,
                finish: finish.into(),
            },
            C::Discard { seq } => Command::Discard(seq),
        }
    }
}

/// A display-sized camera frame (with overlays when
/// [`EngineOptions::draw_overlay`]). The JPEG encoding is done once, on first
/// request, and shared by every viewer.
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
    /// A card was added, priced (dry run) or held for review, or a held card
    /// was reviewed (same `seq`: replaces it). Boxed: events are cloned per
    /// subscriber.
    Card(Box<CardView>),
    /// Undo removed these session cards.
    Undone {
        seqs: Vec<u64>,
        message: String,
    },
    /// The session list was emptied.
    Cleared,
    Info(String),
    Error(String),
}

struct State {
    mode: Mode,
    finish: Finish,
    auto: bool,
    dry_run: bool,
    save: bool,
    ready: bool,
    busy: bool,
    status: String,
    camera: String,
    /// Why no camera is open, while the engine waits for one.
    camera_error: Option<String>,
    recognizer: String,
    collection: String,
    /// Card Kingdom prices are recorded.
    card_kingdom: bool,
    boxes: Vec<(Rect, Color)>,
    guide_color: Color,
    /// The scan region and the frame size, while a camera is open.
    frame: Option<(Rect, (u32, u32))>,
    /// A browser's camera is the source.
    browser_camera: bool,
    /// Configured capture sizes: single-card mode, table mode.
    sizes: [(u32, u32); 2],
    cards: Vec<CardView>,
    next_seq: u64,
}

impl State {
    /// What to draw over the camera image (camera pixels).
    fn overlay(&self) -> Overlay {
        let busy_or = |c| if self.busy { draw::BUSY } else { c };
        Overlay {
            guide: self
                .frame
                .filter(|_| self.mode == Mode::Single)
                .map(|(area, _)| (area, busy_or(self.guide_color))),
            boxes: self.boxes.clone(),
            foil: self.finish != Finish::Nonfoil,
            auto_dot: self.auto.then_some(busy_or(draw::IDLE)),
        }
    }

    fn view(&self) -> StateView {
        StateView {
            ready: self.ready,
            busy: self.busy,
            auto: self.auto,
            mode: self.mode,
            finish: self.finish.into(),
            dry_run: self.dry_run,
            save: self.save,
            status: match &self.camera_error {
                Some(_) if !self.busy => "no camera".into(),
                _ => self.status.clone(),
            },
            camera: self.camera.clone(),
            camera_error: self.camera_error.clone(),
            recognizer: self.recognizer.clone(),
            collection: self.collection.clone(),
            card_kingdom: self.card_kingdom,
            overlay: match self.frame {
                Some((_, size)) => draw::overlay_view(size, &self.overlay()),
                None => OverlayView::default(),
            },
            browser_camera: self.browser_camera,
            capture_size: match self.mode {
                Mode::Single => self.sizes[0],
                Mode::Table => self.sizes[1],
            },
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
        if let EngineEvent::Error(msg) = &e {
            crate::telemetry::count_error(msg);
        }
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
        save: bool,
    },
    Table {
        frame: RgbImage,
        finish: Finish,
        save: bool,
    },
    Undo,
    Reset,
    Clear,
    /// Saving was switched on or off: forget the last table pass, and wake
    /// the worker so the background price refresh pauses or resumes.
    SaveChanged,
    Accept {
        seq: u64,
        scryfall_id: String,
        finish: Finish,
    },
    Discard(u64),
    /// The session ended: forget its memory.
    End,
}

/// What a job is for, and whose.
struct Work {
    session: u64,
    ctx: Ctx,
    setup: ScanSetup,
    job: Job,
}

/// The worker's progress, read by every session.
#[derive(Default)]
struct ScannerStatus {
    ready: AtomicBool,
    /// Why the worker couldn't start.
    failed: Mutex<Option<String>>,
    recognizer: Mutex<String>,
    collection: Mutex<String>,
    /// A new recognition setup for the worker to switch to.
    reconfigure: Mutex<Option<(RecognitionConfig, LlmConfig)>>,
}

/// The recognition worker every scan session shares.
pub struct Scanner {
    jobs: Mutex<Option<Sender<Work>>>,
    status: Arc<ScannerStatus>,
    worker: Mutex<Option<JoinHandle<Result<()>>>>,
    next_session: AtomicU64,
}

fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Scanner {
    /// Start the worker; the recognizer loads in the background (sessions
    /// show "loading…" until it is ready).
    pub fn start(cfg: &Config, opts: EngineOptions) -> Result<Arc<Self>> {
        let (tx, rx) = mpsc::channel();
        let status = Arc::new(ScannerStatus::default());
        let worker = {
            let (cfg, status) = (cfg.clone(), status.clone());
            thread::Builder::new()
                .name("worker".into())
                .spawn(move || worker_loop(cfg, opts, status, rx))?
        };
        Ok(Arc::new(Self {
            jobs: Mutex::new(Some(tx)),
            status,
            worker: Mutex::new(Some(worker)),
            next_session: AtomicU64::new(1),
        }))
    }

    fn submit(&self, work: Work) -> bool {
        locked(&self.jobs)
            .as_ref()
            .is_some_and(|jobs| jobs.send(work).is_ok())
    }

    /// Switch every session's recognition (mode and vision model) before
    /// the next scan; the sessions show the new description.
    pub fn reconfigure(&self, recognition: RecognitionConfig, llm: LlmConfig) {
        *locked(&self.status.reconfigure) = Some((recognition, llm));
    }

    /// False once the worker has stopped.
    pub fn is_running(&self) -> bool {
        locked(&self.worker)
            .as_ref()
            .is_some_and(|h| !h.is_finished())
    }

    /// Stop the worker after the jobs already queued (call once every
    /// session has shut down).
    pub fn shutdown(&self) -> Result<()> {
        locked(&self.jobs).take();
        match locked(&self.worker).take() {
            Some(handle) => handle
                .join()
                .unwrap_or_else(|_| Err(anyhow!("worker thread panicked"))),
            None => Ok(()),
        }
    }
}

/// One scan session: its camera, state and events. Scans run on the
/// shared [`Scanner`].
pub struct Engine {
    id: u64,
    ctx: Ctx,
    commands: Sender<Command>,
    preview: watch::Receiver<Option<Arc<Preview>>>,
    capture: Option<JoinHandle<Result<()>>>,
    scanner: Arc<Scanner>,
}

impl Engine {
    /// Open the session's camera (with [`EngineOptions::server_camera`]) and
    /// start its capture thread. Returns once the camera is streaming.
    pub fn start(
        scanner: &Arc<Scanner>,
        cfg: &Config,
        opts: EngineOptions,
        setup: Setup,
    ) -> Result<Self> {
        let (events, _) = broadcast::channel(512);
        let ctx = Ctx {
            state: Arc::new(Mutex::new(State {
                mode: opts.mode,
                finish: opts.finish,
                auto: opts.auto,
                dry_run: opts.dry_run,
                save: !opts.dry_run,
                ready: false,
                busy: true,
                status: "loading…".into(),
                camera: String::new(),
                camera_error: None,
                recognizer: String::new(),
                collection: String::new(),
                card_kingdom: cfg.prices.card_kingdom,
                boxes: Vec::new(),
                guide_color: draw::IDLE,
                frame: None,
                browser_camera: false,
                sizes: [
                    source::resolution(cfg, Mode::Single),
                    source::resolution(cfg, Mode::Table),
                ],
                cards: Vec::new(),
                next_seq: 1,
            })),
            events,
        };
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (preview_tx, preview_rx) = watch::channel(None);
        let (opened_tx, opened_rx) = mpsc::sync_channel(1);
        let id = scanner.next_session.fetch_add(1, Ordering::Relaxed);

        let capture = {
            let (cfg, ctx) = (cfg.clone(), ctx.clone());
            let link = Link {
                scanner: scanner.clone(),
                session: id,
                setup,
            };
            thread::Builder::new()
                .name("capture".into())
                .spawn(move || capture_loop(cfg, ctx, opts, cmd_rx, link, preview_tx, opened_tx))?
        };
        // Camera problems should fail `start`, not surface later.
        let camera = opened_rx
            .recv()
            .map_err(|_| anyhow!("camera thread stopped"))??;
        ctx.update(|s| s.camera = camera);
        Ok(Self {
            id,
            ctx,
            commands: cmd_tx,
            preview: preview_rx,
            capture: Some(capture),
            scanner: scanner.clone(),
        })
    }

    pub fn send(&self, cmd: Command) {
        let _ = self.commands.send(cmd);
    }

    /// Take frames from a browser's camera instead of this computer's
    /// until the returned feed is closed: by the caller when the browser
    /// disconnects, or by the engine when another browser takes over.
    pub fn attach_browser_camera(&self, label: String) -> BrowserFeed {
        let feed = BrowserFeed::new(label);
        self.send(Command::UseBrowser(feed.clone()));
        feed
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

    /// Whether this session's scans are being saved (off in a dry run and
    /// while the Scan page's Save switch is off), for background work that
    /// writes to the database and should pause with it.
    pub fn saving(&self) -> impl Fn() -> bool + Send + 'static {
        let state = self.ctx.state.clone();
        move || state.lock().is_ok_and(|s| s.save)
    }

    /// This session's scans are being saved right now.
    pub fn is_saving(&self) -> bool {
        self.ctx.lock().save
    }

    pub fn session(&self) -> SessionView {
        let s = self.ctx.lock();
        SessionView {
            state: s.view(),
            cards: s.cards.clone(),
        }
    }

    /// False once the capture thread (e.g. the camera was unplugged) or
    /// the shared worker has stopped.
    pub fn is_running(&self) -> bool {
        self.capture.as_ref().is_some_and(|h| !h.is_finished()) && self.scanner.is_running()
    }

    /// Stop the capture thread. Its jobs already queued still run, and the
    /// worker forgets the session's memory.
    pub fn shutdown(mut self) -> Result<()> {
        let _ = self.commands.send(Command::Shutdown);
        let result = match self.capture.take() {
            Some(handle) => handle
                .join()
                .unwrap_or_else(|_| Err(anyhow!("capture thread panicked"))),
            None => Ok(()),
        };
        self.scanner.submit(Work {
            session: self.id,
            ctx: self.ctx.clone(),
            setup: ScanSetup::default(),
            job: Job::End,
        });
        result
    }
}

/// A session's way to the worker.
struct Link {
    scanner: Arc<Scanner>,
    session: u64,
    setup: Setup,
}

impl Link {
    /// Queue a job with the session's current setup. False if the worker has
    /// stopped.
    fn submit(&self, ctx: &Ctx, job: Job) -> bool {
        self.scanner.submit(Work {
            session: self.session,
            ctx: ctx.clone(),
            setup: (self.setup)(),
            job,
        })
    }

    /// Show the worker's current recognizer (it changes when the setup
    /// wizard switches it).
    fn sync_recognizer(&self, ctx: &Ctx) {
        let current = locked(&self.scanner.status.recognizer).clone();
        if ctx.lock().recognizer != current {
            ctx.update(|s| s.recognizer = current);
        }
    }

    /// Once the worker is ready (or failed), say so in this session.
    fn sync_ready(&self, ctx: &Ctx, told_failure: &mut bool) {
        let status = &self.scanner.status;
        if status.ready.load(Ordering::Acquire) {
            let recognizer = locked(&status.recognizer).clone();
            let collection = locked(&status.collection).clone();
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
                "Recognizer ready ({recognizer}).\n\
                 Lay out cards and hold still — scanning is automatic."
            )));
        } else if !*told_failure && let Some(msg) = locked(&status.failed).clone() {
            *told_failure = true;
            ctx.update(|s| s.status = format!("failed to start: {msg}"));
            ctx.emit(EngineEvent::Error(msg));
        }
    }
}

// ----------------------------------------------------------------- capture --

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
    opts: EngineOptions,
    commands: Receiver<Command>,
    link: Link,
    preview: watch::Sender<Option<Arc<Preview>>>,
    opened: mpsc::SyncSender<Result<String>>,
) -> Result<()> {
    let mut mode = ctx.lock().mode;
    // `cam` is `None` while there is no camera (only with `wait_for_camera`,
    // or in a session without the server's camera).
    let opened_now = if opts.server_camera {
        source::open(&cfg, mode)
    } else {
        Err(anyhow!(NO_SESSION_CAMERA))
    };
    let (mut cam, mut frame) = match opened_now {
        Ok((cam, frame)) => {
            let _ = opened.send(Ok(cam.describe()));
            (Some(cam), frame)
        }
        Err(e) if !opts.server_camera => {
            ctx.update(|s| s.camera_error = Some(format!("{e:#}")));
            let _ = opened.send(Ok("no camera".into()));
            (None, RgbImage::new(1, 1))
        }
        Err(e) if opts.wait_for_camera => {
            let why = format!("{e:#}");
            eprintln!("! No camera ({why}). Everything but scanning works; looking for one.");
            ctx.update(|s| s.camera_error = Some(why));
            let _ = opened.send(Ok("no camera".into()));
            (None, RgbImage::new(1, 1))
        }
        Err(e) => {
            let _ = opened.send(Err(e));
            return Ok(());
        }
    };
    let mut retry_at = Instant::now() + CAMERA_RETRY;
    let mut area = region(&cfg, mode, frame.dimensions());
    let mut watcher = Watcher::new(area);
    let mut frame_id = 0u64;
    let mut told_failure = false;
    let submit = |job: Job| link.submit(&ctx, job);

    loop {
        // (force_llm, auto)
        let mut trigger: Option<(bool, bool)> = None;
        if !ctx.lock().ready {
            link.sync_ready(&ctx, &mut told_failure);
        } else {
            link.sync_recognizer(&ctx);
        }
        let (ready, busy) = {
            let s = ctx.lock();
            (s.ready, s.busy)
        };
        // A browser's camera has no frame until its first one arrives.
        let can_scan = ready && !busy && cam.is_some() && frame.width() > 1;

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
                    let _ = submit(Job::Undo);
                }
                Ok(Command::Reset) if can_scan && mode == Mode::Table => {
                    watcher.forget_scanned();
                    ctx.update(|s| {
                        s.busy = true;
                        s.boxes.clear();
                    });
                    let _ = submit(Job::Reset);
                }
                // Reviews queue behind any running scan.
                Ok(Command::Accept {
                    seq,
                    scryfall_id,
                    finish,
                }) => {
                    let _ = submit(Job::Accept {
                        seq,
                        scryfall_id,
                        finish,
                    });
                }
                Ok(Command::Discard(seq)) => {
                    let _ = submit(Job::Discard(seq));
                }
                Ok(Command::Clear) if can_scan => {
                    watcher.forget_scanned();
                    ctx.update(|s| {
                        s.busy = true;
                        s.boxes.clear();
                    });
                    let _ = submit(Job::Clear);
                }
                // `--dry-run` never saves.
                Ok(Command::SetSave(save)) if !ctx.lock().dry_run => {
                    ctx.update(|s| s.save = save);
                    ctx.emit(EngineEvent::Info(
                        if save {
                            "Saving scans to the collection"
                        } else {
                            "Not saving: scans are identified and priced only"
                        }
                        .into(),
                    ));
                    let _ = submit(Job::SaveChanged);
                }
                Ok(Command::SetFinish(f)) => ctx.update(|s| s.finish = f),
                Ok(Command::SetAuto(a)) => ctx.update(|s| {
                    s.auto = a;
                    if s.ready && !s.busy {
                        s.status = if a { "watching for cards" } else { "ready" }.into();
                    }
                }),
                // Without a camera the mode only picks the size opened once
                // one appears; a browser's camera follows `capture_size`.
                Ok(Command::SetMode(m))
                    if m != mode && cam.as_ref().is_none_or(|c| c.external()) =>
                {
                    mode = m;
                    ctx.update(|s| s.mode = m);
                }
                Ok(Command::UseBrowser(feed)) => {
                    // Closes this computer's camera (or the previous browser's
                    // feed, which ends that browser's connection).
                    drop(cam.take());
                    let browser = source::BrowserSource::new(feed);
                    let camera = browser.describe();
                    cam = Some(Box::new(browser));
                    frame = RgbImage::new(1, 1);
                    ctx.update(|s| {
                        s.camera = camera.clone();
                        s.camera_error = None;
                        s.browser_camera = true;
                        s.boxes.clear();
                    });
                    ctx.emit(EngineEvent::Info(format!("Using {camera}")));
                }
                Ok(Command::SetMode(m)) if m != mode && !busy => {
                    ctx.update(|s| {
                        s.busy = true;
                        s.status = "switching camera mode…".into();
                    });
                    // Close it first: a camera opens only once.
                    drop(cam.take());
                    let (next, msg) = match source::open(&cfg, m) {
                        Ok(opened) => (Some((opened, m)), None),
                        // Fall back to the previous mode rather than going dark.
                        Err(e) => match source::open(&cfg, mode) {
                            Ok(opened) => (Some((opened, mode)), Some(format!("{e:#}"))),
                            Err(e2) => {
                                ctx.emit(EngineEvent::Error(format!("camera lost: {e2:#}")));
                                return Err(e2);
                            }
                        },
                    };
                    let ((c, f), new_mode) = next.expect("checked above");
                    let camera = c.describe();
                    (cam, frame, mode) = (Some(c), f, new_mode);
                    area = region(&cfg, mode, frame.dimensions());
                    watcher = Watcher::new(area);
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

        if cam.is_none() && opts.server_camera && Instant::now() >= retry_at {
            retry_at = Instant::now() + CAMERA_RETRY;
            if let Ok((c, f)) = source::open(&cfg, mode) {
                let camera = c.describe();
                (cam, frame) = (Some(c), f);
                area = region(&cfg, mode, frame.dimensions());
                watcher = Watcher::new(area);
                ctx.update(|s| {
                    s.camera = camera.clone();
                    s.camera_error = None;
                });
                ctx.emit(EngineEvent::Info(format!("Camera connected: {camera}")));
            }
        }
        // Where the page draws the guide and boxes.
        let shown = cam
            .as_ref()
            .filter(|_| frame.width() > 1)
            .map(|_| (area, frame.dimensions()));
        if ctx.lock().frame != shown {
            ctx.update(|s| s.frame = shown);
        }
        let Some(source) = cam.as_mut() else {
            // Keep handling commands while there is no camera.
            std::thread::sleep(Duration::from_millis(100));
            continue;
        };
        let (got, external) = (source.frame(), source.external());
        match got {
            Ok(Some(f)) if f.dimensions() == frame.dimensions() => frame = f,
            // A browser's camera changes size with the mode or another camera.
            Ok(Some(f)) if external => {
                frame = f;
                area = region(&cfg, mode, frame.dimensions());
                watcher = Watcher::new(area);
                let camera = source.describe();
                ctx.update(|s| s.camera = camera);
            }
            Ok(_) => {}
            // The browser disconnected: look for this computer's camera again,
            // after a pause (a browser switching mode reconnects within it).
            Err(e) if external => {
                cam = None;
                retry_at = Instant::now() + CAMERA_RETRY;
                let why = if opts.server_camera {
                    format!("{e:#}")
                } else {
                    format!("{e:#}. {NO_SESSION_CAMERA}")
                };
                ctx.update(|s| {
                    s.camera = "no camera".into();
                    s.camera_error = Some(why);
                    s.browser_camera = false;
                    s.boxes.clear();
                });
                continue;
            }
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
            let (finish, save) = {
                let s = ctx.lock();
                (s.finish, s.save)
            };
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
                    save,
                },
                Mode::Table => {
                    ctx.emit(EngineEvent::Info("Scanning table…".into()));
                    Job::Table {
                        frame: frame.clone(),
                        finish,
                        save,
                    }
                }
            };
            if !submit(job) {
                return Err(anyhow!("scanner worker stopped"));
            }
        }

        let overlay = if opts.draw_overlay {
            ctx.lock().overlay()
        } else {
            Overlay::default()
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

/// A session's memory between its jobs.
#[derive(Default)]
struct SessionMemory {
    pipeline: crate::pipeline::Memory,
    /// Session cards added by the most recent scan, for Undo.
    last_scan: Vec<u64>,
}

fn worker_loop(
    cfg: Config,
    opts: EngineOptions,
    status: Arc<ScannerStatus>,
    jobs: Receiver<Work>,
) -> Result<()> {
    let dry_run = opts.dry_run;
    let mut pipeline = match Pipeline::new(&cfg) {
        Ok(p) => p,
        Err(e) => {
            *locked(&status.failed) = Some(format!("{e:#}"));
            return Err(e);
        }
    };
    pipeline.hold_uncertain = opts.review_uncertain && !dry_run;
    // Warm the Card Kingdom cache now rather than during the first scan.
    if let Some(ck) = &pipeline.app.prices.cardkingdom {
        let _ = ck.price("", Finish::Nonfoil);
    }
    let default_language = cfg.recognition.card_language.clone();
    *locked(&status.recognizer) = pipeline.recognizer.describe();
    *locked(&status.collection) = pipeline.app.store.location();
    status.ready.store(true, Ordering::Release);

    let mut sessions: HashMap<u64, SessionMemory> = HashMap::new();
    loop {
        // Wake up now and then to apply a new recognition setup while idle.
        let work = match jobs.recv_timeout(Duration::from_millis(500)) {
            Ok(work) => Some(work),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if let Some((recognition, llm)) = locked(&status.reconfigure).take() {
            let applied = VisionLlm::from_config(&llm).and_then(|vision| {
                pipeline
                    .recognizer
                    .reconfigure(recognition.mode, recognition.ocr, vision)
            });
            match applied {
                Ok(()) => *locked(&status.recognizer) = pipeline.recognizer.describe(),
                Err(e) => eprintln!("  ! could not switch the recognizer: {e:#}"),
            }
        }
        let Some(work) = work else { continue };
        if matches!(work.job, Job::End) {
            sessions.remove(&work.session);
            continue;
        }
        // Switch to the session's account, language, key and memory.
        if let Some(user) = work.setup.user {
            pipeline.app.store.set_user(user);
        }
        pipeline.set_card_language(if work.setup.card_language.is_empty() {
            &default_language
        } else {
            &work.setup.card_language
        });
        pipeline.recognizer.set_llm_key(work.setup.llm_key.clone());
        let mut memory = sessions.remove(&work.session).unwrap_or_default();
        pipeline.memory = std::mem::take(&mut memory.pipeline);
        run_job(&mut pipeline, &work.ctx, &mut memory.last_scan, work.job);
        memory.pipeline = std::mem::take(&mut pipeline.memory);
        sessions.insert(work.session, memory);
    }
    Ok(())
}

/// Run one job of a session (`ctx`) on the pipeline set up for it.
fn run_job(pipeline: &mut Pipeline, ctx: &Ctx, last_scan: &mut Vec<u64>, job: Job) {
    let ctx = ctx.clone();
    {
        match job {
            Job::Single {
                image,
                finish,
                force_llm,
                auto,
                save,
            } => match pipeline.scan_single(&image, finish, force_llm, !save) {
                Ok((r, replaced)) => {
                    if replaced {
                        // The retry replaced the previous single-card add.
                        mark_undone(&ctx, last_scan);
                    }
                    let seq = record_card(&ctx, &r);
                    // A failed retry keeps the previous scan undoable.
                    if !(force_llm && matches!(r.outcome, Outcome::Failed(_))) {
                        *last_scan = seq.into_iter().collect();
                    }
                    finish_scan(&ctx, vec![r], auto, Mode::Single);
                }
                Err(e) => fail(&ctx, e),
            },
            Job::Table {
                frame,
                finish,
                save,
            } => {
                last_scan.clear();
                let result = pipeline.scan_table(&frame, finish, !save, |partial| {
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
                    let seqs = std::mem::take(last_scan);
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
            Job::Accept {
                seq,
                scryfall_id,
                finish,
            } => {
                let Some(mut view) = held_card(&ctx, seq) else {
                    ctx.emit(EngineEvent::Info("That card was already reviewed.".into()));
                    return;
                };
                match accept(pipeline, &scryfall_id, finish) {
                    Ok((card, entry)) => {
                        let other = card.scryfall_id != view.scryfall_id;
                        fill_card(&mut view, &card);
                        view.outcome = CardOutcome::Added;
                        view.finish = entry.finish.into();
                        view.quantity = Some(entry.quantity);
                        view.tcgplayer = entry.prices.tcgplayer;
                        view.cardmarket = entry.prices.cardmarket;
                        view.cardkingdom = entry.prices.cardkingdom;
                        view.cardkingdom_buylist = entry.prices.cardkingdom_buylist;
                        view.tcgplayer_url = entry.prices.tcgplayer_url.clone();
                        view.cardkingdom_url = entry.prices.cardkingdom_url.clone();
                        view.needs_review = false;
                        view.note = Some(
                            if other {
                                "accepted as another printing"
                            } else {
                                "accepted after review"
                            }
                            .into(),
                        );
                        replace_card(&ctx, view);
                        ctx.emit(EngineEvent::Info(format!(
                            "Added {} ({} #{}, {}) · now x{}",
                            entry.name,
                            entry.set_code,
                            entry.collector_number,
                            entry.finish,
                            entry.quantity
                        )));
                    }
                    Err(e) => ctx.emit(EngineEvent::Error(format!("could not add it: {e:#}"))),
                }
            }
            Job::Discard(seq) => match held_card(&ctx, seq) {
                Some(mut view) => {
                    view.outcome = CardOutcome::Discarded;
                    replace_card(&ctx, view);
                }
                None => ctx.emit(EngineEvent::Info("That card was already reviewed.".into())),
            },
            Job::Clear => {
                pipeline.clear();
                last_scan.clear();
                ctx.update(|s| {
                    s.cards.clear();
                    s.busy = false;
                    s.boxes.clear();
                    s.guide_color = draw::IDLE;
                    s.status = "cleared — starting fresh".into();
                });
                ctx.emit(EngineEvent::Cleared);
            }
            // Cards on the table that were only priced get added on the
            // next pass once saving is back on.
            Job::SaveChanged => pipeline.reset_memory(),
            Job::Reset => {
                pipeline.reset_memory();
                let message = "memory cleared — every card will be added on the next pass";
                ctx.update(|s| {
                    s.busy = false;
                    s.status = message.into();
                });
                ctx.emit(EngineEvent::Info(message.into()));
            }
            Job::End => {}
        }
    }
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

/// The session card `seq` if it is still waiting for review.
fn held_card(ctx: &Ctx, seq: u64) -> Option<CardView> {
    ctx.lock()
        .cards
        .iter()
        .find(|c| c.seq == seq && c.outcome == CardOutcome::Held && !c.undone)
        .cloned()
}

/// Replace a session card (same seq) and tell the front ends.
fn replace_card(ctx: &Ctx, view: CardView) {
    let mut s = ctx.lock();
    if let Some(c) = s.cards.iter_mut().find(|c| c.seq == view.seq) {
        *c = view.clone();
    }
    drop(s);
    ctx.emit(EngineEvent::Card(Box::new(view)));
}

/// Save a reviewed card as printing `scryfall_id` (priced now).
fn accept(
    pipeline: &mut Pipeline,
    scryfall_id: &str,
    finish: Finish,
) -> Result<(crate::model::Card, crate::model::CollectionEntry)> {
    let card = pipeline
        .app
        .scryfall
        .by_id(scryfall_id)?
        .ok_or_else(|| anyhow!("Scryfall doesn't know the printing {scryfall_id}"))?;
    let language = pipeline.card_language().to_string();
    let entry = pipeline.app.add_card(&card, finish, 1, &language)?;
    Ok((card, entry))
}

fn mark_undone(ctx: &Ctx, seqs: &[u64]) {
    let mut s = ctx.lock();
    for c in s.cards.iter_mut().filter(|c| seqs.contains(&c.seq)) {
        c.undone = true;
    }
}

fn finish_scan(ctx: &Ctx, results: Vec<CardResult>, auto: bool, mode: Mode) {
    let failed = results
        .iter()
        .filter(|r| matches!(r.outcome, Outcome::Failed(_)))
        .count();
    crate::telemetry::count_scans((results.len() - failed) as u64, failed as u64);
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
        Outcome::Held { .. } => draw::REVIEW,
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
    let (mut new, mut same, mut failed, mut held) = (0, 0, 0, 0);
    let (mut tcg, mut cm) = (0.0, 0.0);
    for r in results {
        match &r.outcome {
            Outcome::Added { entry, .. } => {
                new += 1;
                tcg += entry.prices.tcgplayer.unwrap_or(0.0);
                cm += entry.prices.cardmarket.unwrap_or(0.0);
            }
            Outcome::Priced { prices, .. } => {
                new += 1;
                tcg += prices.tcgplayer.unwrap_or(0.0);
                cm += prices.cardmarket.unwrap_or(0.0);
            }
            Outcome::Held { .. } => held += 1,
            Outcome::Unchanged { .. } => same += 1,
            Outcome::Failed(_) => failed += 1,
        }
    }
    let review = if held > 0 {
        format!(", {held} to review")
    } else {
        String::new()
    };
    format!(
        "{} cards: {new} new (TCGplayer ${tcg:.2} · Cardmarket €{cm:.2}){review}, {same} unchanged, {failed} unrecognised",
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
        Outcome::Held { id, finish, prices } => (id, CardOutcome::Held, *finish, None, prices),
        Outcome::Unchanged { .. } | Outcome::Failed(_) => return None,
    };
    let mut view = CardView {
        seq,
        time: chrono::Local::now().format("%H:%M:%S").to_string(),
        outcome,
        scryfall_id: String::new(),
        name: String::new(),
        set_code: String::new(),
        set_name: String::new(),
        number: String::new(),
        rarity: String::new(),
        finish: finish.into(),
        quantity,
        tcgplayer: prices.tcgplayer,
        cardmarket: prices.cardmarket,
        cardkingdom: prices.cardkingdom,
        cardkingdom_buylist: prices.cardkingdom_buylist,
        tcgplayer_url: prices.tcgplayer_url.clone(),
        cardkingdom_url: prices.cardkingdom_url.clone(),
        scryfall_url: String::new(),
        image_url: None,
        method: id.method.to_string(),
        quality: id.quality.label().to_string(),
        needs_review: id.quality.needs_review(),
        note: id.note.clone(),
        undone: false,
    };
    fill_card(&mut view, &id.card);
    Some(view)
}

/// The printing's details in a session card.
fn fill_card(view: &mut CardView, c: &crate::model::Card) {
    view.scryfall_id = c.scryfall_id.clone();
    view.name = c.name.clone();
    view.set_code = c.set_code.to_uppercase();
    view.set_name = c.set_name.clone();
    view.number = c.collector_number.clone();
    view.rarity = c.rarity.clone();
    view.scryfall_url = format!(
        "https://scryfall.com/card/{}/{}",
        c.set_code.to_lowercase(),
        c.collector_number
    );
    // Scryfall's "normal" size (488x680) is sharper than "small" in the UI.
    view.image_url = c
        .image_small
        .as_ref()
        .map(|u| u.replacen("/small/", "/normal/", 1));
}

/// A worker and one session for the first admin, as the settings file sets
/// it up (the desktop window), with a friendlier error for the most common
/// problem.
pub fn start(cfg: &Config, opts: EngineOptions) -> Result<(Arc<Scanner>, Engine)> {
    let scanner = Scanner::start(cfg, opts)?;
    let setup = ScanSetup {
        user: None,
        card_language: cfg.recognition.card_language.clone(),
        llm_key: None,
    };
    let engine = Engine::start(&scanner, cfg, opts, Arc::new(move || setup.clone()))
        .context("could not start the scanner (is the camera connected and free?)")?;
    Ok((scanner, engine))
}
