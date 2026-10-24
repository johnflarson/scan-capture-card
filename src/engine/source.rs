//! Where the capture thread's frames come from: a camera attached to this
//! computer, or a still photo (`--frames-from`, for trying the UI and table
//! scans without a camera).

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
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
