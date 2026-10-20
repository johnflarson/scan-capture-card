//! Visual printing matcher. When we know a card's name but not which
//! printing it is (the set code / collector number were unreadable), compare
//! the photo against Scryfall's image of every printing and keep the closest.
//! This reliably separates regular / showcase / borderless / extended-art
//! frames — which is usually where the big price differences are.

use anyhow::Result;
use image::{DynamicImage, imageops::FilterType};

use crate::model::Card;
use crate::scryfall::Scryfall;

/// Comparison resolution. Deliberately coarse so that loose crops, slight
/// rotation and camera blur don't dominate the score.
const SIG_W: u32 = 16;
const SIG_H: u32 = 22;
/// Cap image downloads for staples with hundreds of printings.
const MAX_CANDIDATES: usize = 40;

pub struct PrintingMatch {
    pub card: Card,
    pub score: f32,
    pub candidates: usize,
}

/// Returns `Ok(None)` when there is nothing to choose between.
pub fn best_printing(
    scryfall: &Scryfall,
    photo: &DynamicImage,
    card: &Card,
) -> Result<Option<PrintingMatch>> {
    let front_name = card.name.split(" // ").next().unwrap_or(&card.name);
    let printings = scryfall.printings(front_name)?;
    if printings.len() < 2 {
        return Ok(None);
    }
    let target = signature(photo);
    let mut best: Option<(Card, f32)> = None;
    let mut compared = 0;
    for candidate in printings.into_iter().take(MAX_CANDIDATES) {
        let Ok(Some(img)) = scryfall.small_image(&candidate) else {
            continue;
        };
        compared += 1;
        let score = correlation(&target, &signature(&img));
        if best.as_ref().is_none_or(|(_, s)| score > *s) {
            best = Some((candidate, score));
        }
    }
    Ok(best.map(|(card, score)| PrintingMatch {
        card,
        score,
        candidates: compared,
    }))
}

/// Per-channel z-normalised thumbnail, so white balance and exposure
/// differences between the camera and Scryfall's scans cancel out.
pub fn signature(img: &DynamicImage) -> Vec<f32> {
    let small = img
        .resize_exact(SIG_W, SIG_H, FilterType::Triangle)
        .to_rgb8();
    let n = (SIG_W * SIG_H) as usize;
    let mut out = vec![0f32; n * 3];
    for ch in 0..3 {
        let vals: Vec<f32> = small.pixels().map(|p| p.0[ch] as f32).collect();
        let mean = vals.iter().sum::<f32>() / n as f32;
        let var = vals.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / n as f32;
        let std = var.sqrt().max(1.0);
        for (i, v) in vals.iter().enumerate() {
            out[ch * n + i] = (v - mean) / std;
        }
    }
    out
}

/// Pearson correlation of two z-normalised signatures (-1.0..=1.0).
pub fn correlation(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>() / a.len().max(1) as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};

    #[test]
    fn identical_images_correlate_and_lighting_is_ignored() {
        let img = RgbImage::from_fn(64, 88, |x, y| {
            Rgb([(x * 4) as u8, (y * 2) as u8, ((x + y) % 255) as u8])
        });
        let darker = RgbImage::from_fn(64, 88, |x, y| {
            let p = img.get_pixel(x, y).0;
            Rgb([p[0] / 2 + 10, p[1] / 2 + 10, p[2] / 2 + 10])
        });
        let different = RgbImage::from_fn(64, 88, |x, y| {
            Rgb([255 - (x * 4) as u8, (x * 3) as u8, (y * 2) as u8])
        });
        let a = signature(&DynamicImage::ImageRgb8(img));
        let b = signature(&DynamicImage::ImageRgb8(darker));
        let c = signature(&DynamicImage::ImageRgb8(different));
        assert!(correlation(&a, &b) > 0.95);
        assert!(correlation(&a, &c) < correlation(&a, &b));
    }
}
