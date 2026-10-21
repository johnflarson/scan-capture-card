//! PostgreSQL collection store (iteration 2).
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
use postgres::{Client, GenericClient, Row};
use postgres_native_tls::MakeTlsConnector;

use super::CollectionStore;
use crate::model::{CollectionEntry, Finish, PricePoint, Prices};
use crate::scryfall::Scryfall;

fn latest_migration() -> i32 {
    MIGRATIONS.iter().map(|m| m.0).max().unwrap_or(0)
}

/// (version, name, sql). Append new migrations; never edit applied ones.
const MIGRATIONS: &[(i32, &str, &str)] =
    &[(1, "init", include_str!("../../migrations/0001_init.sql"))];

pub struct PostgresStore {
    client: RefCell<Client>,
    scryfall: Rc<Scryfall>,
    schema: String,
    location: String,
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

/// A collection row as stored: no card details.
struct StoredRow {
    scryfall_id: String,
    finish: Finish,
    quantity: u32,
    added: NaiveDate,
    prices: Prices,
    price_date: Option<NaiveDate>,
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
        Ok(Self {
            client: RefCell::new(client),
            scryfall,
            schema: schema.to_string(),
            location,
        })
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

    /// Import rows (e.g. from `collection.md`), setting quantities rather than
    /// adding to them, and recording each row's prices as a price point.
    pub fn import(&self, entries: &[CollectionEntry]) -> Result<usize> {
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

    fn stored_rows(&self, filter: Option<(&str, Finish)>) -> Result<Vec<StoredRow>> {
        const SELECT: &str = "select scryfall_id::text, finish, quantity, added_at,
                price_tcg::float8, price_ck::float8, price_ck_buylist::float8, price_date
             from collection_value";
        let mut client = self.client.borrow_mut();
        let rows = match filter {
            Some((id, finish)) => client.query(
                &format!("{SELECT} where scryfall_id = $1::text::uuid and finish = $2"),
                &[&id, &finish.as_str()],
            )?,
            None => client.query(SELECT, &[])?,
        };
        rows.iter().map(stored_row).collect()
    }

    /// Fill in card details from Scryfall. Falls back to placeholders when
    /// Scryfall is unreachable so the collection can still be listed.
    fn hydrate(&self, rows: Vec<StoredRow>) -> Vec<CollectionEntry> {
        let ids: Vec<String> = rows.iter().map(|r| r.scryfall_id.clone()).collect();
        let cards = self.scryfall.cards_by_id(&ids).unwrap_or_else(|e| {
            eprintln!("  ! could not fetch card details from Scryfall ({e:#})");
            HashMap::new()
        });
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
                    scryfall_id: r.scryfall_id,
                    finish: r.finish,
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
        });
        entries
    }
}

impl CollectionStore for PostgresStore {
    fn entries(&self) -> Result<Vec<CollectionEntry>> {
        Ok(self.hydrate(self.stored_rows(None)?))
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
            return Ok(self.hydrate(rows).into_iter().next());
        }
        // Row is gone; describe it from Scryfall alone.
        let card = self.scryfall.by_id(scryfall_id).ok().flatten();
        Ok(Some(CollectionEntry {
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
            finish,
            quantity: 0,
            prices: Prices::default(),
            added: Local::now().date_naive(),
            price_updated: None,
        }))
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

    fn price_history(&self, scryfall_id: &str, finish: Finish) -> Result<Vec<PricePoint>> {
        let mut client = self.client.borrow_mut();
        let rows = client.query(
            "select price_date, price_tcg::float8, price_ck::float8, price_ck_buylist::float8
             from price_history
             where scryfall_id = $1::text::uuid and finish = $2
             order by price_date",
            &[&scryfall_id, &finish.as_str()],
        )?;
        Ok(rows
            .iter()
            .map(|r| PricePoint {
                date: r.get(0),
                tcgplayer: r.get(1),
                cardkingdom: r.get(2),
                cardkingdom_buylist: r.get(3),
            })
            .collect())
    }

    fn location(&self) -> String {
        self.location.clone()
    }
}

/// Upsert today's (or `date`'s) price point. Skipped when no price is known.
fn record_price(
    client: &mut impl GenericClient,
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
    client.execute(
        "insert into price_history (scryfall_id, finish, price_date, price_tcg, price_ck, price_ck_buylist)
         values ($1::text::uuid, $2, $3::date, $4::float8, $5::float8, $6::float8)
         on conflict (scryfall_id, finish, price_date)
         do update set price_tcg = excluded.price_tcg,
                       price_ck = excluded.price_ck,
                       price_ck_buylist = excluded.price_ck_buylist,
                       captured_at = now()",
        &[
            &scryfall_id,
            &finish.as_str(),
            &date,
            &prices.tcgplayer,
            &prices.cardkingdom,
            &prices.cardkingdom_buylist,
        ],
    )?;
    Ok(())
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
            cardkingdom: r.get(5),
            cardkingdom_buylist: r.get(6),
            tcgplayer_url: None,
            cardkingdom_url: None,
        },
        price_date: r.get(7),
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
        assert_eq!(applied, vec![1]);
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

        let today = Local::now().date_naive();
        let entry = |id: &str, finish, qty, tcg| CollectionEntry {
            scryfall_id: id.into(),
            name: "Lightning Bolt".into(),
            set_code: "X".into(),
            set_name: "X".into(),
            collector_number: "1".into(),
            rarity: "common".into(),
            finish,
            quantity: qty,
            prices: Prices {
                tcgplayer: Some(tcg),
                cardkingdom: Some(tcg + 1.0),
                cardkingdom_buylist: None,
                ..Prices::default()
            },
            added: today,
            price_updated: Some(today),
        };
        // Two printings of Lightning Bolt (M11 #149 and A25 #141) + a foil copy.
        let m11 = "e768c957-3a1f-42f5-853a-96942f645df5";
        let a25 = "e3285e6b-3e79-4d7c-bf96-d920f973b122";
        store.add(entry(m11, Finish::Nonfoil, 2, 1.0)).unwrap();
        let merged = store.add(entry(m11, Finish::Nonfoil, 1, 1.5)).unwrap();
        assert_eq!(merged.quantity, 3);
        store.add(entry(a25, Finish::Nonfoil, 1, 2.0)).unwrap();
        store.add(entry(a25, Finish::Foil, 1, 5.0)).unwrap();

        let rows = store.entries().unwrap();
        assert_eq!(rows.len(), 3, "one row per printing + finish");
        let m11_row = rows.iter().find(|e| e.scryfall_id == m11).unwrap();
        assert_eq!(m11_row.quantity, 3);
        assert_eq!(
            m11_row.prices.tcgplayer,
            Some(1.5),
            "same-day price point is replaced"
        );
        assert_eq!(m11_row.name, "Lightning Bolt", "details come from Scryfall");
        assert_eq!(store.price_history(m11, Finish::Nonfoil).unwrap().len(), 1);

        let left = store.remove(a25, Finish::Foil, 5).unwrap().unwrap();
        assert_eq!(left.quantity, 0);
        assert_eq!(store.entries().unwrap().len(), 2);
        // History survives removal.
        assert_eq!(store.price_history(a25, Finish::Foil).unwrap().len(), 1);
        assert!(store.remove(a25, Finish::Foil, 1).unwrap().is_none());

        let status = store.status().unwrap();
        assert_eq!(status.schema_version, 1);
        assert_eq!(status.cards, 4);

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
