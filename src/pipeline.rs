//! Scan pipeline shared by the live window and one-shot commands:
//! image(s) in → identified, priced and (optionally) saved cards out.

use std::path::PathBuf;

use anyhow::{Result, bail};
use image::{DynamicImage, RgbImage};

use crate::app::{App, describe_card, describe_prices};
use crate::config::Config;
use crate::llm::{NormBox, VisionLlm};
use crate::model::{Card, CollectionEntry, Finish, Prices};
use crate::printing::{correlation, signature};
use crate::recognize::{Identification, Recognizer};

/// Extra margin around detected boxes; LLM boxes tend to be slightly tight.
const BOX_MARGIN: f32 = 0.04;
/// Same place + this similar ⇒ the same physical card as last pass.
const SAME_CARD_IOU: f32 = 0.4;
const SAME_CARD_SIMILARITY: f32 = 0.85;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub fn from_norm(b: NormBox, img_w: u32, img_h: u32, margin: f32) -> Self {
        let (bw, bh) = (b.x2 - b.x1, b.y2 - b.y1);
        let x1 = ((b.x1 - bw * margin).max(0.0) * img_w as f32).round() as u32;
        let y1 = ((b.y1 - bh * margin).max(0.0) * img_h as f32).round() as u32;
        let x2 = (((b.x2 + bw * margin).min(1.0)) * img_w as f32).round() as u32;
        let y2 = (((b.y2 + bh * margin).min(1.0)) * img_h as f32).round() as u32;
        Rect {
            x: x1,
            y: y1,
            w: x2.saturating_sub(x1).max(1),
            h: y2.saturating_sub(y1).max(1),
        }
    }

    pub fn iou(&self, o: &Rect) -> f32 {
        let ix = (self.x + self.w)
            .min(o.x + o.w)
            .saturating_sub(self.x.max(o.x));
        let iy = (self.y + self.h)
            .min(o.y + o.h)
            .saturating_sub(self.y.max(o.y));
        let inter = (ix * iy) as f32;
        let union = (self.w * self.h + o.w * o.h) as f32 - inter;
        if union <= 0.0 { 0.0 } else { inter / union }
    }

    pub fn crop(&self, img: &RgbImage) -> DynamicImage {
        DynamicImage::ImageRgb8(
            image::imageops::crop_imm(img, self.x, self.y, self.w, self.h).to_image(),
        )
    }
}

#[derive(Debug, Clone)]
pub enum Outcome {
    /// Identified and added to the collection.
    Added {
        id: Box<Identification>,
        entry: CollectionEntry,
    },
    /// Identified and priced only (dry run).
    Priced {
        id: Box<Identification>,
        finish: Finish,
        prices: Prices,
    },
    /// Same card as the previous pass; not added again.
    Unchanged {
        label: String,
    },
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct CardResult {
    pub rect: Rect,
    pub outcome: Outcome,
}

impl CardResult {
    pub fn summary(&self) -> String {
        match &self.outcome {
            Outcome::Added { id, entry } => {
                let mut s = format!(
                    "✓ {}\n      {} · now x{} · via {} ({})",
                    describe_card(&id.card, entry.finish),
                    describe_prices(&entry.prices),
                    entry.quantity,
                    id.method,
                    id.quality.label()
                );
                if let Some(note) = &id.note {
                    s.push_str(&format!(" — {note}"));
                }
                if id.quality.needs_review() {
                    s.push_str("\n      ⚠ printing uncertain — check it, or undo");
                }
                s
            }
            Outcome::Priced { id, finish, prices } => format!(
                "• {}\n      {} · via {} ({}){}",
                describe_card(&id.card, *finish),
                describe_prices(prices),
                id.method,
                id.quality.label(),
                id.note
                    .as_deref()
                    .map(|n| format!(" — {n}"))
                    .unwrap_or_default()
            ),
            Outcome::Unchanged { label } => format!("= {label} (unchanged, not re-added)"),
            Outcome::Failed(e) => format!("✗ {e}"),
        }
    }
}

/// A card seen in the previous table pass, for de-duplication.
struct Seen {
    rect: Rect,
    signature: Vec<f32>,
    label: String,
}

pub struct Pipeline {
    pub app: App,
    pub recognizer: Recognizer,
    save_dir: Option<PathBuf>,
    save_counter: u32,
    seen: Vec<Seen>,
    /// Rows added by the most recent scan, for undo.
    last_added: Vec<(String, Finish, String)>,
}

impl Pipeline {
    pub fn new(cfg: &Config) -> Result<Self> {
        let app = App::new(cfg)?;
        let llm = VisionLlm::from_config(&cfg.llm)?;
        let recognizer = Recognizer::new(cfg.recognition.mode, llm, &app.scryfall)?;
        Ok(Self {
            app,
            recognizer,
            save_dir: cfg.recognition.save_scans.clone(),
            save_counter: 0,
            seen: Vec::new(),
            last_added: Vec::new(),
        })
    }

    fn save_debug(&mut self, img: &DynamicImage) {
        if let Some(dir) = &self.save_dir {
            self.save_counter += 1;
            let name = format!(
                "scan-{}-{:03}.png",
                chrono::Local::now().format("%Y%m%d-%H%M%S"),
                self.save_counter
            );
            let _ = std::fs::create_dir_all(dir);
            let _ = img.save(dir.join(name));
        }
    }

    fn record(&mut self, id: Identification, finish: Finish, dry_run: bool) -> Result<Outcome> {
        if dry_run {
            let f = id.card.best_finish(finish);
            let prices = self.app.prices.price(&id.card, f);
            return Ok(Outcome::Priced {
                id: Box::new(id),
                finish: f,
                prices,
            });
        }
        let entry = self.app.add_card(&id.card, finish, 1)?;
        self.last_added
            .push((entry.scryfall_id.clone(), entry.finish, entry.name.clone()));
        Ok(Outcome::Added {
            id: Box::new(id),
            entry,
        })
    }

    /// Identify the single card in `img` (already cropped to the card).
    pub fn scan_single(
        &mut self,
        img: &DynamicImage,
        finish: Finish,
        force_llm: bool,
        dry_run: bool,
    ) -> Result<CardResult> {
        if force_llm && !self.recognizer.has_llm() {
            bail!("no LLM configured (see [llm] in mtg-scan.toml)");
        }
        self.save_debug(img);
        self.last_added.clear();
        let rect = Rect {
            x: 0,
            y: 0,
            w: img.width(),
            h: img.height(),
        };
        let outcome = match self.recognizer.identify(img, &self.app.scryfall, force_llm) {
            Ok(id) => self.record(id, finish, dry_run)?,
            Err(e) => Outcome::Failed(format!("{e:#}")),
        };
        Ok(CardResult { rect, outcome })
    }

    /// Find and identify every card on the table. Cards that haven't moved
    /// or changed since the previous pass are reported as `Unchanged`.
    /// `progress` is called after each card so a UI can update live.
    pub fn scan_table(
        &mut self,
        frame: &RgbImage,
        finish: Finish,
        dry_run: bool,
        mut progress: impl FnMut(&[CardResult]),
    ) -> Result<Vec<CardResult>> {
        let scene = DynamicImage::ImageRgb8(frame.clone());
        self.save_debug(&scene);
        let boxes = self.recognizer.detect_cards(&scene)?;
        self.last_added.clear();

        let mut results: Vec<CardResult> = Vec::new();
        let mut seen_now = Vec::new();
        for b in boxes {
            let rect = Rect::from_norm(b, frame.width(), frame.height(), BOX_MARGIN);
            // Skip duplicate detections of the same card.
            if results.iter().any(|r| r.rect.iou(&rect) > 0.5) {
                continue;
            }
            let crop = rect.crop(frame);
            let sig = signature(&crop);
            let previous = self.seen.iter().find(|s| {
                s.rect.iou(&rect) > SAME_CARD_IOU
                    && correlation(&s.signature, &sig) > SAME_CARD_SIMILARITY
            });
            let (outcome, label) = match previous {
                Some(prev) => (
                    Outcome::Unchanged {
                        label: prev.label.clone(),
                    },
                    Some(prev.label.clone()),
                ),
                None => match self.recognizer.identify(&crop, &self.app.scryfall, false) {
                    Ok(id) => {
                        let label = label_for(&id.card);
                        (self.record(id, finish, dry_run)?, Some(label))
                    }
                    Err(e) => (Outcome::Failed(format!("{e:#}")), None),
                },
            };
            if let Some(label) = label {
                seen_now.push(Seen {
                    rect,
                    signature: sig,
                    label,
                });
            }
            results.push(CardResult { rect, outcome });
            progress(&results);
        }
        self.seen = seen_now;
        Ok(results)
    }

    /// Forget the previous table pass so every card is added again.
    pub fn reset_memory(&mut self) {
        self.seen.clear();
    }

    /// Remove the cards added by the most recent scan.
    pub fn undo(&mut self) -> Result<Vec<String>> {
        let mut undone = Vec::new();
        for (id, finish, name) in std::mem::take(&mut self.last_added) {
            if self.app.store.remove(&id, finish, 1)?.is_some() {
                undone.push(format!("{name} ({finish})"));
            }
        }
        // Undone cards should be picked up again on the next pass.
        self.seen.clear();
        Ok(undone)
    }
}

fn label_for(card: &Card) -> String {
    format!(
        "{} ({} #{})",
        card.name,
        card.set_code.to_uppercase(),
        card.collector_number
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_from_norm_adds_margin_and_clamps() {
        let r = Rect::from_norm(
            NormBox {
                x1: 0.0,
                y1: 0.5,
                x2: 0.1,
                y2: 0.7,
            },
            1000,
            1000,
            0.1,
        );
        assert_eq!(
            r,
            Rect {
                x: 0,
                y: 480,
                w: 110,
                h: 240
            }
        );
    }

    #[test]
    fn iou_of_overlapping_rects() {
        let a = Rect {
            x: 0,
            y: 0,
            w: 10,
            h: 10,
        };
        let b = Rect {
            x: 5,
            y: 0,
            w: 10,
            h: 10,
        };
        assert!((a.iou(&b) - 50.0 / 150.0).abs() < 1e-6);
        assert_eq!(
            a.iou(&Rect {
                x: 20,
                y: 20,
                w: 5,
                h: 5
            }),
            0.0
        );
    }
}
