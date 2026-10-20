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

/// A specific printing of a card, as resolved from Scryfall.
#[derive(Debug, Clone)]
pub struct Card {
    pub scryfall_id: String,
    pub name: String,
    pub set_code: String,
    pub set_name: String,
    pub collector_number: String,
    pub rarity: String,
    pub finishes: Vec<Finish>,
    pub tcgplayer_id: Option<u64>,
    pub tcgplayer_etched_id: Option<u64>,
    /// ~146x204 card image, used for visual printing matching.
    pub image_small: Option<String>,
    /// Scryfall's daily TCGplayer-sourced prices, used as a fallback.
    pub scryfall_usd: Option<f64>,
    pub scryfall_usd_foil: Option<f64>,
    pub scryfall_usd_etched: Option<f64>,
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

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Prices {
    pub tcgplayer: Option<f64>,
    pub tcgplayer_url: Option<String>,
    pub cardkingdom: Option<f64>,
    pub cardkingdom_buylist: Option<f64>,
    pub cardkingdom_url: Option<String>,
}

/// One row of the collection: a printing + finish and how many copies.
#[derive(Debug, Clone, PartialEq)]
pub struct CollectionEntry {
    pub scryfall_id: String,
    pub name: String,
    pub set_code: String,
    pub set_name: String,
    pub collector_number: String,
    pub rarity: String,
    pub finish: Finish,
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
            finish,
            quantity,
            prices,
            added: today,
            price_updated: Some(today),
        }
    }

    pub fn same_printing(&self, scryfall_id: &str, finish: Finish) -> bool {
        self.scryfall_id == scryfall_id && self.finish == finish
    }

    pub fn scryfall_url(&self) -> String {
        format!(
            "https://scryfall.com/card/{}/{}",
            self.set_code.to_lowercase(),
            self.collector_number
        )
    }
}

/// Prices observed on one day (postgres backend only).
#[derive(Debug, Clone, PartialEq)]
pub struct PricePoint {
    pub date: NaiveDate,
    pub tcgplayer: Option<f64>,
    pub cardkingdom: Option<f64>,
    pub cardkingdom_buylist: Option<f64>,
}

pub fn format_usd(v: Option<f64>) -> String {
    v.map(|p| format!("${p:.2}")).unwrap_or_else(|| "—".into())
}
