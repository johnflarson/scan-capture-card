//! PostgreSQL collection store (iteration 2; optional since 1.0, where
//! SQLite is the default).
//!
//! Only what isn't available online is stored: Scryfall ID, finish,
//! quantity and dated prices. Names, sets, numbers and rarities are fetched
//! from Scryfall in batches whenever rows are listed. Schema:
//! `migrations/*.sql`, applied by `mtg-scan db migrate` with the admin
//! (schema owner) account; the app itself runs as a least-privilege role.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local, NaiveDate, Utc};
use postgres::types::ToSql;
use postgres::{Client, GenericClient, Row};
use postgres_native_tls::MakeTlsConnector;

use super::{
    CardCache, CollectionStore, Job, PriceMove, StoredRow, ValueChange, colors_from_text,
    colors_text, finishes_from_text, finishes_text, hydrate, removed_entry,
};
use crate::model::{Card, CollectionEntry, Finish, PricePoint, Prices};
use crate::scryfall::Scryfall;

pub fn latest_migration() -> i32 {
    MIGRATIONS.iter().map(|m| m.0).max().unwrap_or(0)
}

/// (version, name, sql). Append new migrations; never edit applied ones.
const MIGRATIONS: &[(i32, &str, &str)] = &[
    (
        1,
        "init",
        include_str!("../../migrations/postgres/0001_init.sql"),
    ),
    (
        2,
        "card_cache",
        include_str!("../../migrations/postgres/0002_card_cache.sql"),
    ),
    (
        3,
        "price_sources",
        include_str!("../../migrations/postgres/0003_price_sources.sql"),
    ),
];

pub struct PostgresStore {
    client: RefCell<Client>,
    scryfall: Rc<Scryfall>,
    schema: String,
    location: String,
    /// The schema version when opened. [`Self::open`] also opens older
    /// schemas (e.g. a 0.x database, to copy it without upgrading it); reads
    /// then leave out what that version doesn't have.
    version: i32,
}

/// Result of [`PostgresStore::setup_app_role`].
pub enum RoleSetup {
    /// Role exists (created now or before) and has been granted access.
    Ready { created: bool },
    /// The admin account may not create roles; a superuser must run `sql`
    /// (it contains only a SCRAM verifier, never the password).
    NeedsSuperuser { sql: String },
}

#[derive(Debug)]
pub struct DbStatus {
    pub server_version: String,
    pub user: String,
    /// Can create roles (needed by `db setup-app-user`).
    pub can_create_roles: bool,
    pub schema_version: i32,
    pub printings: i64,
    pub cards: i64,
    pub price_points: i64,
    pub first_price_date: Option<NaiveDate>,
}

impl PostgresStore {
    /// Day-to-day connection (normally the least-privilege app account).
    /// Never changes the schema: fails if migrations are pending so they are
    /// applied deliberately with `mtg-scan db migrate`.
    pub fn connect(config: postgres::Config, schema: &str, scryfall: Rc<Scryfall>) -> Result<Self> {
        let store = Self::open(config, schema, scryfall)?;
        let version = store.schema_version()?;
        let latest = latest_migration();
        if version < latest {
            bail!(
                "the database schema is at version {version} but this mtg-scan needs version \
                 {latest}: run `mtg-scan db migrate`"
            );
        }
        if version > latest {
            bail!(
                "the database schema (version {version}) is newer than this mtg-scan supports \
                 (version {latest}): update mtg-scan"
            );
        }
        Ok(store)
    }

    /// Admin connection (the schema owner): creates the schema and applies
    /// pending migrations. Returns the store and the versions applied.
    pub fn connect_admin(
        config: postgres::Config,
        schema: &str,
        scryfall: Rc<Scryfall>,
    ) -> Result<(Self, Vec<i32>)> {
        let store = Self::open(config, schema, scryfall)?;
        store
            .client
            .borrow_mut()
            .batch_execute(&format!("create schema if not exists {schema}"))
            .context("could not create the schema (is this the schema owner account?)")?;
        let applied = store.migrate()?;
        Ok((store, applied))
    }

    /// Connect and select the schema without any version checks.
    pub fn open(config: postgres::Config, schema: &str, scryfall: Rc<Scryfall>) -> Result<Self> {
        validate_identifier(schema)?;
        let location = describe(&config, schema);
        let tls =
            MakeTlsConnector::new(native_tls::TlsConnector::new().context("TLS setup failed")?);
        let mut client = config
            .connect(tls)
            .with_context(|| format!("failed to connect to {location}"))?;
        client.batch_execute(&format!("set search_path to {schema}"))?;
        let store = Self {
            client: RefCell::new(client),
            scryfall,
            schema: schema.to_string(),
            location,
            version: 0,
        };
        let version = store.schema_version()?;
        Ok(Self { version, ..store })
    }

    /// The Cardmarket and tix price columns (migration 3), or nulls in an
    /// older schema. `prefix` is a table alias with its dot, or empty.
    fn newer_prices(&self, prefix: &str) -> String {
        if self.version >= 3 {
            format!("{prefix}price_cardmarket::float8, {prefix}price_tix::float8")
        } else {
            "null::float8, null::float8".into()
        }
    }

    /// Highest applied migration, or 0 for an empty schema.
    pub fn schema_version(&self) -> Result<i32> {
        let mut client = self.client.borrow_mut();
        let exists: bool = client
            .query_one("select to_regclass('schema_migrations') is not null", &[])?
            .get(0);
        if !exists {
            return Ok(0);
        }
        Ok(client
            .query_one(
                "select coalesce(max(version), 0) from schema_migrations",
                &[],
            )?
            .get(0))
    }

    /// Create (if allowed) a login role with only row-level access to this
    /// schema: it can read and write cards and prices but not change tables.
    /// Default privileges cover tables added by future migrations.
    pub fn setup_app_role(&self, role: &str, password: &str) -> Result<RoleSetup> {
        validate_identifier(role)?;
        let schema = &self.schema;
        let mut client = self.client.borrow_mut();
        let row = client.query_one(
            "select exists(select 1 from pg_roles where rolname = $1),
                    (select rolsuper or rolcreaterole from pg_roles where rolname = current_user),
                    quote_ident(current_database())",
            &[&role],
        )?;
        let (exists, can_create, database): (bool, bool, String) =
            (row.get(0), row.get(1), row.get(2));
        // Send a SCRAM verifier rather than the password, so the password
        // never appears in server logs or in the SQL shown to a superuser.
        let verifier = postgres_protocol::password::scram_sha_256(password.as_bytes());
        let create = format!(
            "create role {role} login password '{}';",
            verifier.replace('\'', "''")
        );
        let mut created = false;
        if !exists {
            if !can_create {
                return Ok(RoleSetup::NeedsSuperuser { sql: create });
            }
            client.batch_execute(&create)?;
            created = true;
        }
        client
            .batch_execute(&format!(
                "grant connect on database {database} to {role};
                 grant usage on schema {schema} to {role};
                 grant select, insert, update, delete on all tables in schema {schema} to {role};
                 grant usage, select on all sequences in schema {schema} to {role};
                 revoke insert, update, delete on {schema}.schema_migrations from {role};
                 alter default privileges in schema {schema}
                   grant select, insert, update, delete on tables to {role};
                 alter default privileges in schema {schema}
                   grant usage, select on sequences to {role};"
            ))
            .context("granting privileges failed (run this with the schema owner account)")?;
        Ok(RoleSetup::Ready { created })
    }

    /// Apply pending migrations; returns the versions applied.
    pub fn migrate(&self) -> Result<Vec<i32>> {
        let mut client = self.client.borrow_mut();
        let mut tx = client.transaction()?;
        tx.batch_execute(
            "create table if not exists schema_migrations (
               version    integer primary key,
               name       text not null,
               applied_at timestamptz not null default now()
             );
             lock table schema_migrations in exclusive mode;",
        )?;
        let applied: Vec<i32> = tx
            .query("select version from schema_migrations", &[])?
            .iter()
            .map(|r| r.get(0))
            .collect();
        let mut newly = Vec::new();
        for (version, name, sql) in MIGRATIONS {
            if applied.contains(version) {
                continue;
            }
            tx.batch_execute(sql)
                .with_context(|| format!("migration {version} ({name}) failed"))?;
            tx.execute(
                "insert into schema_migrations (version, name) values ($1, $2)",
                &[version, name],
            )?;
            newly.push(*version);
        }
        tx.commit()?;
        Ok(newly)
    }

    pub fn status(&self) -> Result<DbStatus> {
        let mut client = self.client.borrow_mut();
        let row = client.query_one(
            "select
               current_setting('server_version'),
               current_user::text,
               (select rolsuper or rolcreaterole from pg_roles where rolname = current_user),
               (select coalesce(max(version), 0) from schema_migrations),
               (select count(*) from collection),
               (select coalesce(sum(quantity), 0)::bigint from collection),
               (select count(*) from price_history),
               (select min(price_date) from price_history)",
            &[],
        )?;
        Ok(DbStatus {
            server_version: row.get(0),
            user: row.get(1),
            can_create_roles: row.get(2),
            schema_version: row.get(3),
            printings: row.get(4),
            cards: row.get(5),
            price_points: row.get(6),
            first_price_date: row.get(7),
        })
    }

    fn stored_rows(&self, filter: Option<(&str, Finish)>) -> Result<Vec<StoredRow>> {
        let select = format!(
            "select scryfall_id::text, finish, quantity, added_at, price_tcg::float8, {},
                    price_ck::float8, price_ck_buylist::float8, price_date
             from collection_value",
            self.newer_prices("")
        );
        let mut client = self.client.borrow_mut();
        let rows = match filter {
            Some((id, finish)) => client.query(
                &format!("{select} where scryfall_id = $1::text::uuid and finish = $2"),
                &[&id, &finish.as_str()],
            )?,
            None => client.query(&select, &[])?,
        };
        rows.iter().map(stored_row).collect()
    }
}

impl CollectionStore for PostgresStore {
    fn entries(&self) -> Result<Vec<CollectionEntry>> {
        Ok(hydrate(
            Some(self),
            Some(&self.scryfall),
            self.stored_rows(None)?,
        ))
    }

    fn add(&mut self, entry: CollectionEntry) -> Result<CollectionEntry> {
        let mut client = self.client.borrow_mut();
        let mut tx = client.transaction()?;
        let row = tx.query_one(
            "insert into collection (scryfall_id, finish, quantity)
             values ($1::text::uuid, $2, $3)
             on conflict (scryfall_id, finish)
             do update set quantity = collection.quantity + excluded.quantity, updated_at = now()
             returning quantity, added_at",
            &[
                &entry.scryfall_id,
                &entry.finish.as_str(),
                &(entry.quantity as i32),
            ],
        )?;
        let date = entry
            .price_updated
            .unwrap_or_else(|| Local::now().date_naive());
        record_price(
            &mut tx,
            &entry.scryfall_id,
            entry.finish,
            date,
            &entry.prices,
        )?;
        tx.commit()?;
        let quantity: i32 = row.get(0);
        let added: DateTime<Utc> = row.get(1);
        Ok(CollectionEntry {
            quantity: quantity as u32,
            added: added.with_timezone(&Local).date_naive(),
            ..entry
        })
    }

    fn remove(
        &mut self,
        scryfall_id: &str,
        finish: Finish,
        quantity: u32,
    ) -> Result<Option<CollectionEntry>> {
        let remaining = {
            let mut client = self.client.borrow_mut();
            let mut tx = client.transaction()?;
            let Some(row) = tx.query_opt(
                "select quantity from collection
                 where scryfall_id = $1::text::uuid and finish = $2
                 for update",
                &[&scryfall_id, &finish.as_str()],
            )?
            else {
                return Ok(None);
            };
            let have: i32 = row.get(0);
            let left = have.saturating_sub(quantity as i32).max(0);
            if left == 0 {
                tx.execute(
                    "delete from collection where scryfall_id = $1::text::uuid and finish = $2",
                    &[&scryfall_id, &finish.as_str()],
                )?;
            } else {
                tx.execute(
                    "update collection set quantity = $3, updated_at = now()
                     where scryfall_id = $1::text::uuid and finish = $2",
                    &[&scryfall_id, &finish.as_str(), &left],
                )?;
            }
            tx.commit()?;
            left as u32
        };
        if remaining > 0 {
            let rows = self.stored_rows(Some((scryfall_id, finish)))?;
            return Ok(hydrate(Some(self), Some(&self.scryfall), rows)
                .into_iter()
                .next());
        }
        // Row is gone; describe it from Scryfall alone.
        Ok(Some(removed_entry(
            Some(self),
            Some(&self.scryfall),
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
            return self.remove(scryfall_id, finish, i32::MAX as u32);
        }
        let updated = self.client.borrow_mut().execute(
            "update collection set quantity = $3, updated_at = now()
             where scryfall_id = $1::text::uuid and finish = $2",
            &[&scryfall_id, &finish.as_str(), &(quantity as i32)],
        )?;
        if updated == 0 {
            return Ok(None);
        }
        let rows = self.stored_rows(Some((scryfall_id, finish)))?;
        Ok(hydrate(Some(self), Some(&self.scryfall), rows)
            .into_iter()
            .next())
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
        let mut client = self.client.borrow_mut();
        let mut tx = client.transaction()?;
        let Some(old) = tx.query_opt(
            "delete from collection where scryfall_id = $1::text::uuid and finish = $2
             returning added_at",
            &[&scryfall_id, &finish.as_str()],
        )?
        else {
            return Ok(None);
        };
        let first_added: DateTime<Utc> = old.get(0);
        // Merge into the target row if it exists, keeping the earliest add date.
        let row = tx.query_one(
            "insert into collection (scryfall_id, finish, quantity, added_at)
             values ($1::text::uuid, $2, $3, $4)
             on conflict (scryfall_id, finish)
             do update set quantity = collection.quantity + excluded.quantity,
                           added_at = least(collection.added_at, excluded.added_at),
                           updated_at = now()
             returning quantity, added_at",
            &[
                &to.scryfall_id,
                &to.finish.as_str(),
                &(to.quantity as i32),
                &first_added,
            ],
        )?;
        let date = to
            .price_updated
            .unwrap_or_else(|| Local::now().date_naive());
        record_price(&mut tx, &to.scryfall_id, to.finish, date, &to.prices)?;
        tx.commit()?;
        let quantity: i32 = row.get(0);
        let added: DateTime<Utc> = row.get(1);
        Ok(Some(CollectionEntry {
            quantity: quantity as u32,
            added: added.with_timezone(&Local).date_naive(),
            ..to
        }))
    }

    fn import(&mut self, entries: &[CollectionEntry]) -> Result<usize> {
        let mut client = self.client.borrow_mut();
        let mut tx = client.transaction()?;
        for e in entries {
            tx.execute(
                "insert into collection (scryfall_id, finish, quantity, added_at)
                 values ($1::text::uuid, $2, $3, $4::date)
                 on conflict (scryfall_id, finish)
                 do update set quantity = excluded.quantity, updated_at = now()",
                &[
                    &e.scryfall_id,
                    &e.finish.as_str(),
                    &(e.quantity as i32),
                    &e.added,
                ],
            )
            .with_context(|| format!("failed to import {} ({})", e.name, e.scryfall_id))?;
            let date = e.price_updated.unwrap_or(e.added);
            record_price(&mut tx, &e.scryfall_id, e.finish, date, &e.prices)?;
        }
        tx.commit()?;
        Ok(entries.len())
    }

    fn update_prices(&mut self, scryfall_id: &str, finish: Finish, prices: Prices) -> Result<()> {
        let mut client = self.client.borrow_mut();
        record_price(
            &mut *client,
            scryfall_id,
            finish,
            Local::now().date_naive(),
            &prices,
        )
    }

    fn all_price_points(&self) -> Result<Vec<(String, Finish, PricePoint)>> {
        let rows = self.client.borrow_mut().query(
            &format!(
                "select scryfall_id::text, finish, price_date, price_tcg::float8, {},
                        price_ck::float8, price_ck_buylist::float8
                 from price_history
                 order by scryfall_id, finish, price_date",
                self.newer_prices("")
            ),
            &[],
        )?;
        rows.iter()
            .map(|r| {
                let finish: String = r.get(1);
                Ok((r.get(0), finish.parse()?, point_at(r, 2)))
            })
            .collect()
    }

    fn import_price_points(&mut self, points: &[(String, Finish, PricePoint)]) -> Result<usize> {
        let mut client = self.client.borrow_mut();
        let mut tx = client.transaction()?;
        for (id, finish, p) in points {
            let prices = Prices {
                tcgplayer: p.tcgplayer,
                cardmarket: p.cardmarket,
                tix: p.tix,
                cardkingdom: p.cardkingdom,
                cardkingdom_buylist: p.cardkingdom_buylist,
                ..Prices::default()
            };
            record_price(&mut tx, id, *finish, p.date, &prices)?;
        }
        tx.commit()?;
        Ok(points.len())
    }

    fn price_history(&self, scryfall_id: &str, finish: Finish) -> Result<Vec<PricePoint>> {
        let mut client = self.client.borrow_mut();
        let rows = client.query(
            "select price_date, price_tcg::float8, price_cardmarket::float8, price_tix::float8,
                    price_ck::float8, price_ck_buylist::float8
             from price_history
             where scryfall_id = $1::text::uuid and finish = $2
             order by price_date",
            &[&scryfall_id, &finish.as_str()],
        )?;
        Ok(rows.iter().map(|r| point_at(r, 0)).collect())
    }

    fn price_moves(&self, since: NaiveDate) -> Result<Vec<PriceMove>> {
        let rows = self.client.borrow_mut().query(
            "select c.scryfall_id::text, c.finish, c.quantity,
                    old.price_date, old.price_tcg::float8, old.price_cardmarket::float8,
                    old.price_tix::float8, old.price_ck::float8, old.price_ck_buylist::float8,
                    cur.price_date, cur.price_tcg::float8, cur.price_cardmarket::float8,
                    cur.price_tix::float8, cur.price_ck::float8, cur.price_ck_buylist::float8
             from collection c
             join lateral (
               select * from price_history ph
               where ph.scryfall_id = c.scryfall_id and ph.finish = c.finish
                 and ph.price_date <= $1
               order by ph.price_date desc limit 1
             ) old on true
             join lateral (
               select * from price_history ph
               where ph.scryfall_id = c.scryfall_id and ph.finish = c.finish
               order by ph.price_date desc limit 1
             ) cur on true
             where cur.price_date > old.price_date",
            &[&since],
        )?;
        rows.iter()
            .map(|r| {
                let finish: String = r.get(1);
                let quantity: i32 = r.get(2);
                Ok(PriceMove {
                    scryfall_id: r.get(0),
                    finish: finish.parse()?,
                    quantity: quantity as u32,
                    then: point_at(r, 3),
                    now: point_at(r, 9),
                })
            })
            .collect()
    }

    fn value_change(&self, since: NaiveDate) -> Result<Option<ValueChange>> {
        let row = self.client.borrow_mut().query_one(
            "select coalesce(sum(c.quantity * old.price_tcg), 0)::float8,
                    coalesce(sum(c.quantity * cur.price_tcg), 0)::float8,
                    count(*)
             from collection c
             join lateral (
               select ph.price_tcg from price_history ph
               where ph.scryfall_id = c.scryfall_id and ph.finish = c.finish
                 and ph.price_date <= $1 and ph.price_tcg is not null
               order by ph.price_date desc limit 1
             ) old on true
             join lateral (
               select ph.price_tcg from price_history ph
               where ph.scryfall_id = c.scryfall_id and ph.finish = c.finish
                 and ph.price_tcg is not null
               order by ph.price_date desc limit 1
             ) cur on true",
            &[&since],
        )?;
        let rows: i64 = row.get(2);
        Ok((rows > 0).then(|| ValueChange {
            before: row.get(0),
            now: row.get(1),
            rows: rows as u32,
        }))
    }

    fn record_cached_prices(&mut self, date: NaiveDate) -> Result<usize> {
        let n = self.client.borrow_mut().execute(
            "insert into price_history
               (scryfall_id, finish, price_date, price_tcg, price_cardmarket, price_tix)
             select c.scryfall_id, c.finish, $1::date,
                    case c.finish when 'foil' then p.usd_foil
                                  when 'etched' then p.usd_etched else p.usd end,
                    case c.finish when 'foil' then p.eur_foil
                                  when 'etched' then null else p.eur end,
                    p.tix
             from collection c
             join card_printing p on p.scryfall_id = c.scryfall_id
             where coalesce(p.usd, p.usd_foil, p.usd_etched, p.eur, p.eur_foil, p.tix) is not null
             on conflict (scryfall_id, finish, price_date) do update set
               price_tcg = coalesce(excluded.price_tcg, price_history.price_tcg),
               price_cardmarket = coalesce(excluded.price_cardmarket, price_history.price_cardmarket),
               price_tix = coalesce(excluded.price_tix, price_history.price_tix),
               captured_at = now()",
            &[&date],
        )?;
        Ok(n as usize)
    }

    fn location(&self) -> String {
        self.location.clone()
    }

    fn card_cache(&self) -> Option<&dyn CardCache> {
        // Migration 2 added the cache.
        (self.version >= 2).then_some(self as &dyn CardCache)
    }
}

/// `card_printing` columns in the order [`card_from_row`] reads them.
const CARD_COLUMNS: &str = "scryfall_id::text, oracle_id::text, name, lang, set_code, set_name,
    collector_number, rarity, type_line, colors, finishes, tcgplayer_id, tcgplayer_etched_id,
    cardmarket_id, image_small, image_normal, usd::float8, usd_foil::float8,
    usd_etched::float8, eur::float8, eur_foil::float8, tix::float8";

/// Rows per multi-row upsert (22 parameters each; PostgreSQL allows 65535).
const CARD_BATCH: usize = 500;

impl CardCache for PostgresStore {
    fn cached_cards(&self, ids: &[String]) -> Result<HashMap<String, Card>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let rows = self.client.borrow_mut().query(
            &format!(
                "select {CARD_COLUMNS} from card_printing
                 where scryfall_id = any($1::text[]::uuid[])"
            ),
            &[&ids],
        )?;
        Ok(rows
            .iter()
            .map(|r| {
                let card = card_from_row(r);
                (card.scryfall_id.clone(), card)
            })
            .collect())
    }

    fn store_cards(&self, cards: &[Card]) -> Result<usize> {
        let mut client = self.client.borrow_mut();
        let mut tx = client.transaction()?;
        for chunk in cards.chunks(CARD_BATCH) {
            let mut values = Vec::with_capacity(chunk.len());
            let mut params: Vec<Box<dyn ToSql + Sync>> = Vec::with_capacity(chunk.len() * 22);
            for (i, c) in chunk.iter().enumerate() {
                let n = i * 22;
                let p = |k: usize| format!("${}", n + k);
                values.push(format!(
                    "({}::text::uuid, {}::text::uuid, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, \
                     {}, {}, {}, {}::float8, {}::float8, {}::float8, {}::float8, {}::float8, \
                     {}::float8, now())",
                    p(1),
                    p(2),
                    p(3),
                    p(4),
                    p(5),
                    p(6),
                    p(7),
                    p(8),
                    p(9),
                    p(10),
                    p(11),
                    p(12),
                    p(13),
                    p(14),
                    p(15),
                    p(16),
                    p(17),
                    p(18),
                    p(19),
                    p(20),
                    p(21),
                    p(22),
                ));
                params.push(Box::new(c.scryfall_id.clone()));
                params.push(Box::new(c.oracle_id.clone()));
                params.push(Box::new(c.name.clone()));
                params.push(Box::new(c.lang.clone()));
                params.push(Box::new(c.set_code.clone()));
                params.push(Box::new(c.set_name.clone()));
                params.push(Box::new(c.collector_number.clone()));
                params.push(Box::new(c.rarity.clone()));
                params.push(Box::new(c.type_line.clone()));
                params.push(Box::new(colors_text(&c.colors)));
                params.push(Box::new(finishes_text(&c.finishes)));
                params.push(Box::new(c.tcgplayer_id.map(|v| v as i64)));
                params.push(Box::new(c.tcgplayer_etched_id.map(|v| v as i64)));
                params.push(Box::new(c.cardmarket_id.map(|v| v as i64)));
                params.push(Box::new(c.image_small.clone()));
                params.push(Box::new(c.image_normal.clone()));
                params.push(Box::new(c.scryfall_usd));
                params.push(Box::new(c.scryfall_usd_foil));
                params.push(Box::new(c.scryfall_usd_etched));
                params.push(Box::new(c.scryfall_eur));
                params.push(Box::new(c.scryfall_eur_foil));
                params.push(Box::new(c.scryfall_tix));
            }
            let sql = format!(
                "insert into card_printing (scryfall_id, oracle_id, name, lang, set_code,
                   set_name, collector_number, rarity, type_line, colors, finishes, tcgplayer_id,
                   tcgplayer_etched_id, cardmarket_id, image_small, image_normal, usd, usd_foil,
                   usd_etched, eur, eur_foil, tix, synced_at)
                 values {}
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
                   synced_at = excluded.synced_at",
                values.join(", ")
            );
            let refs: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| p.as_ref()).collect();
            tx.execute(&sql, &refs)?;
        }
        tx.commit()?;
        Ok(cards.len())
    }

    fn cached_count(&self) -> Result<i64> {
        Ok(self
            .client
            .borrow_mut()
            .query_one("select count(*) from card_printing", &[])?
            .get(0))
    }

    fn job(&self, name: &str) -> Result<Option<Job>> {
        Ok(self
            .client
            .borrow_mut()
            .query_opt(
                "select done_at, detail from app_job where name = $1",
                &[&name],
            )?
            .map(|r| Job {
                done_at: r.get(0),
                detail: r.get(1),
            }))
    }

    fn finish_job(&self, name: &str, detail: &str) -> Result<()> {
        self.client.borrow_mut().execute(
            "insert into app_job (name, done_at, detail) values ($1, now(), $2)
             on conflict (name) do update set done_at = excluded.done_at, detail = excluded.detail",
            &[&name, &detail],
        )?;
        Ok(())
    }
}

fn card_from_row(r: &Row) -> Card {
    let id = |at: usize| r.get::<_, Option<i64>>(at).map(|v| v as u64);
    Card {
        scryfall_id: r.get(0),
        oracle_id: r.get(1),
        name: r.get(2),
        lang: r.get(3),
        set_code: r.get(4),
        set_name: r.get(5),
        collector_number: r.get(6),
        rarity: r.get(7),
        type_line: r.get(8),
        colors: colors_from_text(r.get(9)),
        finishes: finishes_from_text(r.get(10)),
        tcgplayer_id: id(11),
        tcgplayer_etched_id: id(12),
        cardmarket_id: id(13),
        image_small: r.get(14),
        image_normal: r.get(15),
        scryfall_usd: r.get(16),
        scryfall_usd_foil: r.get(17),
        scryfall_usd_etched: r.get(18),
        scryfall_eur: r.get(19),
        scryfall_eur_foil: r.get(20),
        scryfall_tix: r.get(21),
    }
}

/// Upsert `date`'s price point. Skipped when no price is known. A price
/// missing here keeps the one already recorded that day, so sources recorded
/// separately (Scryfall's daily data, then Card Kingdom) don't erase each other.
fn record_price(
    client: &mut impl GenericClient,
    scryfall_id: &str,
    finish: Finish,
    date: NaiveDate,
    prices: &Prices,
) -> Result<()> {
    if prices.tcgplayer.is_none()
        && prices.cardmarket.is_none()
        && prices.tix.is_none()
        && prices.cardkingdom.is_none()
        && prices.cardkingdom_buylist.is_none()
    {
        return Ok(());
    }
    client.execute(
        "insert into price_history (scryfall_id, finish, price_date, price_tcg, price_cardmarket,
           price_tix, price_ck, price_ck_buylist)
         values ($1::text::uuid, $2, $3::date, $4::float8, $5::float8, $6::float8, $7::float8,
           $8::float8)
         on conflict (scryfall_id, finish, price_date)
         do update set price_tcg = coalesce(excluded.price_tcg, price_history.price_tcg),
                       price_cardmarket = coalesce(excluded.price_cardmarket,
                                                   price_history.price_cardmarket),
                       price_tix = coalesce(excluded.price_tix, price_history.price_tix),
                       price_ck = coalesce(excluded.price_ck, price_history.price_ck),
                       price_ck_buylist = coalesce(excluded.price_ck_buylist,
                                                   price_history.price_ck_buylist),
                       captured_at = now()",
        &[
            &scryfall_id,
            &finish.as_str(),
            &date,
            &prices.tcgplayer,
            &prices.cardmarket,
            &prices.tix,
            &prices.cardkingdom,
            &prices.cardkingdom_buylist,
        ],
    )?;
    Ok(())
}

/// The date and five prices (TCGplayer, Cardmarket, tix, Card Kingdom, CK
/// buylist) starting at column `at`.
fn point_at(r: &Row, at: usize) -> PricePoint {
    PricePoint {
        date: r.get(at),
        tcgplayer: r.get(at + 1),
        cardmarket: r.get(at + 2),
        tix: r.get(at + 3),
        cardkingdom: r.get(at + 4),
        cardkingdom_buylist: r.get(at + 5),
    }
}

fn stored_row(r: &Row) -> Result<StoredRow> {
    let finish: String = r.get(1);
    let quantity: i32 = r.get(2);
    let added: DateTime<Utc> = r.get(3);
    Ok(StoredRow {
        scryfall_id: r.get(0),
        finish: finish.parse()?,
        quantity: quantity as u32,
        added: added.with_timezone(&Local).date_naive(),
        prices: Prices {
            tcgplayer: r.get(4),
            cardmarket: r.get(5),
            tix: r.get(6),
            cardkingdom: r.get(7),
            cardkingdom_buylist: r.get(8),
            ..Prices::default()
        },
        price_date: r.get(9),
    })
}

/// `user@host:port/db (schema s)` — never includes the password.
fn describe(config: &postgres::Config, schema: &str) -> String {
    let host = config
        .get_hosts()
        .first()
        .map(|h| match h {
            postgres::config::Host::Tcp(name) => name.clone(),
            #[allow(unreachable_patterns)]
            _ => "local socket".into(),
        })
        .unwrap_or_else(|| "localhost".into());
    let port = config.get_ports().first().copied().unwrap_or(5432);
    format!(
        "postgres://{}@{host}:{port}/{} (schema {schema})",
        config.get_user().unwrap_or("?"),
        config.get_dbname().unwrap_or("?")
    )
}

/// Schema names are interpolated into SQL, so only allow plain identifiers.
fn validate_identifier(name: &str) -> Result<()> {
    let ok = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && name.len() <= 63;
    if !ok {
        bail!("invalid schema name '{name}': use lowercase letters, digits and underscores");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_names_are_validated() {
        assert!(validate_identifier("mtg_scan").is_ok());
        assert!(validate_identifier("_x1").is_ok());
        assert!(validate_identifier("Mtg").is_err());
        assert!(validate_identifier("x; drop table collection").is_err());
        assert!(validate_identifier("").is_err());
    }

    #[test]
    fn location_hides_password() {
        let cfg: postgres::Config = "postgres://alice:s3cret@db.example.com:6543/cards"
            .parse()
            .unwrap();
        let loc = describe(&cfg, "mtg_scan");
        assert_eq!(
            loc,
            "postgres://alice@db.example.com:6543/cards (schema mtg_scan)"
        );
        assert!(!loc.contains("s3cret"));
    }

    #[test]
    fn migrations_are_ordered_and_unique() {
        let versions: Vec<i32> = MIGRATIONS.iter().map(|m| m.0).collect();
        let mut sorted = versions.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(versions, sorted);
    }

    /// Full round trip against a real database. Run with
    /// `MTG_SCAN_TEST_DATABASE_URL=postgres://... cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn round_trip_against_postgres() {
        let url =
            std::env::var("MTG_SCAN_TEST_DATABASE_URL").expect("set MTG_SCAN_TEST_DATABASE_URL");
        let scryfall = Rc::new(Scryfall::new(crate::app::http_client().unwrap()));
        let schema = format!("mtg_scan_test_{}", std::process::id());
        let config: postgres::Config = url.parse().unwrap();
        // A fresh schema refuses day-to-day connections until migrated.
        let err = PostgresStore::connect(config.clone(), &schema, scryfall.clone())
            .err()
            .unwrap();
        assert!(err.to_string().contains("db migrate"));
        let (admin, applied) =
            PostgresStore::connect_admin(config.clone(), &schema, scryfall.clone()).unwrap();
        assert_eq!(applied, (1..=latest_migration()).collect::<Vec<_>>());
        // Applying again is a no-op.
        assert!(admin.migrate().unwrap().is_empty());

        // Least-privilege app role: can write rows, cannot change the schema.
        let role = format!("mtg_scan_test_app_{}", std::process::id());
        let password = "test-Pa$$word#1";
        assert!(matches!(
            admin.setup_app_role(&role, password).unwrap(),
            RoleSetup::Ready { created: true }
        ));
        let mut app_config = config.clone();
        app_config.user(&role).password(password);
        let mut store = PostgresStore::connect(app_config, &schema, scryfall).unwrap();
        let ddl = store
            .client
            .borrow_mut()
            .batch_execute("drop table price_history");
        assert!(ddl.is_err(), "app role must not be able to drop tables");

        crate::store::conformance(&mut store);
        let rows = store.entries().unwrap();
        assert_eq!(rows[0].name, "Lightning Bolt", "details come from Scryfall");

        // Copying into SQLite keeps every row and price point.
        let mut lite = crate::store::sqlite::SqliteStore::open_in_memory().unwrap();
        let (copied, points) = crate::store::copy(&store, &mut lite).unwrap();
        assert_eq!(copied, rows.len());
        assert_eq!(points, store.all_price_points().unwrap().len());
        assert_eq!(
            store.all_price_points().unwrap(),
            lite.all_price_points().unwrap()
        );

        let status = store.status().unwrap();
        assert_eq!(status.schema_version, latest_migration());

        // A 0.x database (schema version 1) copies into SQLite without being upgraded.
        let legacy = format!("{schema}_v1");
        admin
            .client
            .borrow_mut()
            .batch_execute(&format!(
                "create schema {legacy}; set search_path to {legacy};
                 create table schema_migrations (version integer primary key, name text not null,
                   applied_at timestamptz not null default now());
                 {}
                 insert into schema_migrations (version, name) values (1, 'init');
                 insert into collection (scryfall_id, finish, quantity, added_at)
                   values ('e768c957-3a1f-42f5-853a-96942f645df5', 'foil', 2, '2026-10-01');
                 insert into price_history (scryfall_id, finish, price_date, price_tcg, price_ck)
                   values ('e768c957-3a1f-42f5-853a-96942f645df5', 'foil', '2026-10-01', 9.10, 11.99);
                 set search_path to {schema};",
                MIGRATIONS[0].2
            ))
            .unwrap();
        let old = PostgresStore::open(
            config.clone(),
            &legacy,
            Rc::new(Scryfall::new(crate::app::http_client().unwrap())),
        )
        .unwrap();
        assert!(
            old.card_cache().is_none(),
            "no card cache before migration 2"
        );
        let mut lite = crate::store::sqlite::SqliteStore::open_in_memory().unwrap();
        assert_eq!(crate::store::copy(&old, &mut lite).unwrap(), (1, 1));
        let row = &lite.entries().unwrap()[0];
        assert_eq!((row.finish, row.quantity), (Finish::Foil, 2));
        let point = &lite.all_price_points().unwrap()[0].2;
        assert_eq!(
            (point.tcgplayer, point.cardkingdom, point.cardmarket),
            (Some(9.10), Some(11.99), None)
        );
        assert_eq!(
            old.schema_version().unwrap(),
            1,
            "the old database is left as it was"
        );
        drop(old);
        admin
            .client
            .borrow_mut()
            .batch_execute(&format!("drop schema {legacy} cascade"))
            .unwrap();
        assert_eq!(status.cards, 6);

        drop(store);
        admin
            .client
            .borrow_mut()
            .batch_execute(&format!(
                "drop schema {schema} cascade; drop owned by {role}; drop role {role};"
            ))
            .unwrap();
    }
}
