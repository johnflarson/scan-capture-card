//! SQLite collection store, the default since 1.0: one file, no server, the
//! same tables and price history as PostgreSQL (`migrations/sqlite/`).
//!
//! Several stores can have the file open at once (the scanner, each web
//! request, a CLI command). WAL mode lets readers and the writer work side
//! by side, and every change runs in a `BEGIN IMMEDIATE` transaction so two
//! writers wait for each other instead of failing. Migrations are applied
//! when the file is opened: the app owns the file, so there is no separate
//! admin step as with PostgreSQL.
//!
//! Dates are ISO-8601 text (`YYYY-MM-DD` local dates, UTC timestamps) and
//! money is integer cents.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local, NaiveDate, Utc};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params};

use super::{
    CardCache, CollectionStore, Job, PriceMove, StoredRow, ValueChange, colors_from_text,
    colors_text, finishes_from_text, finishes_text, hydrate, removed_entry,
};
use crate::model::{Card, CollectionEntry, Finish, PricePoint, Prices};
use crate::scryfall::Scryfall;

/// (version, name, sql). Append new migrations; never edit applied ones.
/// Versions match `migrations/postgres/` so both stores stay in step.
const MIGRATIONS: &[(i32, &str, &str)] = &[
    (
        1,
        "init",
        include_str!("../../migrations/sqlite/0001_init.sql"),
    ),
    (
        2,
        "card_cache",
        include_str!("../../migrations/sqlite/0002_card_cache.sql"),
    ),
];

pub fn latest_migration() -> i32 {
    MIGRATIONS.iter().map(|m| m.0).max().unwrap_or(0)
}

pub struct SqliteStore {
    conn: Connection,
    /// Card details for listings; `None` lists rows without them (tests).
    scryfall: Option<Rc<Scryfall>>,
    path: PathBuf,
}

#[derive(Debug)]
pub struct DbStatus {
    pub sqlite_version: String,
    pub schema_version: i32,
    pub printings: i64,
    pub cards: i64,
    pub price_points: i64,
    pub first_price_date: Option<NaiveDate>,
}

impl SqliteStore {
    /// Open (creating it and its folder if needed) and migrate the file.
    pub fn open(path: impl AsRef<Path>, scryfall: Option<Rc<Scryfall>>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("failed to create {}", dir.display()))?;
        }
        let conn = Connection::open(&path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        // WAL needs a local disk (not a network share); it is persistent, so
        // this only does work the first time.
        let mode: String =
            conn.pragma_update_and_check(None, "journal_mode", "wal", |r| r.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            eprintln!(
                "  ! {} can't use WAL mode (journal mode is {mode})",
                path.display()
            );
        }
        Self::setup(conn, path, scryfall)
    }

    /// A private database in memory (tests).
    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        Self::setup(
            Connection::open_in_memory()?,
            PathBuf::from(":memory:"),
            None,
        )
    }

    fn setup(conn: Connection, path: PathBuf, scryfall: Option<Rc<Scryfall>>) -> Result<Self> {
        conn.busy_timeout(Duration::from_secs(10))?;
        conn.execute_batch("pragma synchronous = normal; pragma foreign_keys = on;")?;
        let mut store = Self {
            conn,
            scryfall,
            path,
        };
        store.migrate()?;
        Ok(store)
    }

    /// Apply pending migrations; returns the versions applied. Refuses a
    /// file written by a newer mtg-scan.
    pub fn migrate(&mut self) -> Result<Vec<i32>> {
        let tx = self.write()?;
        tx.execute_batch(
            "create table if not exists schema_migrations (
               version    integer primary key,
               name       text not null,
               applied_at text not null
             );",
        )?;
        let applied: Vec<i32> = tx
            .prepare("select version from schema_migrations")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        if let Some(newer) = applied.iter().copied().find(|v| *v > latest_migration()) {
            bail!(
                "this database is at schema version {newer}, newer than this mtg-scan supports \
                 (version {}): update mtg-scan",
                latest_migration()
            );
        }
        let mut newly = Vec::new();
        for (version, name, sql) in MIGRATIONS {
            if applied.contains(version) {
                continue;
            }
            tx.execute_batch(sql)
                .with_context(|| format!("migration {version} ({name}) failed"))?;
            tx.execute(
                "insert into schema_migrations (version, name, applied_at) values (?1, ?2, ?3)",
                params![version, name, now()],
            )?;
            newly.push(*version);
        }
        tx.commit()?;
        Ok(newly)
    }

    pub fn schema_version(&self) -> Result<i32> {
        Ok(self.conn.query_row(
            "select coalesce(max(version), 0) from schema_migrations",
            [],
            |r| r.get(0),
        )?)
    }

    pub fn status(&self) -> Result<DbStatus> {
        let (printings, cards, price_points, first): (i64, i64, i64, Option<String>) =
            self.conn.query_row(
                "select (select count(*) from collection),
                        (select coalesce(sum(quantity), 0) from collection),
                        (select count(*) from price_history),
                        (select min(price_date) from price_history)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
        Ok(DbStatus {
            sqlite_version: rusqlite::version().to_string(),
            schema_version: self.schema_version()?,
            printings,
            cards,
            price_points,
            first_price_date: first.as_deref().map(parse_date).transpose()?,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A write transaction that takes the write lock up front, so it waits
    /// for (rather than fails against) another writer.
    fn write(&mut self) -> Result<Transaction<'_>> {
        Ok(self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?)
    }

    /// [`Self::write`] for methods that only have `&self` (the card cache):
    /// runs `f` in a `BEGIN IMMEDIATE` transaction, rolled back on error.
    fn write_with<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        self.conn.execute_batch("begin immediate")?;
        match f(&self.conn) {
            Ok(value) => {
                self.conn.execute_batch("commit")?;
                Ok(value)
            }
            Err(e) => {
                let _ = self.conn.execute_batch("rollback");
                Err(e)
            }
        }
    }

    fn stored_rows(&self, filter: Option<(&str, Finish)>) -> Result<Vec<StoredRow>> {
        const SELECT: &str = "select c.scryfall_id, c.finish, c.quantity, c.added_at,
                p.price_tcg, p.price_ck, p.price_ck_buylist, p.price_date
             from collection c
             left join price_history p
               on p.scryfall_id = c.scryfall_id and p.finish = c.finish
              and p.price_date = (select max(q.price_date) from price_history q
                                  where q.scryfall_id = c.scryfall_id and q.finish = c.finish)";
        let mut stmt;
        let rows = match filter {
            Some((id, finish)) => {
                stmt = self.conn.prepare(&format!(
                    "{SELECT} where c.scryfall_id = ?1 and c.finish = ?2"
                ))?;
                stmt.query_map(params![id, finish.as_str()], raw_row)?
            }
            None => {
                stmt = self.conn.prepare(SELECT)?;
                stmt.query_map([], raw_row)?
            }
        };
        rows.map(|r| stored_row(r?)).collect()
    }

    fn one(&self, scryfall_id: &str, finish: Finish) -> Result<Option<CollectionEntry>> {
        let rows = self.stored_rows(Some((scryfall_id, finish)))?;
        Ok(hydrate(Some(self), self.scryfall.as_deref(), rows)
            .into_iter()
            .next())
    }
}

impl CollectionStore for SqliteStore {
    fn entries(&self) -> Result<Vec<CollectionEntry>> {
        Ok(hydrate(
            Some(self),
            self.scryfall.as_deref(),
            self.stored_rows(None)?,
        ))
    }

    fn add(&mut self, entry: CollectionEntry) -> Result<CollectionEntry> {
        let today = Local::now().date_naive();
        let tx = self.write()?;
        let (quantity, added): (u32, String) = tx.query_row(
            "insert into collection (scryfall_id, finish, quantity, added_at, updated_at)
             values (?1, ?2, ?3, ?4, ?5)
             on conflict (scryfall_id, finish)
             do update set quantity = collection.quantity + excluded.quantity,
                           updated_at = excluded.updated_at
             returning quantity, added_at",
            params![
                entry.scryfall_id,
                entry.finish.as_str(),
                entry.quantity,
                date_text(today),
                now()
            ],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let date = entry.price_updated.unwrap_or(today);
        record_price(&tx, &entry.scryfall_id, entry.finish, date, &entry.prices)?;
        tx.commit()?;
        Ok(CollectionEntry {
            quantity,
            added: parse_date(&added)?,
            ..entry
        })
    }

    fn remove(
        &mut self,
        scryfall_id: &str,
        finish: Finish,
        quantity: u32,
    ) -> Result<Option<CollectionEntry>> {
        let tx = self.write()?;
        let Some(have) = tx
            .query_row(
                "select quantity from collection where scryfall_id = ?1 and finish = ?2",
                params![scryfall_id, finish.as_str()],
                |r| r.get::<_, u32>(0),
            )
            .optional()?
        else {
            return Ok(None);
        };
        let left = have.saturating_sub(quantity);
        if left == 0 {
            tx.execute(
                "delete from collection where scryfall_id = ?1 and finish = ?2",
                params![scryfall_id, finish.as_str()],
            )?;
        } else {
            tx.execute(
                "update collection set quantity = ?3, updated_at = ?4
                 where scryfall_id = ?1 and finish = ?2",
                params![scryfall_id, finish.as_str(), left, now()],
            )?;
        }
        tx.commit()?;
        if left > 0 {
            return self.one(scryfall_id, finish);
        }
        // Row is gone; describe it from Scryfall alone.
        Ok(Some(removed_entry(
            Some(self),
            self.scryfall.as_deref(),
            scryfall_id,
            finish,
        )))
    }

    fn set_quantity(
        &mut self,
        scryfall_id: &str,
        finish: Finish,
        quantity: u32,
    ) -> Result<Option<CollectionEntry>> {
        if quantity == 0 {
            return self.remove(scryfall_id, finish, u32::MAX);
        }
        let updated = self.conn.execute(
            "update collection set quantity = ?3, updated_at = ?4
             where scryfall_id = ?1 and finish = ?2",
            params![scryfall_id, finish.as_str(), quantity, now()],
        )?;
        if updated == 0 {
            return Ok(None);
        }
        self.one(scryfall_id, finish)
    }

    fn relocate(
        &mut self,
        scryfall_id: &str,
        finish: Finish,
        to: CollectionEntry,
    ) -> Result<Option<CollectionEntry>> {
        if to.quantity == 0 {
            bail!("a moved row needs at least one copy");
        }
        let tx = self.write()?;
        let Some(first_added) = tx
            .query_row(
                "delete from collection where scryfall_id = ?1 and finish = ?2
                 returning added_at",
                params![scryfall_id, finish.as_str()],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        else {
            return Ok(None);
        };
        // Merge into the target row if it exists, keeping the earliest add date.
        let (quantity, added): (u32, String) = tx.query_row(
            "insert into collection (scryfall_id, finish, quantity, added_at, updated_at)
             values (?1, ?2, ?3, ?4, ?5)
             on conflict (scryfall_id, finish)
             do update set quantity = collection.quantity + excluded.quantity,
                           added_at = min(collection.added_at, excluded.added_at),
                           updated_at = excluded.updated_at
             returning quantity, added_at",
            params![
                to.scryfall_id,
                to.finish.as_str(),
                to.quantity,
                first_added,
                now()
            ],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let date = to
            .price_updated
            .unwrap_or_else(|| Local::now().date_naive());
        record_price(&tx, &to.scryfall_id, to.finish, date, &to.prices)?;
        tx.commit()?;
        Ok(Some(CollectionEntry {
            quantity,
            added: parse_date(&added)?,
            ..to
        }))
    }

    fn update_prices(&mut self, scryfall_id: &str, finish: Finish, prices: Prices) -> Result<()> {
        let tx = self.write()?;
        record_price(&tx, scryfall_id, finish, Local::now().date_naive(), &prices)?;
        tx.commit()?;
        Ok(())
    }

    fn import(&mut self, entries: &[CollectionEntry]) -> Result<usize> {
        let tx = self.write()?;
        for e in entries {
            tx.execute(
                "insert into collection (scryfall_id, finish, quantity, added_at, updated_at)
                 values (?1, ?2, ?3, ?4, ?5)
                 on conflict (scryfall_id, finish)
                 do update set quantity = excluded.quantity, updated_at = excluded.updated_at",
                params![
                    e.scryfall_id,
                    e.finish.as_str(),
                    e.quantity,
                    date_text(e.added),
                    now()
                ],
            )
            .with_context(|| format!("failed to import {} ({})", e.name, e.scryfall_id))?;
            let date = e.price_updated.unwrap_or(e.added);
            record_price(&tx, &e.scryfall_id, e.finish, date, &e.prices)?;
        }
        tx.commit()?;
        Ok(entries.len())
    }

    fn price_history(&self, scryfall_id: &str, finish: Finish) -> Result<Vec<PricePoint>> {
        let mut stmt = self.conn.prepare(
            "select price_date, price_tcg, price_ck, price_ck_buylist
             from price_history
             where scryfall_id = ?1 and finish = ?2
             order by price_date",
        )?;
        let rows = stmt.query_map(params![scryfall_id, finish.as_str()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<i64>>(1)?,
                r.get::<_, Option<i64>>(2)?,
                r.get::<_, Option<i64>>(3)?,
            ))
        })?;
        rows.map(|r| {
            let (date, tcg, ck, ckb) = r?;
            Ok(PricePoint {
                date: parse_date(&date)?,
                tcgplayer: dollars(tcg),
                cardkingdom: dollars(ck),
                cardkingdom_buylist: dollars(ckb),
            })
        })
        .collect()
    }

    fn price_moves(&self, since: NaiveDate) -> Result<Vec<PriceMove>> {
        let mut stmt = self.conn.prepare(
            "select c.scryfall_id, c.finish, c.quantity,
                    old.price_date, old.price_tcg, old.price_ck, old.price_ck_buylist,
                    cur.price_date, cur.price_tcg, cur.price_ck, cur.price_ck_buylist
             from collection c
             join price_history old
               on old.scryfall_id = c.scryfall_id and old.finish = c.finish
              and old.price_date = (select max(q.price_date) from price_history q
                                    where q.scryfall_id = c.scryfall_id and q.finish = c.finish
                                      and q.price_date <= ?1)
             join price_history cur
               on cur.scryfall_id = c.scryfall_id and cur.finish = c.finish
              and cur.price_date = (select max(q.price_date) from price_history q
                                    where q.scryfall_id = c.scryfall_id and q.finish = c.finish)
             where cur.price_date > old.price_date",
        )?;
        let rows = stmt.query_map(params![date_text(since)], |r| {
            let point = |at: usize| -> rusqlite::Result<(String, [Option<i64>; 3])> {
                Ok((r.get(at)?, [r.get(at + 1)?, r.get(at + 2)?, r.get(at + 3)?]))
            };
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, u32>(2)?,
                point(3)?,
                point(7)?,
            ))
        })?;
        rows.map(|r| {
            let (scryfall_id, finish, quantity, then, now) = r?;
            let point = |(date, [tcg, ck, ckb]): (String, [Option<i64>; 3])| -> Result<_> {
                Ok(PricePoint {
                    date: parse_date(&date)?,
                    tcgplayer: dollars(tcg),
                    cardkingdom: dollars(ck),
                    cardkingdom_buylist: dollars(ckb),
                })
            };
            Ok(PriceMove {
                scryfall_id,
                finish: finish.parse()?,
                quantity,
                then: point(then)?,
                now: point(now)?,
            })
        })
        .collect()
    }

    fn value_change(&self, since: NaiveDate) -> Result<Option<ValueChange>> {
        let (before, now, rows): (i64, i64, u32) = self.conn.query_row(
            "select coalesce(sum(c.quantity * old.price_tcg), 0),
                    coalesce(sum(c.quantity * cur.price_tcg), 0),
                    count(*)
             from collection c
             join price_history old
               on old.scryfall_id = c.scryfall_id and old.finish = c.finish
              and old.price_date = (select max(q.price_date) from price_history q
                                    where q.scryfall_id = c.scryfall_id and q.finish = c.finish
                                      and q.price_date <= ?1 and q.price_tcg is not null)
             join price_history cur
               on cur.scryfall_id = c.scryfall_id and cur.finish = c.finish
              and cur.price_date = (select max(q.price_date) from price_history q
                                    where q.scryfall_id = c.scryfall_id and q.finish = c.finish
                                      and q.price_tcg is not null)",
            params![date_text(since)],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        Ok((rows > 0).then(|| ValueChange {
            before: before as f64 / 100.0,
            now: now as f64 / 100.0,
            rows,
        }))
    }

    fn location(&self) -> String {
        std::path::absolute(&self.path)
            .unwrap_or_else(|_| self.path.clone())
            .display()
            .to_string()
    }

    fn card_cache(&self) -> Option<&dyn CardCache> {
        Some(self)
    }
}

/// `card_printing` columns in the order [`card_from_row`] reads them.
const CARD_COLUMNS: &str = "scryfall_id, oracle_id, name, lang, set_code, set_name,
    collector_number, rarity, type_line, colors, finishes, tcgplayer_id, tcgplayer_etched_id,
    cardmarket_id, image_small, image_normal, usd, usd_foil, usd_etched, eur, eur_foil, tix";

impl CardCache for SqliteStore {
    fn cached_cards(&self, ids: &[String]) -> Result<HashMap<String, Card>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let mut stmt = self.conn.prepare(&format!(
            "select {CARD_COLUMNS} from card_printing
             where scryfall_id in (select value from json_each(?1))"
        ))?;
        let rows = stmt.query_map(params![serde_json::to_string(ids)?], card_from_row)?;
        rows.map(|r| {
            let card = r?;
            Ok((card.scryfall_id.clone(), card))
        })
        .collect()
    }

    fn store_cards(&self, cards: &[Card]) -> Result<usize> {
        if cards.is_empty() {
            return Ok(0);
        }
        self.write_with(|conn| {
            let mut stmt = conn.prepare(&format!(
                "insert into card_printing ({CARD_COLUMNS}, synced_at)
                 values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                         ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23)
                 on conflict (scryfall_id) do update set
                   oracle_id = excluded.oracle_id, name = excluded.name, lang = excluded.lang,
                   set_code = excluded.set_code, set_name = excluded.set_name,
                   collector_number = excluded.collector_number, rarity = excluded.rarity,
                   type_line = excluded.type_line, colors = excluded.colors,
                   finishes = excluded.finishes, tcgplayer_id = excluded.tcgplayer_id,
                   tcgplayer_etched_id = excluded.tcgplayer_etched_id,
                   cardmarket_id = excluded.cardmarket_id, image_small = excluded.image_small,
                   image_normal = excluded.image_normal, usd = excluded.usd,
                   usd_foil = excluded.usd_foil, usd_etched = excluded.usd_etched,
                   eur = excluded.eur, eur_foil = excluded.eur_foil, tix = excluded.tix,
                   synced_at = excluded.synced_at"
            ))?;
            let synced = now();
            for c in cards {
                stmt.execute(params![
                    c.scryfall_id,
                    c.oracle_id,
                    c.name,
                    c.lang,
                    c.set_code,
                    c.set_name,
                    c.collector_number,
                    c.rarity,
                    c.type_line,
                    colors_text(&c.colors),
                    finishes_text(&c.finishes),
                    c.tcgplayer_id.map(|v| v as i64),
                    c.tcgplayer_etched_id.map(|v| v as i64),
                    c.cardmarket_id.map(|v| v as i64),
                    c.image_small,
                    c.image_normal,
                    cents(c.scryfall_usd),
                    cents(c.scryfall_usd_foil),
                    cents(c.scryfall_usd_etched),
                    cents(c.scryfall_eur),
                    cents(c.scryfall_eur_foil),
                    cents(c.scryfall_tix),
                    synced,
                ])?;
            }
            Ok(cards.len())
        })
    }

    fn cached_count(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("select count(*) from card_printing", [], |r| r.get(0))?)
    }

    fn job(&self, name: &str) -> Result<Option<Job>> {
        let row: Option<(String, String)> = self
            .conn
            .query_row(
                "select done_at, detail from app_job where name = ?1",
                params![name],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        row.map(|(done_at, detail)| {
            Ok(Job {
                done_at: DateTime::parse_from_rfc3339(&done_at)
                    .with_context(|| format!("bad timestamp {done_at:?}"))?
                    .with_timezone(&Utc),
                detail,
            })
        })
        .transpose()
    }

    fn finish_job(&self, name: &str, detail: &str) -> Result<()> {
        self.conn.execute(
            "insert into app_job (name, done_at, detail) values (?1, ?2, ?3)
             on conflict (name) do update set done_at = excluded.done_at, detail = excluded.detail",
            params![name, now(), detail],
        )?;
        Ok(())
    }
}

fn card_from_row(r: &Row<'_>) -> rusqlite::Result<Card> {
    let id = |at: usize| -> rusqlite::Result<Option<u64>> {
        Ok(r.get::<_, Option<i64>>(at)?.map(|v| v as u64))
    };
    let price = |at: usize| -> rusqlite::Result<Option<f64>> { Ok(dollars(r.get(at)?)) };
    Ok(Card {
        scryfall_id: r.get(0)?,
        oracle_id: r.get(1)?,
        name: r.get(2)?,
        lang: r.get(3)?,
        set_code: r.get(4)?,
        set_name: r.get(5)?,
        collector_number: r.get(6)?,
        rarity: r.get(7)?,
        type_line: r.get(8)?,
        colors: colors_from_text(&r.get::<_, String>(9)?),
        finishes: finishes_from_text(&r.get::<_, String>(10)?),
        tcgplayer_id: id(11)?,
        tcgplayer_etched_id: id(12)?,
        cardmarket_id: id(13)?,
        image_small: r.get(14)?,
        image_normal: r.get(15)?,
        scryfall_usd: price(16)?,
        scryfall_usd_foil: price(17)?,
        scryfall_usd_etched: price(18)?,
        scryfall_eur: price(19)?,
        scryfall_eur_foil: price(20)?,
        scryfall_tix: price(21)?,
    })
}

/// Upsert `date`'s price point. Skipped when no price is known.
fn record_price(
    tx: &Transaction<'_>,
    scryfall_id: &str,
    finish: Finish,
    date: NaiveDate,
    prices: &Prices,
) -> Result<()> {
    if prices.tcgplayer.is_none()
        && prices.cardkingdom.is_none()
        && prices.cardkingdom_buylist.is_none()
    {
        return Ok(());
    }
    tx.execute(
        "insert into price_history
           (scryfall_id, finish, price_date, price_tcg, price_ck, price_ck_buylist, captured_at)
         values (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         on conflict (scryfall_id, finish, price_date)
         do update set price_tcg = excluded.price_tcg,
                       price_ck = excluded.price_ck,
                       price_ck_buylist = excluded.price_ck_buylist,
                       captured_at = excluded.captured_at",
        params![
            scryfall_id,
            finish.as_str(),
            date_text(date),
            cents(prices.tcgplayer),
            cents(prices.cardkingdom),
            cents(prices.cardkingdom_buylist),
            now()
        ],
    )?;
    Ok(())
}

type RawRow = (
    String,
    String,
    u32,
    String,
    [Option<i64>; 3],
    Option<String>,
);

fn raw_row(r: &Row<'_>) -> rusqlite::Result<RawRow> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        [r.get(4)?, r.get(5)?, r.get(6)?],
        r.get(7)?,
    ))
}

fn stored_row(
    (scryfall_id, finish, quantity, added, [tcg, ck, ckb], price_date): RawRow,
) -> Result<StoredRow> {
    Ok(StoredRow {
        scryfall_id,
        finish: finish.parse()?,
        quantity,
        added: parse_date(&added)?,
        prices: Prices {
            tcgplayer: dollars(tcg),
            cardkingdom: dollars(ck),
            cardkingdom_buylist: dollars(ckb),
            tcgplayer_url: None,
            cardkingdom_url: None,
        },
        price_date: price_date.as_deref().map(parse_date).transpose()?,
    })
}

fn cents(price: Option<f64>) -> Option<i64> {
    price.map(|p| (p * 100.0).round() as i64)
}

fn dollars(cents: Option<i64>) -> Option<f64> {
    cents.map(|c| c as f64 / 100.0)
}

fn date_text(date: NaiveDate) -> String {
    date.format("%Y-%m-%d").to_string()
}

fn parse_date(text: &str) -> Result<NaiveDate> {
    NaiveDate::parse_from_str(text, "%Y-%m-%d").with_context(|| format!("bad date '{text}'"))
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn behaves_like_every_store() {
        let mut store = SqliteStore::open_in_memory().unwrap();
        assert_eq!(store.schema_version().unwrap(), latest_migration());
        crate::store::conformance(&mut store);
        let status = store.status().unwrap();
        assert_eq!((status.printings, status.cards), (1, 6));
        assert_eq!(status.price_points, 4);
    }

    #[test]
    fn a_file_is_created_migrated_once_and_shared() {
        let dir = std::env::temp_dir().join(format!("mtg-scan-sqlite-{}", std::process::id()));
        let path = dir.join("nested").join("collection.db");
        let mut first = SqliteStore::open(&path, None).unwrap();
        assert!(path.exists());
        let id = "e768c957-3a1f-42f5-853a-96942f645df5";
        let entry = CollectionEntry {
            scryfall_id: id.into(),
            name: "Lightning Bolt".into(),
            set_code: "M11".into(),
            set_name: String::new(),
            collector_number: "149".into(),
            rarity: "common".into(),
            colors: Vec::new(),
            type_line: String::new(),
            finish: Finish::Foil,
            quantity: 2,
            prices: Prices {
                tcgplayer: Some(0.105),
                ..Prices::default()
            },
            added: Local::now().date_naive(),
            price_updated: None,
        };
        first.add(entry).unwrap();
        // A second connection (another request or process) sees the change
        // and doesn't migrate again.
        let mut second = SqliteStore::open(&path, None).unwrap();
        assert!(second.migrate().unwrap().is_empty());
        let rows = second.entries().unwrap();
        assert_eq!((rows.len(), rows[0].quantity), (1, 2));
        assert_eq!(
            rows[0].prices.tcgplayer,
            Some(0.11),
            "money is stored in cents"
        );
        drop((first, second));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn listings_take_card_details_from_the_cache() {
        let mut store = SqliteStore::open_in_memory().unwrap();
        let id = "e768c957-3a1f-42f5-853a-96942f645df5";
        let entry = CollectionEntry {
            scryfall_id: id.into(),
            name: String::new(),
            set_code: String::new(),
            set_name: String::new(),
            collector_number: String::new(),
            rarity: String::new(),
            colors: Vec::new(),
            type_line: String::new(),
            finish: Finish::Nonfoil,
            quantity: 1,
            prices: Prices::default(),
            added: Local::now().date_naive(),
            price_updated: None,
        };
        store.add(entry).unwrap();
        // Not cached and no Scryfall client: a placeholder name.
        assert!(store.entries().unwrap()[0].name.contains("unknown card"));
        let card = Card {
            scryfall_id: id.into(),
            oracle_id: None,
            name: "Lightning Bolt".into(),
            lang: "en".into(),
            set_code: "m11".into(),
            set_name: "Magic 2011".into(),
            collector_number: "149".into(),
            rarity: "common".into(),
            colors: vec!["R".into()],
            type_line: "Instant".into(),
            finishes: vec![Finish::Nonfoil, Finish::Foil],
            tcgplayer_id: Some(37301),
            tcgplayer_etched_id: None,
            cardmarket_id: None,
            image_small: None,
            image_normal: None,
            scryfall_usd: Some(1.13),
            scryfall_usd_foil: None,
            scryfall_usd_etched: None,
            scryfall_eur: Some(0.95),
            scryfall_eur_foil: None,
            scryfall_tix: None,
        };
        store.store_cards(&[card]).unwrap();
        let row = &store.entries().unwrap()[0];
        assert_eq!(
            (
                row.name.as_str(),
                row.set_code.as_str(),
                row.collector_number.as_str()
            ),
            ("Lightning Bolt", "M11", "149")
        );
        assert_eq!(row.colors, vec!["R"]);
        assert_eq!(
            row.prices.tcgplayer_url.as_deref(),
            Some("https://www.tcgplayer.com/product/37301")
        );
    }

    #[test]
    fn a_newer_schema_is_refused() {
        let mut store = SqliteStore::open_in_memory().unwrap();
        store
            .conn
            .execute(
                "insert into schema_migrations (version, name, applied_at) values (999, 'x', '')",
                [],
            )
            .unwrap();
        let err = store.migrate().unwrap_err().to_string();
        assert!(err.contains("update mtg-scan"), "{err}");
    }

    #[test]
    fn migrations_are_ordered_and_unique() {
        let versions: Vec<i32> = MIGRATIONS.iter().map(|m| m.0).collect();
        let mut sorted = versions.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(versions, sorted);
        assert_eq!(
            latest_migration(),
            crate::store::postgres::latest_migration(),
            "SQLite and PostgreSQL schemas move in step"
        );
    }
}
