//! Turns a photo of a card into a specific Scryfall printing.
//!
//! Two readers produce [`CardHints`] (name / set code / collector number):
//! Windows OCR and an optional vision LLM. Hints are then resolved against
//! Scryfall, preferring an exact set + collector-number match confirmed by
//! the printed name.

use std::collections::HashSet;
use std::fmt;
use std::sync::LazyLock;

use anyhow::{Result, bail};
use image::DynamicImage;
use regex::Regex;

use crate::config::RecognitionMode;
use crate::llm::VisionLlm;
use crate::model::Card;
use crate::ocr::{self, OcrChoice, OcrEngine, OcrLine};
use crate::printing;
use crate::scryfall::Scryfall;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CardHints {
    pub name: Option<String>,
    /// Lowercase Scryfall set code.
    pub set_code: Option<String>,
    pub collector_number: Option<String>,
    pub foil: Option<bool>,
}

impl fmt::Display for CardHints {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "name={:?} set={:?} number={:?}",
            self.name.as_deref().unwrap_or("?"),
            self.set_code.as_deref().unwrap_or("?"),
            self.collector_number.as_deref().unwrap_or("?")
        )
    }
}

/// How confident we are that the resolved printing is the one in the photo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MatchQuality {
    /// Name matched, but the printing is Scryfall's default guess.
    NameOnly,
    /// Name matched; printing chosen by comparing the photo to every
    /// printing's image (frame treatment right, reprints may be confused).
    Visual,
    /// Set + number matched but the name couldn't confirm it.
    SetNumber,
    /// Name matched within the detected set.
    NameAndSet,
    /// Name matched and the card has only one paper printing.
    OnlyPrinting,
    /// Set + collector number matched and the name agrees.
    Exact,
}

impl MatchQuality {
    /// Good enough that we don't need to ask a slower reader.
    pub fn is_confident(self) -> bool {
        self >= MatchQuality::NameAndSet
    }

    /// Worth flagging to the user for a manual check.
    pub fn needs_review(self) -> bool {
        matches!(self, MatchQuality::NameOnly | MatchQuality::SetNumber)
    }

    pub fn label(self) -> &'static str {
        match self {
            MatchQuality::Exact => "exact printing",
            MatchQuality::OnlyPrinting => "name, single printing",
            MatchQuality::NameAndSet => "name + set",
            MatchQuality::SetNumber => "set + number only",
            MatchQuality::Visual => "name + visual printing match",
            MatchQuality::NameOnly => "name only, printing guessed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Ocr,
    Llm,
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Method::Ocr => "OCR",
            Method::Llm => "LLM",
        })
    }
}

#[derive(Debug, Clone)]
pub struct Identification {
    pub card: Card,
    pub quality: MatchQuality,
    pub method: Method,
    /// Extra detail for the user, e.g. how a printing was chosen.
    pub note: Option<String>,
}

pub struct Recognizer {
    ocr: Option<Box<dyn OcrEngine>>,
    llm: Option<VisionLlm>,
    mode: RecognitionMode,
    set_codes: HashSet<String>,
    names: NameIndex,
}

impl Recognizer {
    pub fn new(
        mode: RecognitionMode,
        ocr_choice: OcrChoice,
        llm: Option<VisionLlm>,
        scryfall: &Scryfall,
    ) -> Result<Self> {
        if mode == RecognitionMode::Llm && llm.is_none() {
            bail!(
                "recognition mode 'llm' needs an LLM provider (set [llm] provider or --llm-provider)"
            );
        }
        let ocr = if mode == RecognitionMode::Llm {
            None
        } else {
            Some(ocr::open(ocr_choice)?)
        };
        let set_codes = scryfall.set_codes().unwrap_or_else(|e| {
            eprintln!("  ! could not load Scryfall set list ({e:#}); set-code detection limited");
            HashSet::new()
        });
        Ok(Self {
            ocr,
            llm,
            mode,
            set_codes,
            names: NameIndex::load(scryfall),
        })
    }

    pub fn has_llm(&self) -> bool {
        self.llm.is_some()
    }

    /// Locate every card in a photo of several cards (needs the LLM).
    pub fn detect_cards(&self, img: &DynamicImage) -> Result<Vec<crate::llm::NormBox>> {
        match &self.llm {
            Some(llm) => llm.detect_cards(img),
            None => bail!(
                "finding several cards at once needs a vision LLM: set [llm] provider = \"ollama\" or \
                 \"openrouter\" in mtg-scan.toml (or pass --llm-provider)"
            ),
        }
    }

    pub fn describe(&self) -> String {
        let llm = self
            .llm
            .as_ref()
            .map(|l| l.describe())
            .unwrap_or_else(|| "none".into());
        match &self.ocr {
            Some(ocr) => format!("mode={:?}, ocr={}, llm={llm}", self.mode, ocr.name()),
            None => format!("mode={:?}, llm={llm}", self.mode),
        }
    }

    /// Identify the card in `img`. `force_llm` skips OCR (used when the user
    /// rejects an OCR result).
    pub fn identify(
        &self,
        img: &DynamicImage,
        scryfall: &Scryfall,
        force_llm: bool,
    ) -> Result<Identification> {
        let mut best: Option<Identification> = None;
        let mut notes = Vec::new();

        let use_ocr = !force_llm && self.mode != RecognitionMode::Llm;
        if let (true, Some(ocr)) = (use_ocr, &self.ocr) {
            // Retry upside-down: cards are easy to place rotated.
            for candidate in [img.clone(), img.rotate180()] {
                let lines = ocr.recognize(&candidate)?;
                let hints = hints_from_ocr(&lines, &self.set_codes);
                notes.push(format!("OCR read {hints}"));
                if let Some(id) = resolve(&hints, scryfall, Method::Ocr, &self.names)? {
                    best = pick_better(best, id);
                    break;
                }
            }
            if let Some(id) = &best
                && id.quality.is_confident()
            {
                return Ok(best.unwrap());
            }
        }

        let use_llm = force_llm || self.mode != RecognitionMode::Ocr;
        if let (true, Some(llm)) = (use_llm, &self.llm) {
            match llm.read_card(img) {
                Ok((hints, conf)) => {
                    notes.push(format!("LLM read {hints} (confidence {conf:?})"));
                    if let Some(id) = resolve(&hints, scryfall, Method::Llm, &self.names)? {
                        best = pick_better(best, id);
                    }
                }
                Err(e) => notes.push(format!("LLM failed: {e:#}")),
            }
        }

        if std::env::var_os("MTG_SCAN_DEBUG").is_some() {
            for n in &notes {
                eprintln!("  [debug] {n}");
            }
        }
        match best {
            Some(id) if id.quality == MatchQuality::NameOnly => {
                Ok(refine_printing(id, img, scryfall))
            }
            Some(id) => Ok(id),
            None => bail!("could not identify the card ({})", notes.join("; ")),
        }
    }
}

/// Pick the printing whose image best matches the photo.
fn refine_printing(id: Identification, img: &DynamicImage, scryfall: &Scryfall) -> Identification {
    match printing::best_printing(scryfall, img, &id.card) {
        Ok(Some(m)) => Identification {
            card: m.card,
            quality: MatchQuality::Visual,
            note: Some(format!(
                "best of {} printings, similarity {:.2}",
                m.candidates, m.score
            )),
            ..id
        },
        Ok(None) => id,
        Err(e) => {
            eprintln!("  ! visual printing match failed ({e:#})");
            id
        }
    }
}

fn pick_better(current: Option<Identification>, new: Identification) -> Option<Identification> {
    match current {
        Some(cur) if cur.quality >= new.quality => Some(cur),
        _ => Some(new),
    }
}

/// Every card name Scryfall knows, for correcting misread names locally
/// ("Smurg, the Great Calamity" -> "Smaug, the Great Calamity").
pub struct NameIndex {
    /// (face name, normalised face name) - one entry per face.
    faces: Vec<(String, String)>,
}

impl NameIndex {
    pub fn load(scryfall: &Scryfall) -> Self {
        let names = scryfall.card_names().unwrap_or_else(|e| {
            eprintln!("  ! could not load Scryfall card names ({e:#}); name correction disabled");
            Vec::new()
        });
        Self::from_names(names)
    }

    pub fn from_names(names: Vec<String>) -> Self {
        let faces = names
            .iter()
            .flat_map(|full| {
                full.split(" // ")
                    .map(|face| (face.to_string(), normalize_name(face)))
            })
            .collect();
        Self { faces }
    }

    /// Closest real card name to `read`, if it is close enough to trust.
    pub fn closest(&self, read: &str) -> Option<&str> {
        let read = normalize_name(read);
        if read.len() < 3 {
            return None;
        }
        self.faces
            .iter()
            .map(|(name, norm)| (name, strsim::normalized_levenshtein(&read, norm)))
            .filter(|(_, score)| *score >= 0.8)
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(name, _)| name.as_str())
    }
}

fn normalize_name(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Resolve hints to a printing. Returns `Ok(None)` when nothing matched.
pub fn resolve(
    hints: &CardHints,
    scryfall: &Scryfall,
    method: Method,
    names: &NameIndex,
) -> Result<Option<Identification>> {
    let make = |card: Card, quality| Identification {
        card,
        quality,
        method,
        note: None,
    };
    let mut set_number_match = None;

    if let (Some(set), Some(number)) = (&hints.set_code, &hints.collector_number)
        && let Some(card) = scryfall.by_set_number(set, number)?
    {
        match &hints.name {
            Some(name) if names_match(name, &card.name) => {
                return Ok(Some(make(card, MatchQuality::Exact)));
            }
            Some(_) => set_number_match = Some(card),
            None => return Ok(Some(make(card, MatchQuality::SetNumber))),
        }
    }

    if let Some(name) = &hints.name {
        let mut candidates: Vec<String> = names
            .closest(name)
            .map(str::to_string)
            .into_iter()
            .collect();
        for c in name_candidates(name) {
            if !candidates.contains(&c) {
                candidates.push(c);
            }
        }
        for candidate in candidates {
            let Some(card) = scryfall.named_fuzzy(&candidate, None)? else {
                continue;
            };
            // Pick the printing from the card's own printing list rather than
            // spending more (rate-limited) lookups on a possibly-wrong set code.
            let front = card
                .name
                .split(" // ")
                .next()
                .unwrap_or(&card.name)
                .to_string();
            let printings = scryfall.printings(&front).unwrap_or_default();
            if let [only] = printings.as_slice() {
                return Ok(Some(make(only.clone(), MatchQuality::OnlyPrinting)));
            }
            if let Some(set) = &hints.set_code {
                let in_set: Vec<&Card> = printings
                    .iter()
                    .filter(|p| p.set_code.eq_ignore_ascii_case(set))
                    .collect();
                let numbered = hints.collector_number.as_ref().and_then(|n| {
                    in_set
                        .iter()
                        .find(|p| p.collector_number.eq_ignore_ascii_case(n))
                });
                if let Some(p) = numbered {
                    return Ok(Some(make((*p).clone(), MatchQuality::Exact)));
                }
                // Without a number, assume the main-set version (lowest
                // number) rather than a promo / variant from the same set.
                if let Some(p) = in_set
                    .iter()
                    .min_by_key(|p| collector_sort_key(&p.collector_number))
                {
                    return Ok(Some(make((*p).clone(), MatchQuality::NameAndSet)));
                }
            }
            return Ok(Some(make(card, MatchQuality::NameOnly)));
        }
    }

    // A set + number whose card disagrees with the printed name is only
    // trusted from OCR: vision LLMs tend to invent set codes and numbers.
    Ok(set_number_match
        .filter(|_| method == Method::Ocr)
        .map(|c| make(c, MatchQuality::SetNumber)))
}

/// Compare an OCR'd name to a Scryfall name, tolerating OCR noise and
/// matching either face of double-faced cards.
pub fn names_match(read: &str, actual: &str) -> bool {
    let norm = |s: &str| {
        s.to_lowercase()
            .chars()
            .filter(|c| c.is_alphanumeric() || c.is_whitespace())
            .collect::<String>()
    };
    let read = norm(read);
    actual
        .split(" // ")
        .any(|face| strsim::normalized_levenshtein(&read, &norm(face)) >= 0.7)
}

/// The cleaned name, then with trailing tokens dropped (OCR often turns the
/// mana cost into stray letters at the end of the title line).
fn name_candidates(name: &str) -> Vec<String> {
    let tokens: Vec<&str> = name.split_whitespace().collect();
    let mut out = Vec::new();
    for keep in (1..=tokens.len()).rev().take(3) {
        let candidate = tokens[..keep].join(" ");
        if candidate.chars().filter(|c| c.is_alphabetic()).count() >= 3 {
            out.push(candidate);
        }
    }
    out
}

/// Numeric-aware ordering for collector numbers ("9" < "107" < "107a").
fn collector_sort_key(n: &str) -> (u32, String) {
    let digits: String = n.chars().take_while(|c| c.is_ascii_digit()).collect();
    (digits.parse().unwrap_or(u32::MAX), n.to_string())
}

pub fn normalize_collector_number(n: &str) -> String {
    let n = n.trim().to_lowercase();
    match n.trim_start_matches('0') {
        "" => "0".into(),
        trimmed => trimmed.to_string(),
    }
}

static NUMBER_SLASH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(\d{1,4}[a-z]?)\s*/\s*\d{1,4}\b").unwrap());
static NUMBER_RARITY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(\d{1,4}[a-z]?)\s+[CUMRLSTPB]\b|\b[CUMRLSTPB]\s+(\d{3,4}[a-z]?)\b").unwrap()
});
static NUMBER_BARE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(0\d{2,3}|\d{3})\b").unwrap());
static SET_TOKEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b([A-Z0-9]{3,5})\b").unwrap());

/// Extract hints from OCR lines of a card cropped to its borders.
pub fn hints_from_ocr(lines: &[OcrLine], set_codes: &HashSet<String>) -> CardHints {
    CardHints {
        name: title_from_lines(lines),
        set_code: set_from_lines(lines, set_codes),
        collector_number: number_from_lines(lines),
        foil: None,
    }
}

fn title_from_lines(lines: &[OcrLine]) -> Option<String> {
    let mut top: Vec<&OcrLine> = lines
        .iter()
        .filter(|l| l.center_y() < 0.15 && l.x < 0.5)
        .collect();
    top.sort_by(|a, b| a.y.total_cmp(&b.y));
    top.into_iter().find_map(|l| {
        let cleaned: String = l
            .text
            .chars()
            .map(|c| {
                if c.is_alphabetic() || " ,'-".contains(c) {
                    c
                } else {
                    ' '
                }
            })
            .collect();
        let cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
        let cleaned = cleaned
            .trim_matches(|c: char| c == ',' || c == '-' || c == '\'')
            .trim();
        (cleaned.chars().filter(|c| c.is_alphabetic()).count() >= 3).then(|| cleaned.to_string())
    })
}

/// Bottom-left info block (collector number, rarity, set, language).
fn bottom_left_text(lines: &[OcrLine]) -> String {
    lines
        .iter()
        .filter(|l| l.center_y() > 0.85 && l.x < 0.5)
        .map(|l| l.text.as_str())
        .collect::<Vec<_>>()
        .join(" \n ")
}

fn number_from_lines(lines: &[OcrLine]) -> Option<String> {
    let text = bottom_left_text(lines);
    let found = NUMBER_SLASH
        .captures(&text)
        .and_then(|c| c.get(1))
        .or_else(|| {
            NUMBER_RARITY
                .captures(&text)
                .and_then(|c| c.get(1).or_else(|| c.get(2)))
        })
        .or_else(|| NUMBER_BARE.captures(&text).and_then(|c| c.get(1)))?;
    Some(normalize_collector_number(found.as_str()))
}

fn set_from_lines(lines: &[OcrLine], set_codes: &HashSet<String>) -> Option<String> {
    let text = bottom_left_text(lines);
    SET_TOKEN
        .captures_iter(&text)
        .map(|c| c[1].to_string())
        .filter(|t| t.chars().any(|c| c.is_ascii_alphabetic()))
        .map(|t| t.to_lowercase())
        .find(|t| set_codes.contains(t))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str, x: f32, y: f32) -> OcrLine {
        OcrLine {
            text: text.into(),
            x,
            y,
            w: 0.3,
            h: 0.02,
        }
    }

    fn sets() -> HashSet<String> {
        ["dmu", "m21", "woe", "2xm"]
            .into_iter()
            .map(String::from)
            .collect()
    }

    #[test]
    fn extracts_modern_card_hints() {
        let lines = vec![
            line("Sheoldred, the Apocalypse 2@@", 0.06, 0.05),
            line("Legendary Creature — Phyrexian Praetor", 0.06, 0.56),
            line("Deathtouch", 0.08, 0.65),
            line("0107/281 M", 0.04, 0.935),
            line("DMU • EN Chris Rallis", 0.04, 0.955),
            line("© 2022 Wizards of the Coast", 0.6, 0.955),
        ];
        let h = hints_from_ocr(&lines, &sets());
        assert_eq!(h.name.as_deref(), Some("Sheoldred, the Apocalypse"));
        assert_eq!(h.set_code.as_deref(), Some("dmu"));
        assert_eq!(h.collector_number.as_deref(), Some("107"));
    }

    #[test]
    fn extracts_new_style_footer_without_total() {
        let lines = vec![
            line("Hylda of the Icy Crown", 0.06, 0.05),
            line("M 0206", 0.04, 0.935),
            line("WOE • EN Some Artist", 0.04, 0.955),
        ];
        let h = hints_from_ocr(&lines, &sets());
        assert_eq!(h.collector_number.as_deref(), Some("206"));
        assert_eq!(h.set_code.as_deref(), Some("woe"));
    }

    #[test]
    fn ignores_unknown_set_like_tokens_and_copyright_year() {
        let lines = vec![
            line("Opt", 0.06, 0.05),
            line("XYZ TM & © 2020 Wizards", 0.55, 0.955),
            line("ABCD", 0.04, 0.955),
        ];
        let h = hints_from_ocr(&lines, &sets());
        assert_eq!(h.set_code, None);
        assert_eq!(h.collector_number, None);
        assert_eq!(h.name.as_deref(), Some("Opt"));
    }

    #[test]
    fn collector_numbers_sort_numerically() {
        let mut v = vec!["436", "107a", "9", "107", "★1"];
        v.sort_by_key(|n| collector_sort_key(n));
        assert_eq!(v, vec!["9", "107", "107a", "436", "★1"]);
    }

    #[test]
    fn normalizes_collector_numbers() {
        assert_eq!(normalize_collector_number("0107"), "107");
        assert_eq!(normalize_collector_number("12a"), "12a");
        assert_eq!(normalize_collector_number("000"), "0");
    }

    #[test]
    fn names_match_tolerates_noise_and_dfc() {
        assert!(names_match(
            "Sheoldred, the Apocalypse",
            "Sheoldred, the Apocalypse"
        ));
        assert!(names_match(
            "Sheo1dred the Apocalypse",
            "Sheoldred, the Apocalypse"
        ));
        assert!(names_match(
            "Delver of Secrets",
            "Delver of Secrets // Insectile Aberration"
        ));
        assert!(!names_match("Lightning Bolt", "Sheoldred, the Apocalypse"));
    }

    #[test]
    fn name_index_corrects_misreads() {
        let index = NameIndex::from_names(vec![
            "Smaug, the Great Calamity // Spew Flame".into(),
            "Smaug the Magnificent".into(),
            "Opt".into(),
        ]);
        assert_eq!(
            index.closest("Smurg, the Great Calamity"),
            Some("Smaug, the Great Calamity")
        );
        assert_eq!(index.closest("spew flame"), Some("Spew Flame"));
        assert_eq!(index.closest("Completely Different"), None);
        assert_eq!(index.closest("Op"), None);
    }

    #[test]
    fn name_candidates_drop_trailing_noise() {
        assert_eq!(
            name_candidates("Lightning Bolt R"),
            vec!["Lightning Bolt R", "Lightning Bolt", "Lightning"]
        );
    }
}
