use std::fmt;
use std::str::FromStr;

use anyhow::bail;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

/// Physical finish of a printing. Prices differ wildly between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Finish {
    Nonfoil,
    Foil,
    Etched,
}

impl Finish {
    pub fn as_str(self) -> &'static str {
        match self {
            Finish::Nonfoil => "nonfoil",
            Finish::Foil => "foil",
            Finish::Etched => "etched",
        }
    }

    /// Cycle order used by the scanner's finish toggle key.
    pub fn next(self) -> Self {
        match self {
            Finish::Nonfoil => Finish::Foil,
            Finish::Foil => Finish::Etched,
            Finish::Etched => Finish::Nonfoil,
        }
    }
}

impl From<Finish> for mtg_scan_shared::Finish {
    fn from(f: Finish) -> Self {
        match f {
            Finish::Nonfoil => Self::Nonfoil,
            Finish::Foil => Self::Foil,
            Finish::Etched => Self::Etched,
        }
    }
}

impl From<mtg_scan_shared::Finish> for Finish {
    fn from(f: mtg_scan_shared::Finish) -> Self {
        match f {
            mtg_scan_shared::Finish::Nonfoil => Self::Nonfoil,
            mtg_scan_shared::Finish::Foil => Self::Foil,
            mtg_scan_shared::Finish::Etched => Self::Etched,
        }
    }
}

impl fmt::Display for Finish {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Finish {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> anyhow::Result<Self> {
        match s.trim().to_lowercase().as_str() {
            "nonfoil" | "normal" | "" => Ok(Finish::Nonfoil),
            "foil" => Ok(Finish::Foil),
            "etched" => Ok(Finish::Etched),
            other => bail!("unknown finish '{other}'"),
        }
    }
}

/// A specific printing of a card, as resolved from Scryfall (or the local
/// card cache filled from Scryfall's bulk data).
#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    pub scryfall_id: String,
    /// Shared by every printing of the same card.
    pub oracle_id: Option<String>,
    pub name: String,
    /// Printed language, e.g. `en`, `ja`.
    pub lang: String,
    pub set_code: String,
    pub set_name: String,
    pub collector_number: String,
    pub rarity: String,
    /// Color letters (`W`, `U`, `B`, `R`, `G`); empty for colorless.
    pub colors: Vec<String>,
    /// e.g. "Legendary Creature — Phyrexian Praetor".
    pub type_line: String,
    pub finishes: Vec<Finish>,
    pub tcgplayer_id: Option<u64>,
    pub tcgplayer_etched_id: Option<u64>,
    pub cardmarket_id: Option<u64>,
    /// ~146x204 card image, used for visual printing matching.
    pub image_small: Option<String>,
    /// 488x680 card image.
    pub image_normal: Option<String>,
    /// Scryfall's daily prices: TCGplayer (USD), Cardmarket (EUR) and
    /// Cardhoarder (MTGO tix).
    pub scryfall_usd: Option<f64>,
    pub scryfall_usd_foil: Option<f64>,
    pub scryfall_usd_etched: Option<f64>,
    pub scryfall_eur: Option<f64>,
    pub scryfall_eur_foil: Option<f64>,
    pub scryfall_tix: Option<f64>,
}

impl Card {
    /// Pick the closest finish this printing actually exists in.
    pub fn best_finish(&self, wanted: Finish) -> Finish {
        if self.finishes.is_empty() || self.finishes.contains(&wanted) {
            return wanted;
        }
        let fallback = match wanted {
            Finish::Etched => [Finish::Foil, Finish::Nonfoil],
            Finish::Foil => [Finish::Etched, Finish::Nonfoil],
            Finish::Nonfoil => [Finish::Foil, Finish::Etched],
        };
        fallback
            .into_iter()
            .find(|f| self.finishes.contains(f))
            .unwrap_or(wanted)
    }
}

/// A printing + finish's prices: TCGplayer (USD), Cardmarket (EUR) and MTGO
/// tix from Scryfall, Card Kingdom (USD) when that source is switched on.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Prices {
    pub tcgplayer: Option<f64>,
    pub tcgplayer_url: Option<String>,
    /// Cardmarket trend price, in euros.
    pub cardmarket: Option<f64>,
    /// MTGO (Cardhoarder) price in tix.
    pub tix: Option<f64>,
    pub cardkingdom: Option<f64>,
    pub cardkingdom_buylist: Option<f64>,
    pub cardkingdom_url: Option<String>,
}

pub use mtg_scan_shared::Condition;

/// Which collection row (of one account): the printing, finish, condition
/// and language of the copies.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RowKey {
    pub scryfall_id: String,
    pub finish: Finish,
    pub condition: Condition,
    /// Scryfall language code, e.g. `en`.
    pub language: String,
}

impl RowKey {
    /// Near Mint copies in `language`.
    #[cfg(test)]
    pub fn new(scryfall_id: &str, finish: Finish, language: &str) -> Self {
        Self {
            scryfall_id: scryfall_id.to_string(),
            finish,
            condition: Condition::Nm,
            language: language.to_string(),
        }
    }
}

/// One row of the collection: copies of a printing + finish in one
/// condition and language.
#[derive(Debug, Clone, PartialEq)]
pub struct CollectionEntry {
    pub scryfall_id: String,
    pub name: String,
    pub set_code: String,
    pub set_name: String,
    pub collector_number: String,
    pub rarity: String,
    /// From Scryfall; empty when not looked up (the Markdown file doesn't
    /// store them).
    pub colors: Vec<String>,
    pub type_line: String,
    pub finish: Finish,
    pub condition: Condition,
    /// Scryfall language code of the copies, e.g. `en`.
    pub language: String,
    pub quantity: u32,
    pub prices: Prices,
    pub added: NaiveDate,
    pub price_updated: Option<NaiveDate>,
}

impl CollectionEntry {
    pub fn from_card(card: &Card, finish: Finish, quantity: u32, prices: Prices) -> Self {
        let today = chrono::Local::now().date_naive();
        Self {
            scryfall_id: card.scryfall_id.clone(),
            name: card.name.clone(),
            set_code: card.set_code.to_uppercase(),
            set_name: card.set_name.clone(),
            collector_number: card.collector_number.clone(),
            rarity: card.rarity.clone(),
            colors: card.colors.clone(),
            type_line: card.type_line.clone(),
            finish,
            condition: Condition::Nm,
            language: card.lang.clone(),
            quantity,
            prices,
            added: today,
            price_updated: Some(today),
        }
    }

    pub fn key(&self) -> RowKey {
        RowKey {
            scryfall_id: self.scryfall_id.clone(),
            finish: self.finish,
            condition: self.condition,
            language: self.language.clone(),
        }
    }

    pub fn same_printing(&self, scryfall_id: &str, finish: Finish) -> bool {
        self.scryfall_id == scryfall_id && self.finish == finish
    }

    #[cfg(test)]
    pub fn has_key(&self, key: &RowKey) -> bool {
        self.scryfall_id == key.scryfall_id
            && self.finish == key.finish
            && self.condition == key.condition
            && self.language == key.language
    }

    pub fn scryfall_url(&self) -> String {
        format!(
            "https://scryfall.com/card/{}/{}",
            self.set_code.to_lowercase(),
            self.collector_number
        )
    }
}

/// Prices observed on one day.
#[derive(Debug, Clone, PartialEq)]
pub struct PricePoint {
    pub date: NaiveDate,
    pub tcgplayer: Option<f64>,
    pub cardmarket: Option<f64>,
    pub tix: Option<f64>,
    pub cardkingdom: Option<f64>,
    pub cardkingdom_buylist: Option<f64>,
}

/// "foil", "foil, LP, Japanese": the finish, then the condition and
/// language when they aren't Near Mint and English.
pub fn copies(finish: Finish, condition: Condition, language: &str) -> String {
    let mut text = finish.to_string();
    if condition != Condition::Nm {
        text += &format!(", {}", condition.short());
    }
    if language != "en" {
        text += &format!(", {}", mtg_scan_shared::language_name(language));
    }
    text
}

pub fn format_usd(v: Option<f64>) -> String {
    v.map(|p| format!("${p:.2}")).unwrap_or_else(|| "—".into())
}

pub fn format_eur(v: Option<f64>) -> String {
    v.map(|p| format!("€{p:.2}")).unwrap_or_else(|| "—".into())
}
