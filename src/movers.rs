//! `mtg-scan movers`: which cards gained or lost the most value over a
//! period, from the price history.

use chrono::NaiveDate;

use crate::model::{PricePoint, format_eur, format_usd};
use crate::store::PriceMove;

/// Where a price comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Tcgplayer,
    Cardmarket,
    CardKingdom,
}

impl Source {
    pub const ALL: [Source; 3] = [Source::Tcgplayer, Source::Cardmarket, Source::CardKingdom];

    pub fn label(self) -> &'static str {
        match self {
            Source::Tcgplayer => "TCGplayer",
            Source::Cardmarket => "Cardmarket",
            Source::CardKingdom => "Card Kingdom",
        }
    }

    /// An amount in this source's currency: `$1.23` or `€1.23`.
    pub fn money(self, v: f64) -> String {
        match self {
            Source::Cardmarket => format_eur(Some(v)),
            _ => format_usd(Some(v)),
        }
    }

    fn price(self, p: &PricePoint) -> Option<f64> {
        match self {
            Source::Tcgplayer => p.tcgplayer,
            Source::Cardmarket => p.cardmarket,
            Source::CardKingdom => p.cardkingdom,
        }
    }
}

/// One row's price change at one source.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    /// Index into the moves the report was made from.
    pub row: usize,
    pub quantity: u32,
    /// Per copy.
    pub then: f64,
    pub now: f64,
}

impl Change {
    /// Change for all copies owned.
    pub fn total(&self) -> f64 {
        (self.now - self.then) * self.quantity as f64
    }

    pub fn percent(&self) -> Option<f64> {
        (self.then > 0.0).then(|| (self.now - self.then) / self.then * 100.0)
    }
}

/// The collection's value at one source then and now, and the biggest movers.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceReport {
    pub source: Source,
    /// Value of the rows priced at this source on both dates.
    pub before: f64,
    pub now: f64,
    pub rows: usize,
    /// Largest gains for the copies owned, biggest first.
    pub risers: Vec<Change>,
    /// Largest losses, biggest first.
    pub fallers: Vec<Change>,
}

impl SourceReport {
    pub fn percent(&self) -> Option<f64> {
        (self.before > 0.0).then(|| (self.now - self.before) / self.before * 100.0)
    }
}

/// Rank `moves` at each source, keeping `top` risers and fallers.
pub fn report(moves: &[PriceMove], top: usize) -> Vec<SourceReport> {
    Source::ALL
        .into_iter()
        .map(|source| {
            let changes: Vec<Change> = moves
                .iter()
                .enumerate()
                .filter_map(|(row, m)| {
                    Some(Change {
                        row,
                        quantity: m.quantity,
                        then: source.price(&m.then)?,
                        now: source.price(&m.now)?,
                    })
                })
                .collect();
            let value = |f: fn(&Change) -> f64| {
                changes
                    .iter()
                    .map(|c| f(c) * c.quantity as f64)
                    .sum::<f64>()
            };
            let mut risers: Vec<Change> = changes
                .iter()
                .filter(|c| c.total() > 0.0)
                .cloned()
                .collect();
            risers.sort_by(|a, b| b.total().total_cmp(&a.total()));
            risers.truncate(top);
            let mut fallers: Vec<Change> = changes
                .iter()
                .filter(|c| c.total() < 0.0)
                .cloned()
                .collect();
            fallers.sort_by(|a, b| a.total().total_cmp(&b.total()));
            fallers.truncate(top);
            SourceReport {
                source,
                before: value(|c| c.then),
                now: value(|c| c.now),
                rows: changes.len(),
                risers,
                fallers,
            }
        })
        .collect()
}

/// The date `days` before `today`.
pub fn since(today: NaiveDate, days: u32) -> NaiveDate {
    today - chrono::Days::new(days as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Finish;

    fn point(day: u32, tcg: Option<f64>, ck: Option<f64>) -> PricePoint {
        PricePoint {
            date: NaiveDate::from_ymd_opt(2026, 10, day).unwrap(),
            tcgplayer: tcg,
            cardmarket: None,
            tix: None,
            cardkingdom: ck,
            cardkingdom_buylist: None,
        }
    }

    fn row(
        quantity: u32,
        then: (Option<f64>, Option<f64>),
        now: (Option<f64>, Option<f64>),
    ) -> PriceMove {
        PriceMove {
            scryfall_id: String::new(),
            finish: Finish::Nonfoil,
            quantity,
            then: point(1, then.0, then.1),
            now: point(8, now.0, now.1),
        }
    }

    #[test]
    fn ranks_by_change_for_the_copies_owned() {
        let moves = [
            // +$1 per copy, 4 copies: +$4.
            row(4, (Some(1.0), Some(2.0)), (Some(2.0), Some(2.0))),
            // +$3 for one copy.
            row(1, (Some(10.0), Some(12.0)), (Some(13.0), Some(11.0))),
            // −$5.
            row(1, (Some(20.0), None), (Some(15.0), Some(16.0))),
            // Unchanged.
            row(2, (Some(0.5), Some(1.0)), (Some(0.5), Some(1.0))),
        ];
        let r = report(&moves, 5);
        let tcg = &r[0];
        assert_eq!(tcg.source, Source::Tcgplayer);
        assert_eq!(tcg.rows, 4);
        assert_eq!(
            (tcg.before, tcg.now),
            (4.0 + 10.0 + 20.0 + 1.0, 8.0 + 13.0 + 15.0 + 1.0)
        );
        let rows = |v: &[Change]| v.iter().map(|c| c.row).collect::<Vec<_>>();
        assert_eq!(rows(&tcg.risers), [0, 1]);
        assert_eq!(rows(&tcg.fallers), [2]);
        assert_eq!(tcg.fallers[0].total(), -5.0);
        assert_eq!(tcg.risers[1].percent(), Some(30.0));

        // No Cardmarket prices at all: nothing to report.
        assert_eq!((r[1].source, r[1].rows), (Source::Cardmarket, 0));
        // Card Kingdom skips the row with no price back then.
        let ck = &r[2];
        assert_eq!(ck.rows, 3);
        assert_eq!(rows(&ck.fallers), [1]);
        assert!(ck.risers.is_empty());

        assert_eq!(report(&moves, 1)[0].risers.len(), 1);
    }

    #[test]
    fn since_counts_back_days() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 21).unwrap();
        assert_eq!(
            since(today, 7),
            NaiveDate::from_ymd_opt(2026, 10, 14).unwrap()
        );
    }
}
