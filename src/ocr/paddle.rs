//! PaddleOCR (PP-OCR text detection + recognition) through ONNX Runtime,
//! via `oar-ocr`. Works offline on any platform once its models are on disk:
//! they are downloaded on first use into `MTG_SCAN_MODELS_DIR` (the Docker
//! image ships them there), else `models/` in the cache folder, and checked
//! against their SHA-256.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use image::DynamicImage;
use oar_ocr::prelude::*;
use sha2::{Digest, Sha256};

use super::{OcrEngine, OcrLine};

/// Where the model files are published.
const BASE_URL: &str = "https://github.com/GreatV/oar-ocr/releases/download";

/// A model file: release tag, name, SHA-256.
struct ModelFile {
    tag: &'static str,
    name: &'static str,
    sha256: &'static str,
}

/// PP-OCRv5 mobile detection with the English recognition model: on card
/// photos it read names as well as the Latin model and the set line
/// ("DMU · EN") better, and PP-OCRv6 tiny did worse on camera crops. Its
/// character list still has Æ, û, é and the like.
const MODELS: [ModelFile; 3] = [
    ModelFile {
        tag: "v0.3.0",
        name: "pp-ocrv5_mobile_det.onnx",
        sha256: "1eb7b4f7ab657ebd1c66d5f79bca7497f29768a2e3c15e52daecbba1a8e4a039",
    },
    ModelFile {
        tag: "v0.3.0",
        name: "en_pp-ocrv5_mobile_rec.onnx",
        sha256: "8307465d3c9ef2ba4055c3bd0be55aafe11f518630212b7598b70ccb376028ac",
    },
    ModelFile {
        tag: "v0.3.0",
        name: "ppocrv5_en_dict.txt",
        sha256: "e025a66d31f327ba0c232e03f407ae8d105e1e709e7ccb3f408aa778c24e70d6",
    },
];

pub struct PaddleOcr {
    ocr: OAROCR,
}

impl PaddleOcr {
    pub fn new() -> Result<Self> {
        let dir = models_dir();
        let [det, rec, dict] = MODELS.each_ref().map(|f| ensure(&dir, f));
        let ocr = OAROCRBuilder::new(det?, rec?, dict?)
            .build()
            .context("could not load the PaddleOCR models")?;
        Ok(Self { ocr })
    }
}

impl OcrEngine for PaddleOcr {
    fn name(&self) -> &'static str {
        "PaddleOCR"
    }

    fn recognize(&self, img: &DynamicImage) -> Result<Vec<OcrLine>> {
        let (fw, fh) = (img.width() as f32, img.height() as f32);
        let results = self.ocr.predict(vec![img.to_rgb8()])?;
        let mut lines: Vec<OcrLine> = results
            .iter()
            .flat_map(|r| &r.text_regions)
            .filter_map(|region| {
                let text = region.text.as_deref()?.trim();
                let points = &region.bounding_box.points;
                if text.is_empty() || points.is_empty() {
                    return None;
                }
                let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
                for p in points {
                    x0 = x0.min(p.x);
                    y0 = y0.min(p.y);
                    x1 = x1.max(p.x);
                    y1 = y1.max(p.y);
                }
                Some(OcrLine {
                    text: text.to_string(),
                    x: (x0 / fw).clamp(0.0, 1.0),
                    y: (y0 / fh).clamp(0.0, 1.0),
                    w: ((x1 - x0) / fw).clamp(0.0, 1.0),
                    h: ((y1 - y0) / fh).clamp(0.0, 1.0),
                })
            })
            .collect();
        lines.sort_by(|a, b| a.y.total_cmp(&b.y));
        Ok(lines)
    }
}

/// Make sure every model file is on disk; returns their folder.
pub fn fetch_models() -> Result<PathBuf> {
    let dir = models_dir();
    for file in &MODELS {
        ensure(&dir, file)?;
    }
    Ok(dir)
}

fn models_dir() -> PathBuf {
    crate::config::env_path("MTG_SCAN_MODELS_DIR")
        .unwrap_or_else(|| crate::cache::dir().join("models"))
}

/// The model file's path, downloading it first if it isn't there (or is
/// damaged).
fn ensure(dir: &Path, file: &ModelFile) -> Result<PathBuf> {
    let path = dir.join(file.name);
    if path.is_file() && sha256_of(&path)? == file.sha256 {
        return Ok(path);
    }
    std::fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))?;
    let url = format!("{BASE_URL}/{}/{}", file.tag, file.name);
    eprintln!("Downloading the OCR model {}…", file.name);
    crate::tls::init();
    let bytes = reqwest::blocking::Client::builder()
        .user_agent(crate::app::USER_AGENT)
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(10 * 60))
        .build()?
        .get(&url)
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.bytes())
        .with_context(|| format!("could not download {url}"))?;
    let got = hex(&Sha256::digest(&bytes));
    if got != file.sha256 {
        bail!(
            "{url} isn't the expected file (SHA-256 {got}, expected {})",
            file.sha256
        );
    }
    let tmp = path.with_extension("part");
    std::fs::write(&tmp, &bytes).with_context(|| format!("could not write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("could not write {}", path.display()))?;
    Ok(path)
}

fn sha256_of(path: &Path) -> Result<String> {
    let bytes =
        std::fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
    Ok(hex(&Sha256::digest(&bytes)))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_are_lowercase_hex() {
        assert_eq!(hex(&[0x0a, 0xff]), "0aff");
        for f in &MODELS {
            assert_eq!(f.sha256.len(), 64, "{}", f.name);
            assert!(
                f.sha256
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
            );
        }
    }
}
