//! Refreshing stored prices. Used by `mtg-scan reprice` (every row) and by
//! the live scanner, which reprices rows not yet priced today one at a time
//! while it is idle, so price history builds up whenever the app runs,
//! with no OS scheduler involved.

use std::collections::VecDeque;

use anyhow::Result;
use chrono::NaiveDate;

use crate::app::App;
use crate::model::{CollectionEntry, Finish, Prices};

struct Item {
    scryfall_id: String,
    finish: Finish,
    label: String,
    old: Prices,
}

/// One repriced row. `new` is `None` when Scryfall no longer knows the card.
pub struct Repriced {
    pub label: String,
    pub old: Prices,
    pub new: Option<Prices>,
}

pub struct Repricer {
    queue: VecDeque<Item>,
    total: usize,
}

impl Repricer {
    /// Rows whose prices weren't refreshed on `today` (every row when `force`).
    pub fn plan(entries: &[CollectionEntry], today: NaiveDate, force: bool) -> Self {
        let queue: VecDeque<Item> = entries
            .iter()
            .filter(|e| force || e.price_updated != Some(today))
            .map(|e| Item {
                scryfall_id: e.scryfall_id.clone(),
                finish: e.finish,
                label: format!(
                    "{} ({} #{}, {})",
                    e.name, e.set_code, e.collector_number, e.finish
                ),
                old: e.prices.clone(),
            })
            .collect();
        Self {
            total: queue.len(),
            queue,
        }
    }

    pub fn total(&self) -> usize {
        self.total
    }

    pub fn remaining(&self) -> usize {
        self.queue.len()
    }

    pub fn is_done(&self) -> bool {
        self.queue.is_empty()
    }

    /// Stop early (e.g. after a network error); the rest waits for next time.
    pub fn abandon(&mut self) {
        self.queue.clear();
    }

    /// Reprice the next row and record the prices. `Ok(None)` when done.
    pub fn step(&mut self, app: &mut App) -> Result<Option<Repriced>> {
        let Some(item) = self.queue.pop_front() else {
            return Ok(None);
        };
        let Some(card) = app.scryfall.by_id(&item.scryfall_id)? else {
            return Ok(Some(Repriced {
                label: item.label,
                old: item.old,
                new: None,
            }));
        };
        let prices = app.prices.price(&card, item.finish);
        app.store
            .update_prices(&item.scryfall_id, item.finish, prices.clone())?;
        Ok(Some(Repriced {
            label: item.label,
            old: item.old,
            new: Some(prices),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, priced: Option<NaiveDate>) -> CollectionEntry {
        CollectionEntry {
            scryfall_id: id.into(),
            name: "Opt".into(),
            set_code: "XLN".into(),
            set_name: "Ixalan".into(),
            collector_number: "65".into(),
            rarity: "common".into(),
            finish: Finish::Nonfoil,
            quantity: 1,
            prices: Prices::default(),
            added: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            price_updated: priced,
        }
    }

    #[test]
    fn plans_only_rows_not_priced_today_unless_forced() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 21).unwrap();
        let yesterday = today.pred_opt().unwrap();
        let rows = vec![
            entry("a", Some(today)),
            entry("b", Some(yesterday)),
            entry("c", None),
        ];

        let daily = Repricer::plan(&rows, today, false);
        assert_eq!(daily.total(), 2);
        let ids: Vec<&str> = daily.queue.iter().map(|i| i.scryfall_id.as_str()).collect();
        assert_eq!(ids, vec!["b", "c"]);
        assert_eq!(daily.queue[0].label, "Opt (XLN #65, nonfoil)");

        let mut forced = Repricer::plan(&rows, today, true);
        assert_eq!(forced.total(), 3);
        forced.abandon();
        assert!(forced.is_done());
        assert_eq!(forced.remaining(), 0);
    }
}
