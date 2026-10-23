//! Text recognition behind one interface, [`OcrEngine`]: PaddleOCR
//! (PP-OCRv5 through ONNX Runtime, the `paddle-ocr` feature) everywhere, and
//! Windows' built-in OCR on Windows. `[recognition] ocr` picks one; `auto` is
//! PaddleOCR, falling back to Windows OCR on Windows.

#[cfg(feature = "paddle-ocr")]
mod paddle;
#[cfg(windows)]
mod windows;

use anyhow::Result;
use image::DynamicImage;
use serde::Deserialize;

/// One recognised line of text. Coordinates are normalised to 0.0..=1.0 of
/// the source image so callers don't need to care about any scaling.
#[derive(Debug, Clone)]
pub struct OcrLine {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl OcrLine {
    pub fn center_y(&self) -> f32 {
        self.y + self.h / 2.0
    }
}

pub trait OcrEngine {
    /// Shown in the recognizer description, e.g. "PaddleOCR".
    fn name(&self) -> &'static str;
    /// Every line of text in `img`, top to bottom.
    fn recognize(&self, img: &DynamicImage) -> Result<Vec<OcrLine>>;
}

/// `[recognition] ocr`: which OCR engine reads the cards.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum OcrChoice {
    /// PaddleOCR; on Windows, Windows OCR when PaddleOCR can't load.
    #[default]
    Auto,
    /// Windows' built-in OCR (Windows only).
    Windows,
    /// PaddleOCR; its models (~13 MB) are downloaded on first use.
    Paddle,
}

/// Open the chosen engine.
pub fn open(choice: OcrChoice) -> Result<Box<dyn OcrEngine>> {
    match choice {
        // PaddleOCR reads camera crops far better (4 of 9 cards in a 4K table
        // photo where Windows OCR read none), so it comes first; Windows OCR
        // is the fallback, e.g. when the models can't be downloaded.
        OcrChoice::Auto => match paddle_ocr() {
            Err(e) if cfg!(windows) => {
                eprintln!("  ! PaddleOCR is unavailable ({e:#}); using Windows OCR");
                windows_ocr()
            }
            opened => opened,
        },
        OcrChoice::Windows => windows_ocr(),
        OcrChoice::Paddle => paddle_ocr(),
    }
}

#[cfg(windows)]
fn windows_ocr() -> Result<Box<dyn OcrEngine>> {
    Ok(Box::new(windows::WindowsOcr::new()?))
}

#[cfg(not(windows))]
fn windows_ocr() -> Result<Box<dyn OcrEngine>> {
    anyhow::bail!("Windows OCR only exists on Windows: set [recognition] ocr = \"paddle\"")
}

/// Download PaddleOCR's models now (they are otherwise fetched on first
/// use). Returns their folder.
#[cfg(feature = "paddle-ocr")]
pub fn fetch_models() -> Result<std::path::PathBuf> {
    paddle::fetch_models()
}

#[cfg(not(feature = "paddle-ocr"))]
pub fn fetch_models() -> Result<std::path::PathBuf> {
    anyhow::bail!("this build has no PaddleOCR (it was built without the `paddle-ocr` feature)")
}

#[cfg(feature = "paddle-ocr")]
fn paddle_ocr() -> Result<Box<dyn OcrEngine>> {
    Ok(Box::new(paddle::PaddleOcr::new()?))
}

#[cfg(not(feature = "paddle-ocr"))]
fn paddle_ocr() -> Result<Box<dyn OcrEngine>> {
    anyhow::bail!(
        "this build has no PaddleOCR (it was built without the `paddle-ocr` feature): \
         set [recognition] mode = \"llm\" with an [llm] provider"
    )
}
