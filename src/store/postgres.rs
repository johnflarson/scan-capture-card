//! PostgreSQL collection store (iteration 2; optional since 1.0, where
//! SQLite is the default).
//!
//! Only what isn't available online is stored: Scryfall ID, finish,
//! quantity and dated prices. Names, sets, numbers and rarities are fetched
//! from Scryfall in batches whenever rows are listed. Schema:
//! `migrations/*.sql`, applied by `mtg-scan db migrate` with the admin
//! (schema owner) account; the app itself runs as a least-privilege role.

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local, NaiveDate, Utc};
use postgres::{Client, GenericClient, Row};
use postgres_native_tls::MakeTlsConnector;

use super::{CollectionStore, PriceMove, StoredRow, ValueChange, hydrate, removed_entry};
use crate::model::{CollectionEntry, Finish, PricePoint, Prices};
use crate::scryfall::Scryfall;

pub fn latest_migration() -> i32 {
    MIGRATIONS.iter().map(|m| m.0).max().unwrap_or(0)
}

/// (version, name, sql). Append new migrations; never edit applied ones.
const MIGRATIONS: &[(i32, &str, &str)] = &[(
    1,
    "init",
    include_str!("../../migrations/postgres/0001_init.sql"),
)];

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
}

impl CollectionStore for PostgresStore {
    fn entries(&self) -> Result<Vec<CollectionEntry>> {
        Ok(hydrate(Some(&self.scryfall), self.stored_rows(None)?))
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
            return Ok(hydrate(Some(&self.scryfall), rows).into_iter().next());
        }
        // Row is gone; describe it from Scryfall alone.
        Ok(Some(removed_entry(
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
        Ok(hydrate(Some(&self.scryfall), rows).into_iter().next())
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

    fn price_moves(&self, since: NaiveDate) -> Result<Vec<PriceMove>> {
        let rows = self.client.borrow_mut().query(
            "select c.scryfall_id::text, c.finish, c.quantity,
                    old.price_date, old.price_tcg::float8, old.price_ck::float8,
                    old.price_ck_buylist::float8,
                    cur.price_date, cur.price_tcg::float8, cur.price_ck::float8,
                    cur.price_ck_buylist::float8
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
                let point = |at: usize| PricePoint {
                    date: r.get(at),
                    tcgplayer: r.get(at + 1),
                    cardkingdom: r.get(at + 2),
                    cardkingdom_buylist: r.get(at + 3),
                };
                Ok(PriceMove {
                    scryfall_id: r.get(0),
                    finish: finish.parse()?,
                    quantity: quantity as u32,
                    then: point(3),
                    now: point(7),
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

        crate::store::conformance(&mut store);
        let rows = store.entries().unwrap();
        assert_eq!(rows[0].name, "Lightning Bolt", "details come from Scryfall");

        let status = store.status().unwrap();
        assert_eq!(status.schema_version, 1);
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
