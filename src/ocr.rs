//! Text recognition using the OCR engine built into Windows 10/11
//! (`Windows.Media.Ocr`), so no Tesseract install is required. Other
//! platforms have no OCR engine yet: use `[recognition] mode = "llm"`.

use anyhow::Result;
#[cfg(not(windows))]
use anyhow::bail;
#[cfg(windows)]
use anyhow::{Context, anyhow};
use image::DynamicImage;
#[cfg(windows)]
use image::imageops::FilterType;
#[cfg(windows)]
use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
#[cfg(windows)]
use windows::Media::Ocr::OcrEngine;
#[cfg(windows)]
use windows::Storage::Streams::DataWriter;

/// One recognised line of text. Coordinates are normalised to 0.0..=1.0 of
/// the source image so callers don't need to care about any upscaling.
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

#[cfg(not(windows))]
pub struct Ocr;

#[cfg(not(windows))]
impl Ocr {
    pub fn new() -> Result<Self> {
        bail!(
            "there is no OCR engine on this platform yet: set [recognition] mode = \"llm\"              (with an [llm] provider) or pass --mode llm"
        )
    }

    pub fn recognize(&self, _img: &DynamicImage) -> Result<Vec<OcrLine>> {
        Ok(Vec::new())
    }
}

#[cfg(windows)]
pub struct Ocr {
    engine: OcrEngine,
}

#[cfg(windows)]
impl Ocr {
    pub fn new() -> Result<Self> {
        // Prefer English: card names, set codes and collector numbers are
        // printed in Latin script even on many non-English printings.
        let engine = windows::Globalization::Language::CreateLanguage(&"en-US".into())
            .and_then(|lang| OcrEngine::TryCreateFromLanguage(&lang))
            .or_else(|_| OcrEngine::TryCreateFromUserProfileLanguages())
            .context(
                "Windows OCR is unavailable; install an English language pack with OCR support \
                 (Settings > Time & language > Language)",
            )?;
        Ok(Self { engine })
    }

    /// Recognise text in `img`, upscaling small images first since Windows
    /// OCR struggles with text under ~20px tall (e.g. collector numbers).
    pub fn recognize(&self, img: &DynamicImage) -> Result<Vec<OcrLine>> {
        let max_dim = OcrEngine::MaxImageDimension()?;
        let longest = img.width().max(img.height());
        let target = std::env::var("MTG_OCR_TARGET")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2400u32)
            .min(max_dim);
        let scaled = if longest < target {
            let f = target as f32 / longest as f32;
            img.resize(
                (img.width() as f32 * f) as u32,
                (img.height() as f32 * f) as u32,
                FilterType::CatmullRom,
            )
        } else if longest > max_dim {
            img.resize(max_dim, max_dim, FilterType::Triangle)
        } else {
            img.clone()
        };

        let (w, h) = (scaled.width(), scaled.height());
        let mut bgra = scaled.to_rgba8().into_raw();
        bgra.as_chunks_mut::<4>()
            .0
            .iter_mut()
            .for_each(|px| px.swap(0, 2));

        let writer = DataWriter::new()?;
        writer.WriteBytes(&bgra)?;
        let buffer = writer.DetachBuffer()?;
        let bitmap = SoftwareBitmap::CreateCopyFromBuffer(
            &buffer,
            BitmapPixelFormat::Bgra8,
            w as i32,
            h as i32,
        )?;

        let result = self
            .engine
            .RecognizeAsync(&bitmap)?
            .join()
            .map_err(|e| anyhow!("OCR failed: {e}"))?;

        let (fw, fh) = (w as f32, h as f32);
        let mut lines = Vec::new();
        for line in result.Lines()? {
            let text = line.Text()?.to_string_lossy();
            let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
            for word in line.Words()? {
                let r = word.BoundingRect()?;
                x0 = x0.min(r.X);
                y0 = y0.min(r.Y);
                x1 = x1.max(r.X + r.Width);
                y1 = y1.max(r.Y + r.Height);
            }
            if x0 > x1 {
                continue;
            }
            lines.push(OcrLine {
                text,
                x: x0 / fw,
                y: y0 / fh,
                w: (x1 - x0) / fw,
                h: (y1 - y0) / fh,
            });
        }
        Ok(lines)
    }
}
