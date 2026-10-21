//! Collection persistence behind [`CollectionStore`]: a Markdown file
//! (iteration 1) or PostgreSQL with price history (iteration 2).

pub mod markdown;
pub mod postgres;

use anyhow::{Result, bail};
use chrono::NaiveDate;

use crate::model::{CollectionEntry, Finish, PricePoint, Prices};

/// TCGplayer value of the cards owned now, at the prices of `since` and at
/// their latest prices, over the rows priced on both dates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ValueChange {
    pub before: f64,
    pub now: f64,
    pub rows: u32,
}

/// A collection row's prices on a past date and now (see
/// [`CollectionStore::price_moves`]).
#[derive(Debug, Clone, PartialEq)]
pub struct PriceMove {
    pub scryfall_id: String,
    pub finish: Finish,
    pub quantity: u32,
    /// The latest price point on or before the date asked for.
    pub then: PricePoint,
    /// The latest price point.
    pub now: PricePoint,
}

pub trait CollectionStore {
    fn entries(&self) -> Result<Vec<CollectionEntry>>;

    /// Add copies of a printing, merging with an existing row for the same
    /// Scryfall ID + finish. Returns the resulting row.
    fn add(&mut self, entry: CollectionEntry) -> Result<CollectionEntry>;

    /// Remove up to `quantity` copies. Returns the row as it now stands
    /// (quantity 0 when it was deleted), or `None` if it wasn't present.
    fn remove(
        &mut self,
        scryfall_id: &str,
        finish: Finish,
        quantity: u32,
    ) -> Result<Option<CollectionEntry>>;

    /// Set the number of copies of a row; 0 deletes it. Returns the row as
    /// it now stands (quantity 0 when deleted), or `None` if it wasn't present.
    fn set_quantity(
        &mut self,
        scryfall_id: &str,
        finish: Finish,
        quantity: u32,
    ) -> Result<Option<CollectionEntry>>;

    /// Move a row to another printing and/or finish: the row is replaced by
    /// `to` (its quantity, prices and card details), merging into an existing
    /// row for that printing + finish. Returns the resulting row, or `None`
    /// if the original row wasn't present (then nothing changes).
    fn relocate(
        &mut self,
        scryfall_id: &str,
        finish: Finish,
        to: CollectionEntry,
    ) -> Result<Option<CollectionEntry>>;

    /// Record current prices for a printing + finish.
    fn update_prices(&mut self, scryfall_id: &str, finish: Finish, prices: Prices) -> Result<()>;

    /// Price points over time, oldest first.
    fn price_history(&self, _scryfall_id: &str, _finish: Finish) -> Result<Vec<PricePoint>> {
        bail!(
            "price history is only kept by the postgres storage backend ([storage] backend = \"postgres\")"
        )
    }

    /// Rows owned now that were priced on or before `since` and again later.
    fn price_moves(&self, _since: NaiveDate) -> Result<Vec<PriceMove>> {
        bail!(
            "price history is only kept by the postgres storage backend ([storage] backend = \"postgres\")"
        )
    }

    /// How the collection's value moved since `since`; `None` without price
    /// history (Markdown) or when no card was priced back then.
    fn value_change(&self, _since: NaiveDate) -> Result<Option<ValueChange>> {
        Ok(None)
    }

    /// Human-readable location, e.g. a file path.
    fn location(&self) -> String;
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Totals {
    pub printings: usize,
    pub cards: u32,
    pub tcgplayer: f64,
    pub cardkingdom: f64,
    pub cardkingdom_buylist: f64,
}

pub fn totals(entries: &[CollectionEntry]) -> Totals {
    entries.iter().fold(Totals::default(), |mut t, e| {
        let q = e.quantity as f64;
        t.printings += 1;
        t.cards += e.quantity;
        t.tcgplayer += e.prices.tcgplayer.unwrap_or(0.0) * q;
        t.cardkingdom += e.prices.cardkingdom.unwrap_or(0.0) * q;
        t.cardkingdom_buylist += e.prices.cardkingdom_buylist.unwrap_or(0.0) * q;
        t
    })
}
