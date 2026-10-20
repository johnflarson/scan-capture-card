//! Collection persistence. Iteration 1 stores everything in a Markdown file;
//! iteration 2 will add a PostgreSQL implementation of [`CollectionStore`].

pub mod markdown;

use anyhow::Result;

use crate::model::{CollectionEntry, Finish, Prices};

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

    fn update_prices(&mut self, scryfall_id: &str, finish: Finish, prices: Prices) -> Result<()>;

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
