//! The account tables in PostgreSQL (`migrations/postgres/0004_accounts.sql`).
//! One connection, opened again if the server dropped it.

use std::rc::Rc;

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use postgres::{Client, Row};

use super::{AccountStore, ApiToken, Link, LinkKind, User};
use crate::config::Config;
use crate::scryfall::Scryfall;
use crate::store::postgres::PostgresStore;

pub struct PostgresAccounts {
    client: Client,
    cfg: Config,
}

/// `app_user` columns in the order [`user_from`] reads them.
const USER_COLUMNS: &str = "u.id, u.name, u.is_admin, u.disabled, u.password_hash is not null,
    u.created_at, u.last_seen_at";

fn user_from(r: &Row, at: usize) -> User {
    User {
        id: r.get(at),
        name: r.get(at + 1),
        is_admin: r.get(at + 2),
        disabled: r.get(at + 3),
        has_password: r.get(at + 4),
        created_at: r.get(at + 5),
        last_seen_at: r.get(at + 6),
    }
}

fn open(cfg: &Config) -> Result<Client> {
    // Connecting checks the schema version; card lookups aren't needed.
    let scryfall = Rc::new(Scryfall::new(crate::app::http_client()?));
    Ok(crate::app::connect_postgres(cfg, scryfall)?.into_client())
}

impl PostgresAccounts {
    pub fn connect(cfg: &Config) -> Result<Self> {
        Ok(Self {
            client: open(cfg)?,
            cfg: cfg.clone(),
        })
    }

    /// The connection, reopened if the server closed it.
    fn client(&mut self) -> Result<&mut Client> {
        if self.client.is_closed() {
            self.client = open(&self.cfg)?;
        }
        Ok(&mut self.client)
    }

    fn one_user(
        &mut self,
        sql_where: &str,
        param: &(dyn postgres::types::ToSql + Sync),
    ) -> Result<Option<User>> {
        Ok(self
            .client()?
            .query_opt(
                &format!("select {USER_COLUMNS} from app_user u where {sql_where}"),
                &[param],
            )?
            .map(|r| user_from(&r, 0)))
    }
}

impl AccountStore for PostgresAccounts {
    fn user_count(&mut self) -> Result<i64> {
        Ok(self
            .client()?
            .query_one("select count(*) from app_user", &[])?
            .get(0))
    }

    fn users(&mut self) -> Result<Vec<User>> {
        Ok(self
            .client()?
            .query(
                &format!("select {USER_COLUMNS} from app_user u order by lower(u.name)"),
                &[],
            )?
            .iter()
            .map(|r| user_from(r, 0))
            .collect())
    }

    fn user(&mut self, id: i64) -> Result<Option<User>> {
        self.one_user("u.id = $1", &id)
    }

    fn user_by_name(&mut self, name: &str) -> Result<Option<(User, Option<String>)>> {
        Ok(self
            .client()?
            .query_opt(
                &format!(
                    "select {USER_COLUMNS}, u.password_hash from app_user u
                     where lower(u.name) = lower($1)"
                ),
                &[&name],
            )?
            .map(|r| (user_from(&r, 0), r.get(7))))
    }

    fn create_first_admin(&mut self, name: &str, password_hash: &str) -> Result<Option<User>> {
        let mut tx = self.client()?.transaction()?;
        tx.batch_execute("lock table app_user in exclusive mode")?;
        let count: i64 = tx.query_one("select count(*) from app_user", &[])?.get(0);
        if count > 0 {
            return Ok(None);
        }
        let row = tx.query_one(
            &format!(
                "insert into app_user as u (name, password_hash, is_admin) values ($1, $2, true)
                 returning {USER_COLUMNS}"
            ),
            &[&name, &password_hash],
        )?;
        tx.commit()?;
        Ok(Some(user_from(&row, 0)))
    }

    fn redeem_invite(
        &mut self,
        token_hash: &str,
        name: &str,
        password_hash: &str,
    ) -> Result<Option<User>> {
        let mut tx = self.client()?.transaction()?;
        let used = tx.execute(
            "update account_link set used_at = now()
             where token_hash = $1 and kind = 'invite' and used_at is null and expires_at > now()",
            &[&token_hash],
        )?;
        if used == 0 {
            return Ok(None);
        }
        let row = tx.query_one(
            &format!(
                "insert into app_user as u (name, password_hash) values ($1, $2)
                 returning {USER_COLUMNS}"
            ),
            &[&name, &password_hash],
        )?;
        tx.commit()?;
        Ok(Some(user_from(&row, 0)))
    }

    fn redeem_reset(&mut self, token_hash: &str, password_hash: &str) -> Result<Option<User>> {
        let mut tx = self.client()?.transaction()?;
        let Some(user_id) = tx
            .query_opt(
                "update account_link set used_at = now()
                 where token_hash = $1 and kind = 'reset' and used_at is null
                   and expires_at > now()
                 returning user_id",
                &[&token_hash],
            )?
            .and_then(|r| r.get::<_, Option<i64>>(0))
        else {
            return Ok(None);
        };
        tx.execute(
            "update app_user set password_hash = $2 where id = $1",
            &[&user_id, &password_hash],
        )?;
        tx.execute("delete from user_session where user_id = $1", &[&user_id])?;
        tx.commit()?;
        self.user(user_id)
    }

    fn set_password(
        &mut self,
        user_id: i64,
        password_hash: &str,
        keep: Option<&str>,
    ) -> Result<()> {
        let mut tx = self.client()?.transaction()?;
        tx.execute(
            "update app_user set password_hash = $2 where id = $1",
            &[&user_id, &password_hash],
        )?;
        tx.execute(
            "delete from user_session where user_id = $1 and token_hash is distinct from $2",
            &[&user_id, &keep],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn set_flags(&mut self, user_id: i64, is_admin: bool, disabled: bool) -> Result<()> {
        let mut tx = self.client()?.transaction()?;
        // Two admins demoting each other at once must not both succeed.
        tx.batch_execute("lock table app_user in share row exclusive mode")?;
        let other_admins: i64 = tx
            .query_one(
                "select count(*) from app_user where is_admin and not disabled and id <> $1",
                &[&user_id],
            )?
            .get(0);
        if other_admins == 0 && (!is_admin || disabled) {
            bail!("There must be at least one admin who can sign in");
        }
        let changed = tx.execute(
            "update app_user set is_admin = $2, disabled = $3 where id = $1",
            &[&user_id, &is_admin, &disabled],
        )?;
        if changed == 0 {
            bail!("There is no such user");
        }
        if disabled {
            tx.execute("delete from user_session where user_id = $1", &[&user_id])?;
            tx.execute("delete from api_token where user_id = $1", &[&user_id])?;
        }
        tx.commit()?;
        Ok(())
    }

    fn create_session(
        &mut self,
        token_hash: &str,
        user_id: i64,
        expires_at: DateTime<Utc>,
        user_agent: &str,
    ) -> Result<()> {
        let client = self.client()?;
        client.execute(
            "insert into user_session (token_hash, user_id, expires_at, user_agent)
             values ($1, $2, $3, $4)",
            &[&token_hash, &user_id, &expires_at, &user_agent],
        )?;
        client.execute(
            "update app_user set last_seen_at = now() where id = $1",
            &[&user_id],
        )?;
        Ok(())
    }

    fn session_user(&mut self, token_hash: &str) -> Result<Option<(User, DateTime<Utc>)>> {
        Ok(self
            .client()?
            .query_opt(
                &format!(
                    "select {USER_COLUMNS}, s.last_seen_at
                     from user_session s join app_user u on u.id = s.user_id
                     where s.token_hash = $1 and s.expires_at > now() and not u.disabled"
                ),
                &[&token_hash],
            )?
            .map(|r| (user_from(&r, 0), r.get(7))))
    }

    fn touch_session(&mut self, token_hash: &str, expires_at: DateTime<Utc>) -> Result<()> {
        let client = self.client()?;
        client.execute(
            "update user_session set last_seen_at = now(), expires_at = $2 where token_hash = $1",
            &[&token_hash, &expires_at],
        )?;
        client.execute(
            "update app_user set last_seen_at = now()
             where id = (select user_id from user_session where token_hash = $1)",
            &[&token_hash],
        )?;
        Ok(())
    }

    fn delete_session(&mut self, token_hash: &str) -> Result<()> {
        self.client()?.execute(
            "delete from user_session where token_hash = $1",
            &[&token_hash],
        )?;
        Ok(())
    }

    fn create_link(
        &mut self,
        token_hash: &str,
        kind: LinkKind,
        user_id: Option<i64>,
        created_by: Option<i64>,
        expires_at: DateTime<Utc>,
    ) -> Result<()> {
        let client = self.client()?;
        // Expired and used links are of no further use.
        client.execute(
            "delete from account_link where expires_at <= now() or used_at is not null",
            &[],
        )?;
        client.execute(
            "insert into account_link (token_hash, kind, user_id, created_by, expires_at)
             values ($1, $2, $3, $4, $5)",
            &[
                &token_hash,
                &kind.as_str(),
                &user_id,
                &created_by,
                &expires_at,
            ],
        )?;
        Ok(())
    }

    fn link(&mut self, token_hash: &str) -> Result<Option<Link>> {
        let Some(row) = self.client()?.query_opt(
            "select kind, user_id, expires_at from account_link
             where token_hash = $1 and used_at is null and expires_at > now()",
            &[&token_hash],
        )?
        else {
            return Ok(None);
        };
        let kind: String = row.get(0);
        let user = match row.get::<_, Option<i64>>(1) {
            Some(id) => self.user(id)?,
            None => None,
        };
        Ok(Some(Link {
            kind: LinkKind::parse(&kind)?,
            user,
            expires_at: row.get(2),
        }))
    }

    fn create_api_token(&mut self, user_id: i64, name: &str, token_hash: &str) -> Result<i64> {
        Ok(self
            .client()?
            .query_one(
                "insert into api_token (user_id, name, token_hash) values ($1, $2, $3)
                 returning id",
                &[&user_id, &name, &token_hash],
            )?
            .get(0))
    }

    fn api_tokens(&mut self, user_id: i64) -> Result<Vec<ApiToken>> {
        Ok(self
            .client()?
            .query(
                "select id, name, created_at, last_used_at from api_token
                 where user_id = $1 order by created_at, id",
                &[&user_id],
            )?
            .iter()
            .map(|r| ApiToken {
                id: r.get(0),
                name: r.get(1),
                created_at: r.get(2),
                last_used_at: r.get(3),
            })
            .collect())
    }

    fn delete_api_token(&mut self, user_id: i64, id: i64) -> Result<bool> {
        Ok(self.client()?.execute(
            "delete from api_token where id = $1 and user_id = $2",
            &[&id, &user_id],
        )? > 0)
    }

    fn token_user(&mut self, token_hash: &str) -> Result<Option<User>> {
        let user_id: Option<i64> = self
            .client()?
            .query_opt(
                "update api_token set last_used_at = now() where token_hash = $1
                 returning user_id",
                &[&token_hash],
            )?
            .map(|r| r.get(0));
        match user_id {
            Some(id) => Ok(self.user(id)?.filter(|u| !u.disabled)),
            None => Ok(None),
        }
    }
}

impl PostgresStore {
    /// The account tables over this store's connection (the integration test).
    #[cfg(test)]
    pub(crate) fn accounts(self, cfg: &Config) -> Box<dyn AccountStore> {
        Box::new(PostgresAccounts {
            client: self.into_client(),
            cfg: cfg.clone(),
        })
    }
}
