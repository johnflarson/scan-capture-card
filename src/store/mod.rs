//! Collection persistence behind [`CollectionStore`]: SQLite (the default),
//! PostgreSQL, or a Markdown file. The two databases keep daily price
//! history; card details come from Scryfall.
//!
//! A database store works on one account's collection at a time
//! ([`CollectionStore::set_user`]); without one it uses the first admin's,
//! creating an "admin" account without a password when there is none (the
//! setup page then claims it). Prices and card data are shared.

pub mod markdown;
pub mod postgres;
pub mod sqlite;

use std::collections::HashMap;

use anyhow::{Result, bail};
use chrono::{DateTime, Local, NaiveDate, Utc};

use crate::model::{Card, CollectionEntry, Condition, Finish, PricePoint, Prices, RowKey};
use crate::scryfall::Scryfall;

/// TCGplayer value of the cards owned now, at the prices of `since` and at
/// their latest prices, over the rows priced on both dates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ValueChange {
    pub before: f64,
    pub now: f64,
    pub rows: u32,
}

/// An account as copied between databases (`db copy`), password hash
/// included.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountRow {
    pub id: i64,
    pub name: String,
    pub password_hash: Option<String>,
    pub is_admin: bool,
    pub disabled: bool,
    pub created_at: DateTime<Utc>,
}

/// A printing + finish's prices on a past date and now, with the copies of
/// it the account owns (see [`CollectionStore::price_moves`]).
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
    /// Work on this account's collection from now on.
    fn set_user(&mut self, _user_id: i64) {}

    /// The account whose collection this is (database stores).
    #[cfg(test)]
    fn user(&self) -> Result<i64> {
        bail!("this storage backend has no accounts")
    }

    /// The account's rows.
    fn entries(&self) -> Result<Vec<CollectionEntry>>;

    /// Add copies, merging with an existing row with the same key (printing,
    /// finish, condition, language). Returns the resulting row.
    fn add(&mut self, entry: CollectionEntry) -> Result<CollectionEntry>;

    /// Remove up to `quantity` copies. Returns the row as it now stands
    /// (quantity 0 when it was deleted), or `None` if it wasn't present.
    fn remove(&mut self, key: &RowKey, quantity: u32) -> Result<Option<CollectionEntry>>;

    /// Set the number of copies of a row; 0 deletes it. Returns the row as
    /// it now stands (quantity 0 when deleted), or `None` if it wasn't present.
    fn set_quantity(&mut self, key: &RowKey, quantity: u32) -> Result<Option<CollectionEntry>>;

    /// Change a row's printing, finish, condition or language: the row is
    /// replaced by `to` (its key, quantity, prices and card details), merging
    /// into an existing row with that key. Returns the resulting row, or
    /// `None` if the original row wasn't present (then nothing changes).
    fn relocate(&mut self, key: &RowKey, to: CollectionEntry) -> Result<Option<CollectionEntry>>;

    /// Record current prices for a printing + finish.
    fn update_prices(&mut self, scryfall_id: &str, finish: Finish, prices: Prices) -> Result<()>;

    /// Copy rows in (e.g. from `collection.md`), setting quantities rather
    /// than adding to them, and recording each row's prices as a price point
    /// on the day they were taken. Returns the number of rows.
    fn import(&mut self, _entries: &[CollectionEntry]) -> Result<usize> {
        bail!("this storage backend can't import; use sqlite or postgres")
    }

    /// Record `date`'s prices for every printing + finish anyone owns, from
    /// the card cache (Scryfall's daily TCGplayer, Cardmarket and MTGO
    /// prices). Returns the printings priced.
    fn record_cached_prices(&mut self, _date: NaiveDate) -> Result<usize> {
        bail!("this storage backend has no card cache")
    }

    /// Every printing + finish anyone owns (for pricing them all).
    fn owned_printings(&self) -> Result<Vec<(String, Finish)>> {
        let mut all: Vec<(String, Finish)> = self
            .entries()?
            .into_iter()
            .map(|e| (e.scryfall_id, e.finish))
            .collect();
        all.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.as_str().cmp(b.1.as_str())));
        all.dedup();
        Ok(all)
    }

    /// Every account (for copying a database); none before accounts existed.
    fn account_rows(&self) -> Result<Vec<AccountRow>> {
        Ok(Vec::new())
    }

    /// Copy accounts in, matching existing ones by name (their password and
    /// flags are kept). Returns each copied account's id here by its id there.
    fn import_accounts(&mut self, _accounts: &[AccountRow]) -> Result<HashMap<i64, i64>> {
        bail!("this storage backend has no accounts")
    }

    /// Every price point of every printing + finish, owned or not (for
    /// copying a database).
    fn all_price_points(&self) -> Result<Vec<(String, Finish, PricePoint)>> {
        bail!("price history is only kept by the sqlite and postgres storage backends")
    }

    /// Write price points as they are (for copying a database). Returns how
    /// many were written.
    fn import_price_points(&mut self, _points: &[(String, Finish, PricePoint)]) -> Result<usize> {
        bail!("price history is only kept by the sqlite and postgres storage backends")
    }

    /// Price points over time, oldest first.
    fn price_history(&self, _scryfall_id: &str, _finish: Finish) -> Result<Vec<PricePoint>> {
        bail!("price history is only kept by the sqlite and postgres storage backends")
    }

    /// Printings + finishes the account owns now that were priced on or
    /// before `since` and again later.
    fn price_moves(&self, _since: NaiveDate) -> Result<Vec<PriceMove>> {
        bail!("price history is only kept by the sqlite and postgres storage backends")
    }

    /// How the collection's value moved since `since`; `None` without price
    /// history (Markdown) or when no card was priced back then.
    fn value_change(&self, _since: NaiveDate) -> Result<Option<ValueChange>> {
        Ok(None)
    }

    /// Human-readable location, e.g. a file path.
    fn location(&self) -> String;

    /// The local copy of Scryfall's card data (database stores only).
    fn card_cache(&self) -> Option<&dyn CardCache> {
        None
    }
}

/// The local copy of Scryfall's card data, and the bookkeeping of
/// background jobs that keep it current (see `bulk`).
pub trait CardCache {
    /// The cached printings among `ids`.
    fn cached_cards(&self, ids: &[String]) -> Result<HashMap<String, Card>>;

    /// Insert or replace printings. Returns how many were written.
    fn store_cards(&self, cards: &[Card]) -> Result<usize>;

    /// How many printings are cached.
    fn cached_count(&self) -> Result<i64>;

    /// When a job last finished, and what it noted.
    fn job(&self, name: &str) -> Result<Option<Job>>;

    /// Record that a job finished now.
    fn finish_job(&self, name: &str, detail: &str) -> Result<()>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    pub done_at: DateTime<Utc>,
    pub detail: String,
}

/// Card details for `ids`: from the cache, then Scryfall's API for those
/// not cached yet (which are then cached). Errors only cost details, so they
/// are reported and skipped.
pub(crate) fn card_details(
    cache: Option<&dyn CardCache>,
    scryfall: Option<&Scryfall>,
    ids: &[String],
) -> HashMap<String, Card> {
    let mut cards = match cache.map(|c| c.cached_cards(ids)).transpose() {
        Ok(cards) => cards.unwrap_or_default(),
        Err(e) => {
            eprintln!("  ! could not read the card cache ({e:#})");
            HashMap::new()
        }
    };
    let mut missing: Vec<String> = ids
        .iter()
        .filter(|id| !cards.contains_key(*id))
        .cloned()
        .collect();
    missing.sort();
    missing.dedup();
    if let (false, Some(scryfall)) = (missing.is_empty(), scryfall) {
        match scryfall.cards_by_id(&missing) {
            Ok(fetched) => {
                if let Some(cache) = cache {
                    let new: Vec<Card> = fetched.values().cloned().collect();
                    if let Err(e) = cache.store_cards(&new) {
                        eprintln!("  ! could not cache card details ({e:#})");
                    }
                }
                cards.extend(fetched);
            }
            Err(e) => eprintln!("  ! could not fetch card details from Scryfall ({e:#})"),
        }
    }
    cards
}

/// `['W', 'U']` as stored: `"WU"`.
pub(crate) fn colors_text(colors: &[String]) -> String {
    colors.concat()
}

pub(crate) fn colors_from_text(text: &str) -> Vec<String> {
    text.chars().map(|c| c.to_string()).collect()
}

/// Finishes as stored: `"nonfoil,foil"`.
pub(crate) fn finishes_text(finishes: &[Finish]) -> String {
    finishes
        .iter()
        .map(|f| f.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

pub(crate) fn finishes_from_text(text: &str) -> Vec<Finish> {
    text.split(',').filter_map(|f| f.parse().ok()).collect()
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Totals {
    pub printings: usize,
    pub cards: u32,
    pub tcgplayer: f64,
    /// Euros.
    pub cardmarket: f64,
    pub cardkingdom: f64,
    pub cardkingdom_buylist: f64,
}

pub fn totals(entries: &[CollectionEntry]) -> Totals {
    entries.iter().fold(Totals::default(), |mut t, e| {
        let q = e.quantity as f64;
        t.printings += 1;
        t.cards += e.quantity;
        t.tcgplayer += e.prices.tcgplayer.unwrap_or(0.0) * q;
        t.cardmarket += e.prices.cardmarket.unwrap_or(0.0) * q;
        t.cardkingdom += e.prices.cardkingdom.unwrap_or(0.0) * q;
        t.cardkingdom_buylist += e.prices.cardkingdom_buylist.unwrap_or(0.0) * q;
        t
    })
}

/// What [`copy`] copied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Copied {
    pub accounts: usize,
    pub rows: usize,
    pub price_points: usize,
}

/// Copy the accounts, their collections and the whole price history from
/// `from` into `to` (e.g. SQLite to PostgreSQL). Accounts are matched by
/// name; quantities are set, not added. Sessions, links and API tokens
/// aren't copied (people sign in again), nor is the card cache (it rebuilds
/// itself). A database from before accounts goes to `to`'s first admin.
pub fn copy(from: &mut dyn CollectionStore, to: &mut dyn CollectionStore) -> Result<Copied> {
    let accounts = from.account_rows()?;
    let mut rows = 0;
    if accounts.is_empty() {
        rows += to.import(&from.entries()?)?;
    } else {
        let ids = to.import_accounts(&accounts)?;
        for account in &accounts {
            from.set_user(account.id);
            to.set_user(ids[&account.id]);
            rows += to.import(&from.entries()?)?;
        }
    }
    let price_points = to.import_price_points(&from.all_price_points()?)?;
    Ok(Copied {
        accounts: accounts.len(),
        rows,
        price_points,
    })
}

/// A collection row as the databases store it: no card details.
pub(crate) struct StoredRow {
    pub scryfall_id: String,
    pub finish: Finish,
    pub condition: Condition,
    pub language: String,
    pub quantity: u32,
    pub added: NaiveDate,
    pub prices: Prices,
    pub price_date: Option<NaiveDate>,
}

/// Fill in card details (see [`card_details`]), sorted by name. Rows whose
/// card is unknown get placeholders, so the collection can still be listed.
pub(crate) fn hydrate(
    cache: Option<&dyn CardCache>,
    scryfall: Option<&Scryfall>,
    rows: Vec<StoredRow>,
) -> Vec<CollectionEntry> {
    let ids: Vec<String> = rows.iter().map(|r| r.scryfall_id.clone()).collect();
    let cards = card_details(cache, scryfall, &ids);
    let mut entries: Vec<CollectionEntry> = rows
        .into_iter()
        .map(|r| {
            let card = cards.get(&r.scryfall_id);
            let mut prices = r.prices;
            prices.tcgplayer_url = card
                .and_then(|c| {
                    if r.finish == Finish::Etched {
                        c.tcgplayer_etched_id.or(c.tcgplayer_id)
                    } else {
                        c.tcgplayer_id
                    }
                })
                .map(|id| format!("https://www.tcgplayer.com/product/{id}"));
            CollectionEntry {
                name: card
                    .map(|c| c.name.clone())
                    .unwrap_or_else(|| format!("(unknown card {})", r.scryfall_id)),
                set_code: card.map(|c| c.set_code.to_uppercase()).unwrap_or_default(),
                set_name: card.map(|c| c.set_name.clone()).unwrap_or_default(),
                collector_number: card.map(|c| c.collector_number.clone()).unwrap_or_default(),
                rarity: card.map(|c| c.rarity.clone()).unwrap_or_default(),
                colors: card.map(|c| c.colors.clone()).unwrap_or_default(),
                type_line: card.map(|c| c.type_line.clone()).unwrap_or_default(),
                scryfall_id: r.scryfall_id,
                finish: r.finish,
                condition: r.condition,
                language: r.language,
                quantity: r.quantity,
                prices,
                added: r.added,
                price_updated: r.price_date,
            }
        })
        .collect();
    entries.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.set_code.cmp(&b.set_code))
            .then_with(|| a.collector_number.cmp(&b.collector_number))
            .then_with(|| a.finish.as_str().cmp(b.finish.as_str()))
            .then_with(|| a.condition.cmp(&b.condition))
            .then_with(|| a.language.cmp(&b.language))
    });
    entries
}

/// A row that was just deleted (quantity 0), described from the card data alone.
pub(crate) fn removed_entry(
    cache: Option<&dyn CardCache>,
    scryfall: Option<&Scryfall>,
    key: &RowKey,
) -> CollectionEntry {
    let scryfall_id = key.scryfall_id.as_str();
    let card = card_details(cache, scryfall, &[scryfall_id.to_string()]).remove(scryfall_id);
    CollectionEntry {
        scryfall_id: scryfall_id.to_string(),
        name: card
            .as_ref()
            .map(|c| c.name.clone())
            .unwrap_or_else(|| scryfall_id.to_string()),
        set_code: card
            .as_ref()
            .map(|c| c.set_code.to_uppercase())
            .unwrap_or_default(),
        set_name: card
            .as_ref()
            .map(|c| c.set_name.clone())
            .unwrap_or_default(),
        collector_number: card
            .as_ref()
            .map(|c| c.collector_number.clone())
            .unwrap_or_default(),
        rarity: card.as_ref().map(|c| c.rarity.clone()).unwrap_or_default(),
        colors: card.as_ref().map(|c| c.colors.clone()).unwrap_or_default(),
        type_line: card
            .as_ref()
            .map(|c| c.type_line.clone())
            .unwrap_or_default(),
        finish: key.finish,
        condition: key.condition,
        language: key.language.clone(),
        quantity: 0,
        prices: Prices::default(),
        added: Local::now().date_naive(),
        price_updated: None,
    }
}

/// Behaviour every database store must share. Runs on in-memory SQLite in
/// every `cargo test` and on PostgreSQL in the integration test. Card
/// details aren't checked: they come from Scryfall, which tests don't call.
#[cfg(test)]
pub(crate) fn conformance(store: &mut dyn CollectionStore) {
    let today = Local::now().date_naive();
    let entry = |id: &str, finish, qty, tcg: f64| CollectionEntry {
        scryfall_id: id.into(),
        name: "Lightning Bolt".into(),
        set_code: "X".into(),
        set_name: "X".into(),
        collector_number: "1".into(),
        rarity: "common".into(),
        colors: Vec::new(),
        type_line: String::new(),
        finish,
        condition: Condition::Nm,
        language: "en".into(),
        quantity: qty,
        prices: Prices {
            tcgplayer: Some(tcg),
            cardmarket: Some(tcg - 0.25),
            tix: Some(0.02),
            cardkingdom: Some(tcg + 1.0),
            cardkingdom_buylist: None,
            ..Prices::default()
        },
        added: today,
        price_updated: Some(today),
    };
    let key = |id: &str, finish| RowKey::new(id, finish, "en");
    let owner = store.user().ok();
    // Two printings of Lightning Bolt (M11 #149 and A25 #141) + a foil copy.
    let m11 = "e768c957-3a1f-42f5-853a-96942f645df5";
    let a25 = "e3285e6b-3e79-4d7c-bf96-d920f973b122";
    store.add(entry(m11, Finish::Nonfoil, 2, 1.0)).unwrap();
    let merged = store.add(entry(m11, Finish::Nonfoil, 1, 1.5)).unwrap();
    assert_eq!(merged.quantity, 3);
    assert_eq!(merged.added, today);
    store.add(entry(a25, Finish::Nonfoil, 1, 2.0)).unwrap();
    store.add(entry(a25, Finish::Foil, 1, 5.0)).unwrap();

    let rows = store.entries().unwrap();
    assert_eq!(rows.len(), 3, "one row per printing + finish");
    let m11_row = rows.iter().find(|e| e.scryfall_id == m11).unwrap();
    assert_eq!(m11_row.quantity, 3);
    assert_eq!(
        (m11_row.prices.tcgplayer, m11_row.prices.cardkingdom),
        (Some(1.5), Some(2.5)),
        "same-day price point is replaced"
    );
    assert_eq!(
        (m11_row.prices.cardmarket, m11_row.prices.tix),
        (Some(1.25), Some(0.02))
    );
    let point = &store.price_history(m11, Finish::Nonfoil).unwrap()[0];
    assert_eq!((point.cardmarket, point.tix), (Some(1.25), Some(0.02)));
    assert_eq!(m11_row.price_updated, Some(today));
    assert_eq!(store.price_history(m11, Finish::Nonfoil).unwrap().len(), 1);

    let left = store.remove(&key(a25, Finish::Foil), 5).unwrap().unwrap();
    assert_eq!(left.quantity, 0);
    assert_eq!(store.entries().unwrap().len(), 2);
    // History survives removal.
    assert_eq!(store.price_history(a25, Finish::Foil).unwrap().len(), 1);
    assert!(store.remove(&key(a25, Finish::Foil), 1).unwrap().is_none());
    let partial = store
        .remove(&key(m11, Finish::Nonfoil), 1)
        .unwrap()
        .unwrap();
    assert_eq!(partial.quantity, 2);

    // Edits: set a quantity, change the finish, then merge into a row that exists.
    let set = store
        .set_quantity(&key(m11, Finish::Nonfoil), 5)
        .unwrap()
        .unwrap();
    assert_eq!(set.quantity, 5);
    assert!(
        store
            .set_quantity(&key(a25, Finish::Etched), 2)
            .unwrap()
            .is_none()
    );
    let refinished = store
        .relocate(&key(a25, Finish::Nonfoil), entry(a25, Finish::Foil, 1, 6.0))
        .unwrap()
        .unwrap();
    assert_eq!((refinished.finish, refinished.quantity), (Finish::Foil, 1));
    assert_eq!(
        store
            .price_history(a25, Finish::Foil)
            .unwrap()
            .last()
            .unwrap()
            .tcgplayer,
        Some(6.0),
        "the moved row is priced"
    );
    let merged = store
        .relocate(&key(a25, Finish::Foil), entry(m11, Finish::Nonfoil, 1, 1.5))
        .unwrap()
        .unwrap();
    assert_eq!(merged.quantity, 6);
    assert_eq!(store.entries().unwrap().len(), 1);
    assert!(
        store
            .relocate(&key(a25, Finish::Foil), entry(m11, Finish::Nonfoil, 1, 1.5))
            .unwrap()
            .is_none()
    );

    // Value change: the 6 copies were $1.00 ten days ago and $1.50 now.
    assert!(store.value_change(today).unwrap().is_some());
    let ten_days_ago = today - chrono::Days::new(10);
    assert!(
        store.value_change(ten_days_ago).unwrap().is_none(),
        "nothing was priced back then"
    );
    // Importing sets the quantity (unchanged here) and records its price on its day.
    let old = CollectionEntry {
        price_updated: Some(ten_days_ago),
        added: ten_days_ago,
        ..entry(m11, Finish::Nonfoil, 6, 1.0)
    };
    assert_eq!(store.import(&[old]).unwrap(), 1);
    let change = store
        .value_change(today - chrono::Days::new(7))
        .unwrap()
        .unwrap();
    assert_eq!((change.before, change.now, change.rows), (6.0, 9.0, 1));

    // Price moves: the same row, $1.00 ten days ago and $1.50 today.
    let moves = store.price_moves(today - chrono::Days::new(7)).unwrap();
    assert_eq!(moves.len(), 1);
    let m = &moves[0];
    assert_eq!(
        (m.scryfall_id.as_str(), m.finish, m.quantity),
        (m11, Finish::Nonfoil, 6)
    );
    assert_eq!((m.then.date, m.now.date), (ten_days_ago, today));
    assert_eq!((m.then.tcgplayer, m.now.tcgplayer), (Some(1.0), Some(1.5)));
    assert!(
        store
            .price_moves(ten_days_ago - chrono::Days::new(1))
            .unwrap()
            .is_empty(),
        "nothing was priced before then"
    );

    // A day with no prices at all records nothing.
    store
        .update_prices(m11, Finish::Nonfoil, Prices::default())
        .unwrap();
    assert_eq!(store.price_history(m11, Finish::Nonfoil).unwrap().len(), 2);

    // Condition and language make their own rows of a printing + finish.
    let played_ja = CollectionEntry {
        condition: Condition::Lp,
        language: "ja".into(),
        ..entry(m11, Finish::Nonfoil, 2, 1.5)
    };
    store.add(played_ja.clone()).unwrap();
    let rows = store.entries().unwrap();
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter()
            .any(|r| r.has_key(&played_ja.key()) && r.quantity == 2)
    );
    assert_eq!(
        store
            .value_change(today - chrono::Days::new(7))
            .unwrap()
            .unwrap()
            .rows,
        1,
        "value and moves count a printing + finish once"
    );
    let moves = store.price_moves(today - chrono::Days::new(7)).unwrap();
    assert_eq!((moves.len(), moves[0].quantity), (1, 8));
    let near_mint = store
        .relocate(&played_ja.key(), entry(m11, Finish::Nonfoil, 2, 1.5))
        .unwrap()
        .unwrap();
    assert_eq!(
        (near_mint.condition, near_mint.quantity),
        (Condition::Nm, 8),
        "a new condition merges into that row"
    );
    assert!(store.remove(&played_ja.key(), 1).unwrap().is_none());

    // Accounts have their own collections; prices stay shared.
    if let Some(owner) = owner {
        let ids = store
            .import_accounts(&[AccountRow {
                id: 900,
                name: "Other".into(),
                password_hash: None,
                is_admin: false,
                disabled: false,
                created_at: Utc::now(),
            }])
            .unwrap();
        let other = ids[&900];
        assert_ne!(other, owner);
        store.set_user(other);
        assert!(store.entries().unwrap().is_empty());
        assert!(store.value_change(today).unwrap().is_none());
        store.add(entry(a25, Finish::Nonfoil, 1, 2.0)).unwrap();
        assert!(
            store
                .remove(&key(m11, Finish::Nonfoil), 1)
                .unwrap()
                .is_none()
        );
        assert_eq!(store.entries().unwrap().len(), 1);
        let mut owned = store.owned_printings().unwrap();
        owned.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            owned,
            vec![
                (a25.to_string(), Finish::Nonfoil),
                (m11.to_string(), Finish::Nonfoil)
            ],
            "everyone's printings"
        );
        let names: Vec<String> = store
            .account_rows()
            .unwrap()
            .into_iter()
            .map(|a| a.name)
            .collect();
        assert!(names.contains(&"Other".to_string()));
        // Importing the same name again maps onto the existing account.
        let again = store
            .import_accounts(&[AccountRow {
                id: 7,
                name: "other".into(),
                password_hash: Some("x".into()),
                is_admin: true,
                disabled: false,
                created_at: Utc::now(),
            }])
            .unwrap();
        assert_eq!(again[&7], other);
        store.set_user(owner);
        assert_eq!(store.entries().unwrap().len(), 1);
        store.set_user(other);
        store.remove(&key(a25, Finish::Nonfoil), 1).unwrap();
        store.set_user(owner);
    }

    // Listings take card details from the card cache when it has them.
    if let Some(cache) = store.card_cache() {
        let card = Card {
            scryfall_id: m11.into(),
            oracle_id: Some("4457ed35-7c10-48c8-9776-456485fdf070".into()),
            name: "Lightning Bolt".into(),
            lang: "en".into(),
            set_code: "m11".into(),
            set_name: "Cached Set".into(),
            collector_number: "149".into(),
            rarity: "common".into(),
            colors: vec!["R".into()],
            type_line: "Instant".into(),
            finishes: vec![Finish::Nonfoil, Finish::Foil],
            tcgplayer_id: Some(37301),
            tcgplayer_etched_id: None,
            cardmarket_id: Some(22337),
            image_small: Some("small.jpg".into()),
            image_normal: None,
            scryfall_usd: Some(1.13),
            scryfall_usd_foil: Some(9.1),
            scryfall_usd_etched: None,
            scryfall_eur: Some(0.95),
            scryfall_eur_foil: None,
            scryfall_tix: Some(0.02),
        };
        assert_eq!(cache.store_cards(std::slice::from_ref(&card)).unwrap(), 1);
        assert_eq!(cache.cached_cards(&[m11.into()]).unwrap()[m11], card);
        let row = &store.entries().unwrap()[0];
        assert_eq!(
            (row.set_name.as_str(), row.collector_number.as_str()),
            ("Cached Set", "149")
        );
        assert!(cache.cached_count().unwrap() >= 1);
        cache.finish_job("conformance", "x").unwrap();
        cache.finish_job("conformance", "y").unwrap();
        assert_eq!(cache.job("conformance").unwrap().unwrap().detail, "y");
    }
}
