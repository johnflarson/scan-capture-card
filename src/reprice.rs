//! The daily price refresh: once a day every owned printing gets a price
//! point from the card cache (Scryfall's TCGplayer, Cardmarket and MTGO
//! prices, kept current by `bulk`), plus Card Kingdom's when that source is
//! switched on. Runs inside the app after the daily card data sync (no OS
//! scheduler), and on `mtg-scan reprice`.

use anyhow::Result;
use chrono::Local;

use crate::app::App;
use crate::model::Prices;

/// The `app_job` row holding the last day prices were recorded.
pub const JOB: &str = "daily_prices";

#[derive(Debug, PartialEq)]
pub struct Refreshed {
    /// Rows priced from Scryfall's data.
    pub rows: usize,
    /// Rows that also got a Card Kingdom price.
    pub card_kingdom: usize,
}

/// Record today's prices for every owned row, unless that was done today
/// already (`force` does it again). `None` when skipped or when the store
/// has no card cache.
pub fn refresh(app: &mut App, force: bool) -> Result<Option<Refreshed>> {
    let today = Local::now().date_naive();
    let today_text = today.to_string();
    let done = match app.store.card_cache() {
        Some(cache) => cache.job(JOB)?,
        None => return Ok(None),
    };
    if !force && done.is_some_and(|j| j.detail == today_text) {
        return Ok(None);
    }
    let rows = app.store.record_cached_prices(today)?;
    let mut card_kingdom = 0;
    if let Some(ck) = &app.prices.cardkingdom {
        for e in app.store.entries()? {
            if let Some(p) = ck.price(&e.scryfall_id, e.finish) {
                // Only the Card Kingdom columns: Scryfall's prices for today stay.
                let prices = Prices {
                    cardkingdom: p.retail,
                    cardkingdom_buylist: p.buylist,
                    ..Prices::default()
                };
                app.store.update_prices(&e.scryfall_id, e.finish, prices)?;
                card_kingdom += 1;
            }
        }
    }
    if let Some(cache) = app.store.card_cache() {
        cache.finish_job(JOB, &today_text)?;
    }
    Ok(Some(Refreshed { rows, card_kingdom }))
}

#[cfg(test)]
mod tests {
    use chrono::Local;

    use crate::model::{Card, CollectionEntry, Finish, Prices};
    use crate::store::sqlite::SqliteStore;
    use crate::store::{CardCache, CollectionStore};

    fn card(id: &str, usd: f64, usd_foil: f64, eur: f64) -> Card {
        Card {
            scryfall_id: id.into(),
            oracle_id: None,
            name: "Opt".into(),
            lang: "en".into(),
            set_code: "xln".into(),
            set_name: "Ixalan".into(),
            collector_number: "65".into(),
            rarity: "common".into(),
            colors: vec!["U".into()],
            type_line: "Instant".into(),
            finishes: vec![Finish::Nonfoil, Finish::Foil],
            tcgplayer_id: None,
            tcgplayer_etched_id: None,
            cardmarket_id: None,
            image_small: None,
            image_normal: None,
            scryfall_usd: Some(usd),
            scryfall_usd_foil: Some(usd_foil),
            scryfall_usd_etched: None,
            scryfall_eur: Some(eur),
            scryfall_eur_foil: None,
            scryfall_tix: Some(0.01),
        }
    }

    fn owned(id: &str, finish: Finish) -> CollectionEntry {
        CollectionEntry {
            scryfall_id: id.into(),
            name: String::new(),
            set_code: String::new(),
            set_name: String::new(),
            collector_number: String::new(),
            rarity: String::new(),
            colors: Vec::new(),
            type_line: String::new(),
            finish,
            quantity: 1,
            prices: Prices::default(),
            added: Local::now().date_naive(),
            price_updated: None,
        }
    }

    #[test]
    fn owned_rows_are_priced_from_the_cache_per_finish() {
        let mut store = SqliteStore::open_in_memory().unwrap();
        store.add(owned("a", Finish::Nonfoil)).unwrap();
        store.add(owned("a", Finish::Foil)).unwrap();
        store.add(owned("not-cached", Finish::Nonfoil)).unwrap();
        store
            .store_cards(&[card("a", 0.20, 1.50, 0.15), card("unowned", 9.0, 9.0, 9.0)])
            .unwrap();

        let today = Local::now().date_naive();
        assert_eq!(store.record_cached_prices(today).unwrap(), 2);
        let nonfoil = store.price_history("a", Finish::Nonfoil).unwrap();
        assert_eq!(
            (nonfoil[0].tcgplayer, nonfoil[0].cardmarket, nonfoil[0].tix),
            (Some(0.20), Some(0.15), Some(0.01))
        );
        let foil = store.price_history("a", Finish::Foil).unwrap();
        assert_eq!((foil[0].tcgplayer, foil[0].cardmarket), (Some(1.50), None));
        assert!(
            store
                .price_history("not-cached", Finish::Nonfoil)
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .price_history("unowned", Finish::Nonfoil)
                .unwrap()
                .is_empty()
        );

        // Card Kingdom recorded the same day keeps Scryfall's prices, and a
        // second snapshot keeps Card Kingdom's.
        store
            .update_prices(
                "a",
                Finish::Nonfoil,
                Prices {
                    cardkingdom: Some(0.35),
                    ..Prices::default()
                },
            )
            .unwrap();
        assert_eq!(store.record_cached_prices(today).unwrap(), 2);
        let point = &store.price_history("a", Finish::Nonfoil).unwrap()[0];
        assert_eq!(
            (point.tcgplayer, point.cardmarket, point.cardkingdom),
            (Some(0.20), Some(0.15), Some(0.35))
        );
        let row = store
            .entries()
            .unwrap()
            .into_iter()
            .find(|e| e.scryfall_id == "a" && e.finish == Finish::Nonfoil)
            .unwrap();
        assert_eq!(
            (row.prices.tcgplayer, row.price_updated),
            (Some(0.20), Some(today))
        );
    }
}
