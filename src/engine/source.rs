//! Where the capture thread's frames come from: a camera attached to this
//! computer, the camera of a browser showing the web UI ([`BrowserFeed`]),
//! or a still photo (`--frames-from`, for trying the UI and table scans
//! without a camera).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use image::RgbImage;
use mtg_scan_shared::Mode;

use crate::camera::{self, Webcam};
use crate::config::Config;

/// Opened and used on the capture thread only.
pub trait FrameSource {
    /// The next frame, or `None` when none is ready yet (the capture thread
    /// then handles commands and asks again).
    fn frame(&mut self) -> Result<Option<RgbImage>>;
    /// Shown as "Camera: …", e.g. the camera name and mode.
    fn describe(&self) -> String;
    /// Frames come from elsewhere (a browser): the engine can't reopen the
    /// source for another mode, its frames may change size, and an error
    /// means it has ended.
    fn external(&self) -> bool {
        false
    }
}

/// Open the source for `mode` and return its first frame.
pub fn open(cfg: &Config, mode: Mode) -> Result<(Box<dyn FrameSource>, RgbImage)> {
    if let Some(path) = &cfg.camera.frames_from {
        let still = Still::open(path)?;
        let frame = still.image.clone();
        return Ok((Box::new(still), frame));
    }
    let (w, h) = resolution(cfg, mode);
    let mut cam = Webcam::open(cfg.camera.index, w, h)?;
    let frame = camera::warm_frame(&mut cam)?;
    Ok((Box::new(cam), frame))
}

/// The capture size configured for `mode`.
pub fn resolution(cfg: &Config, mode: Mode) -> (u32, u32) {
    match mode {
        Mode::Single => (cfg.camera.width, cfg.camera.height),
        Mode::Table => (cfg.camera.table_width, cfg.camera.table_height),
    }
}

impl FrameSource for Webcam {
    fn frame(&mut self) -> Result<Option<RgbImage>> {
        Webcam::frame(self).map(Some)
    }

    fn describe(&self) -> String {
        let f = self.format();
        format!(
            "{} ({}x{} {} @ {}fps)",
            self.name,
            f.width(),
            f.height(),
            f.format(),
            f.frame_rate()
        )
    }
}

/// Frames a browser sends from its camera (the web server pushes them; the
/// capture thread takes the newest). Closed when the browser disconnects or
/// when the engine stops using it (e.g. another browser took over).
#[derive(Clone)]
pub struct BrowserFeed(Arc<Feed>);

struct Feed {
    label: String,
    latest: Mutex<Option<RgbImage>>,
    arrived: Condvar,
    closed: AtomicBool,
}

impl BrowserFeed {
    /// `label`: the camera's name as the browser reports it.
    pub fn new(label: String) -> Self {
        Self(Arc::new(Feed {
            label,
            latest: Mutex::new(None),
            arrived: Condvar::new(),
            closed: AtomicBool::new(false),
        }))
    }

    /// Hand over a frame; one not taken yet is replaced.
    pub fn push(&self, frame: RgbImage) {
        *self.0.latest.lock().unwrap_or_else(|e| e.into_inner()) = Some(frame);
        self.0.arrived.notify_one();
    }

    pub fn close(&self) {
        self.0.closed.store(true, Ordering::SeqCst);
        self.0.arrived.notify_all();
    }

    pub fn is_closed(&self) -> bool {
        self.0.closed.load(Ordering::SeqCst)
    }

    /// The next frame, waiting up to `wait` for one.
    fn take(&self, wait: Duration) -> Option<RgbImage> {
        let latest = self.0.latest.lock().unwrap_or_else(|e| e.into_inner());
        let (mut latest, _) = self
            .0
            .arrived
            .wait_timeout_while(latest, wait, |l| l.is_none() && !self.is_closed())
            .unwrap_or_else(|e| e.into_inner());
        latest.take()
    }
}

impl PartialEq for BrowserFeed {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl std::fmt::Debug for BrowserFeed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "BrowserFeed({:?})", self.0.label)
    }
}

/// The capture thread's end of a [`BrowserFeed`]; dropping it closes the feed.
pub struct BrowserSource {
    feed: BrowserFeed,
    size: (u32, u32),
}

impl BrowserSource {
    pub fn new(feed: BrowserFeed) -> Self {
        Self { feed, size: (0, 0) }
    }
}

impl FrameSource for BrowserSource {
    fn frame(&mut self) -> Result<Option<RgbImage>> {
        if self.feed.is_closed() {
            bail!("the browser's camera disconnected");
        }
        let frame = self.feed.take(Duration::from_millis(100));
        if let Some(f) = &frame {
            self.size = f.dimensions();
        }
        Ok(frame)
    }

    fn describe(&self) -> String {
        match self.size {
            (0, _) => format!("{} (in the browser, starting)", self.feed.0.label),
            (w, h) => format!("{} (in the browser, {w}x{h})", self.feed.0.label),
        }
    }

    fn external(&self) -> bool {
        true
    }
}

impl Drop for BrowserSource {
    fn drop(&mut self) {
        self.feed.close();
    }
}

/// The same photo over and over, a few times a second.
struct Still {
    path: PathBuf,
    image: RgbImage,
}

impl Still {
    fn open(path: &Path) -> Result<Self> {
        let image = image::open(path)
            .with_context(|| format!("failed to open {}", path.display()))?
            .to_rgb8();
        Ok(Self {
            path: path.to_path_buf(),
            image,
        })
    }
}

impl FrameSource for Still {
    fn frame(&mut self) -> Result<Option<RgbImage>> {
        std::thread::sleep(Duration::from_millis(200));
        Ok(Some(self.image.clone()))
    }

    fn describe(&self) -> String {
        let name = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        format!(
            "photo {name} ({}x{})",
            self.image.width(),
            self.image.height()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_feed_hands_over_the_newest_frame_and_ends_when_closed() {
        let feed = BrowserFeed::new("Fake camera".into());
        let mut source = BrowserSource::new(feed.clone());
        assert_eq!(source.frame().unwrap(), None, "nothing sent yet");
        assert!(source.describe().contains("starting"));

        feed.push(RgbImage::new(4, 3));
        feed.push(RgbImage::new(8, 6));
        assert_eq!(source.frame().unwrap().unwrap().dimensions(), (8, 6));
        assert_eq!(source.describe(), "Fake camera (in the browser, 8x6)");
        assert!(source.external());

        feed.close();
        assert!(source.frame().is_err());
    }

    #[test]
    fn dropping_the_source_closes_the_feed() {
        let feed = BrowserFeed::new("Fake camera".into());
        drop(BrowserSource::new(feed.clone()));
        assert!(feed.is_closed());
    }
}
