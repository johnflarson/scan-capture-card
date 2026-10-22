//! Desktop front end: a minimal camera window driven by keyboard shortcuts,
//! plus console output. All scanning happens in the [`Engine`]; see
//! `web` for the browser front end.

use std::path::Path;
use std::thread::{self, JoinHandle};

use anyhow::{Context, Result, anyhow};
use minifb::{Key, KeyRepeat, ScaleMode, Window, WindowOptions};
use mtg_scan_shared::Mode;
use tokio::sync::broadcast::error::RecvError;

use crate::camera::{self, Webcam};
use crate::config::Config;
use crate::engine::{self, Command, Engine, EngineEvent, EngineOptions, draw};
use crate::model::{Finish, format_usd};
use crate::pipeline::{CardResult, Outcome, Pipeline};
use crate::store::totals;

pub fn run(cfg: &Config, opts: EngineOptions) -> Result<()> {
    if opts.dry_run {
        println!("Dry run: cards are identified and priced but not saved.");
    }
    println!(
        "Keys: SPACE scan now · A auto on/off · F finish · M single/table mode · U undo · \
         L retry with LLM (single) · R rescan all (table) · Q quit"
    );
    println!(
        "Tip: turn off AI tracking / auto-framing in Insta360 Link Controller so the camera stays still.\n"
    );

    let engine = engine::start(cfg, opts)?;
    // Keeps the card data and daily prices current while the app runs.
    crate::bulk::spawn_daily(cfg.clone(), engine.saving());
    println!("Camera: {}", engine.view().camera);
    let logger = spawn_console_logger(&engine);
    let result = window_loop(&engine);
    let stopped = engine.shutdown();
    let _ = logger.join();
    result.and(stopped)
}

fn window_loop(engine: &Engine) -> Result<()> {
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

    let preview = engine.preview();
    let mut buffer = Vec::new();
    let (mut last_id, mut size) = (0u64, (0usize, 0usize));

    while window.is_open() && !window.is_key_down(Key::Escape) && !window.is_key_down(Key::Q) {
        if !engine.is_running() {
            return Err(anyhow!("the scanner stopped (see the messages above)"));
        }
        let state = engine.view();
        let pressed = |k| window.is_key_pressed(k, KeyRepeat::No);
        if pressed(Key::Space) || pressed(Key::Enter) {
            engine.send(Command::ScanNow);
        }
        if pressed(Key::L) {
            engine.send(Command::RetryLlm);
        }
        if pressed(Key::U) {
            engine.send(Command::Undo);
        }
        if pressed(Key::R) {
            engine.send(Command::Reset);
        }
        if pressed(Key::F) {
            let next = Finish::from(state.finish).next();
            println!("Finish for new cards: {next}");
            engine.send(Command::SetFinish(next));
        }
        if pressed(Key::A) {
            println!("Auto-scan {}", if state.auto { "off" } else { "on" });
            engine.send(Command::SetAuto(!state.auto));
        }
        if pressed(Key::M) {
            engine.send(Command::SetMode(match state.mode {
                Mode::Single => Mode::Table,
                Mode::Table => Mode::Single,
            }));
        }

        let mode = match state.mode {
            Mode::Single => "SINGLE",
            Mode::Table => "TABLE",
        };
        window.set_title(&format!(
            "mtg-scan [{mode}{}{}] [{}] — {}",
            if state.auto { " AUTO" } else { "" },
            if state.dry_run { " DRY-RUN" } else { "" },
            state.finish.label().to_uppercase(),
            state.status
        ));

        let latest = preview.borrow().clone();
        match latest {
            Some(p) if p.id != last_id => {
                last_id = p.id;
                draw::to_u32(&p.image, &mut buffer);
                size = (p.image.width() as usize, p.image.height() as usize);
                window.update_with_buffer(&buffer, size.0, size.1)?;
            }
            _ if !buffer.is_empty() => window.update_with_buffer(&buffer, size.0, size.1)?,
            _ => window.update(),
        }
    }
    Ok(())
}

/// Print engine progress to the console (used by both front ends).
pub fn spawn_console_logger(engine: &Engine) -> JoinHandle<()> {
    let mut events = engine.subscribe();
    thread::spawn(move || {
        loop {
            match events.blocking_recv() {
                Ok(EngineEvent::Ready(msg) | EngineEvent::Info(msg)) => println!("{msg}"),
                Ok(EngineEvent::Undone { message, .. }) => println!("{message}"),
                Ok(EngineEvent::Error(msg)) => println!("✗ {msg}"),
                Ok(EngineEvent::Results {
                    results,
                    done,
                    auto,
                    mode,
                }) => {
                    if done {
                        report(&results, mode, auto);
                    } else if let Some(last) = results.last() {
                        println!("  [{}] {}", results.len(), last.summary());
                    }
                }
                Ok(EngineEvent::State | EngineEvent::Card(_) | EngineEvent::Cleared) => {}
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => break,
            }
        }
    })
}

/// Print a finished scan's results.
fn report(results: &[CardResult], mode: Mode, auto: bool) {
    match (mode, results.first()) {
        (Mode::Single, Some(r)) if auto && matches!(r.outcome, Outcome::Failed(_)) => {
            // Quiet for auto-scans of an empty guide.
            println!("· nothing recognised in the guide");
        }
        (Mode::Single, Some(r)) => println!("{}", r.summary()),
        (Mode::Single, None) => println!("nothing scanned"),
        (Mode::Table, _) => println!("{}", engine::summarize(results, mode, auto)),
    }
}

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
            let (w, h) = (cfg.camera.table_width, cfg.camera.table_height);
            let mut cam = Webcam::open(cfg.camera.index, w, h)?;
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
    report(&results, Mode::Table, false);
    if !dry_run {
        let all = pipeline.app.store.entries()?;
        let t = totals(&all);
        println!(
            "Collection ({}): {} cards · TCGplayer {} · Card Kingdom {}",
            pipeline.app.store.location(),
            t.cards,
            format_usd(Some(t.tcgplayer)),
            format_usd(Some(t.cardkingdom))
        );
    }
    Ok(())
}
