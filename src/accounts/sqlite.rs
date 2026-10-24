//! The account tables in SQLite (`migrations/sqlite/0004_accounts.sql`).

use std::path::Path;

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};

use super::{AccountStore, ApiToken, Link, LinkKind, User, parse_time, time_text};
use crate::store::sqlite::SqliteStore;

pub struct SqliteAccounts {
    conn: Connection,
}

/// `app_user` columns in the order [`user_from`] reads them.
const USER_COLUMNS: &str = "u.id, u.name, u.is_admin, u.disabled, u.password_hash is not null,
    u.created_at, u.last_seen_at";

/// A user as stored: id, name, admin, disabled, has a password, created,
/// last seen.
type RawUser = (i64, String, bool, bool, bool, String, Option<String>);

fn user_from(r: &Row<'_>, at: usize) -> rusqlite::Result<RawUser> {
    Ok((
        r.get(at)?,
        r.get(at + 1)?,
        r.get(at + 2)?,
        r.get(at + 3)?,
        r.get(at + 4)?,
        r.get(at + 5)?,
        r.get(at + 6)?,
    ))
}

fn user((id, name, is_admin, disabled, has_password, created, seen): RawUser) -> Result<User> {
    Ok(User {
        id,
        name,
        is_admin,
        disabled,
        has_password,
        created_at: parse_time(&created)?,
        last_seen_at: seen.as_deref().map(parse_time).transpose()?,
    })
}

fn now() -> String {
    time_text(Utc::now())
}

impl SqliteAccounts {
    /// Open (and migrate) the database file.
    pub fn open(path: &Path) -> Result<Self> {
        Ok(Self {
            conn: SqliteStore::open(path, None)?.into_connection(),
        })
    }

    #[cfg(test)]
    pub fn in_memory() -> Result<Self> {
        Ok(Self::from_store(SqliteStore::open_in_memory()?))
    }

    #[cfg(test)]
    pub fn from_store(store: SqliteStore) -> Self {
        Self {
            conn: store.into_connection(),
        }
    }

    fn one_user(&self, sql_where: &str, param: &dyn rusqlite::ToSql) -> Result<Option<User>> {
        let raw = self
            .conn
            .query_row(
                &format!("select {USER_COLUMNS} from app_user u where {sql_where}"),
                [param],
                |r| user_from(r, 0),
            )
            .optional()?;
        raw.map(user).transpose()
    }
}

impl AccountStore for SqliteAccounts {
    fn needs_setup(&mut self) -> Result<bool> {
        Ok(self.conn.query_row(
            "select not exists (select 1 from app_user where password_hash is not null)",
            [],
            |r| r.get(0),
        )?)
    }

    fn users(&mut self) -> Result<Vec<User>> {
        let mut stmt = self.conn.prepare(&format!(
            "select {USER_COLUMNS} from app_user u order by u.name collate nocase"
        ))?;
        let rows = stmt.query_map([], |r| user_from(r, 0))?;
        rows.map(|r| user(r?)).collect()
    }

    fn user(&mut self, id: i64) -> Result<Option<User>> {
        self.one_user("u.id = ?1", &id)
    }

    fn user_by_name(&mut self, name: &str) -> Result<Option<(User, Option<String>)>> {
        let raw = self
            .conn
            .query_row(
                &format!(
                    "select {USER_COLUMNS}, u.password_hash from app_user u where u.name = ?1"
                ),
                [name],
                |r| Ok((user_from(r, 0)?, r.get::<_, Option<String>>(7)?)),
            )
            .optional()?;
        raw.map(|(u, hash)| Ok((user(u)?, hash))).transpose()
    }

    fn create_first_admin(&mut self, name: &str, password_hash: &str) -> Result<Option<User>> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let ready: bool = tx.query_row(
            "select exists (select 1 from app_user where password_hash is not null)",
            [],
            |r| r.get(0),
        )?;
        if ready {
            return Ok(None);
        }
        let waiting: Option<i64> =
            tx.query_row("select min(id) from app_user where is_admin", [], |r| {
                r.get(0)
            })?;
        let id = match waiting {
            Some(id) => {
                tx.execute(
                    "update app_user set name = ?2, password_hash = ?3, disabled = 0 where id = ?1",
                    params![id, name, password_hash],
                )?;
                id
            }
            None => {
                tx.execute(
                    "insert into app_user (name, password_hash, is_admin, created_at)
                     values (?1, ?2, 1, ?3)",
                    params![name, password_hash, now()],
                )?;
                tx.last_insert_rowid()
            }
        };
        tx.commit()?;
        self.user(id)
    }

    fn redeem_invite(
        &mut self,
        token_hash: &str,
        name: &str,
        password_hash: &str,
    ) -> Result<Option<User>> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let used = tx.execute(
            "update account_link set used_at = ?2
             where token_hash = ?1 and kind = 'invite' and used_at is null and expires_at > ?2",
            params![token_hash, now()],
        )?;
        if used == 0 {
            return Ok(None);
        }
        tx.execute(
            "insert into app_user (name, password_hash, is_admin, created_at)
             values (?1, ?2, 0, ?3)",
            params![name, password_hash, now()],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        self.user(id)
    }

    fn redeem_reset(&mut self, token_hash: &str, password_hash: &str) -> Result<Option<User>> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let user_id: Option<i64> = tx
            .query_row(
                "update account_link set used_at = ?2
                 where token_hash = ?1 and kind = 'reset' and used_at is null and expires_at > ?2
                 returning user_id",
                params![token_hash, now()],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        let Some(user_id) = user_id else {
            return Ok(None);
        };
        tx.execute(
            "update app_user set password_hash = ?2 where id = ?1",
            params![user_id, password_hash],
        )?;
        tx.execute("delete from user_session where user_id = ?1", [user_id])?;
        tx.commit()?;
        self.user(user_id)
    }

    fn set_password(
        &mut self,
        user_id: i64,
        password_hash: &str,
        keep: Option<&str>,
    ) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "update app_user set password_hash = ?2 where id = ?1",
            params![user_id, password_hash],
        )?;
        tx.execute(
            "delete from user_session where user_id = ?1 and token_hash is not ?2",
            params![user_id, keep],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn set_flags(&mut self, user_id: i64, is_admin: bool, disabled: bool) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let other_admins: i64 = tx.query_row(
            "select count(*) from app_user where is_admin and not disabled and id <> ?1",
            [user_id],
            |r| r.get(0),
        )?;
        if other_admins == 0 && (!is_admin || disabled) {
            bail!("There must be at least one admin who can sign in");
        }
        let changed = tx.execute(
            "update app_user set is_admin = ?2, disabled = ?3 where id = ?1",
            params![user_id, is_admin, disabled],
        )?;
        if changed == 0 {
            bail!("There is no such user");
        }
        if disabled {
            tx.execute("delete from user_session where user_id = ?1", [user_id])?;
            tx.execute("delete from api_token where user_id = ?1", [user_id])?;
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
        let now = now();
        self.conn.execute(
            "insert into user_session (token_hash, user_id, created_at, last_seen_at, expires_at,
               user_agent)
             values (?1, ?2, ?3, ?3, ?4, ?5)",
            params![token_hash, user_id, now, time_text(expires_at), user_agent],
        )?;
        self.conn.execute(
            "update app_user set last_seen_at = ?2 where id = ?1",
            params![user_id, now],
        )?;
        Ok(())
    }

    fn session_user(&mut self, token_hash: &str) -> Result<Option<(User, DateTime<Utc>)>> {
        let raw = self
            .conn
            .query_row(
                &format!(
                    "select {USER_COLUMNS}, s.last_seen_at
                     from user_session s join app_user u on u.id = s.user_id
                     where s.token_hash = ?1 and s.expires_at > ?2 and not u.disabled"
                ),
                params![token_hash, now()],
                |r| Ok((user_from(r, 0)?, r.get::<_, String>(7)?)),
            )
            .optional()?;
        raw.map(|(u, seen)| Ok((user(u)?, parse_time(&seen)?)))
            .transpose()
    }

    fn touch_session(&mut self, token_hash: &str, expires_at: DateTime<Utc>) -> Result<()> {
        let now = now();
        self.conn.execute(
            "update user_session set last_seen_at = ?2, expires_at = ?3 where token_hash = ?1",
            params![token_hash, now, time_text(expires_at)],
        )?;
        self.conn.execute(
            "update app_user set last_seen_at = ?2
             where id = (select user_id from user_session where token_hash = ?1)",
            params![token_hash, now],
        )?;
        Ok(())
    }

    fn delete_session(&mut self, token_hash: &str) -> Result<()> {
        self.conn.execute(
            "delete from user_session where token_hash = ?1",
            [token_hash],
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
        // Expired and used links are of no further use.
        self.conn.execute(
            "delete from account_link where expires_at <= ?1 or used_at is not null",
            [now()],
        )?;
        self.conn.execute(
            "insert into account_link (token_hash, kind, user_id, created_by, created_at,
               expires_at)
             values (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                token_hash,
                kind.as_str(),
                user_id,
                created_by,
                now(),
                time_text(expires_at)
            ],
        )?;
        Ok(())
    }

    fn link(&mut self, token_hash: &str) -> Result<Option<Link>> {
        let raw: Option<(String, Option<i64>, String)> = self
            .conn
            .query_row(
                "select kind, user_id, expires_at from account_link
                 where token_hash = ?1 and used_at is null and expires_at > ?2",
                params![token_hash, now()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((kind, user_id, expires)) = raw else {
            return Ok(None);
        };
        let user = match user_id {
            Some(id) => self.user(id)?,
            None => None,
        };
        Ok(Some(Link {
            kind: LinkKind::parse(&kind)?,
            user,
            expires_at: parse_time(&expires)?,
        }))
    }

    fn create_api_token(&mut self, user_id: i64, name: &str, token_hash: &str) -> Result<i64> {
        self.conn.execute(
            "insert into api_token (user_id, name, token_hash, created_at)
             values (?1, ?2, ?3, ?4)",
            params![user_id, name, token_hash, now()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    fn api_tokens(&mut self, user_id: i64) -> Result<Vec<ApiToken>> {
        let mut stmt = self.conn.prepare(
            "select id, name, created_at, last_used_at from api_token
             where user_id = ?1 order by created_at, id",
        )?;
        let rows = stmt.query_map([user_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })?;
        rows.map(|r| {
            let (id, name, created, used) = r?;
            Ok(ApiToken {
                id,
                name,
                created_at: parse_time(&created)?,
                last_used_at: used.as_deref().map(parse_time).transpose()?,
            })
        })
        .collect()
    }

    fn delete_api_token(&mut self, user_id: i64, id: i64) -> Result<bool> {
        Ok(self.conn.execute(
            "delete from api_token where id = ?1 and user_id = ?2",
            params![id, user_id],
        )? > 0)
    }

    fn token_user(&mut self, token_hash: &str) -> Result<Option<User>> {
        let user_id: Option<i64> = self
            .conn
            .query_row(
                "update api_token set last_used_at = ?2 where token_hash = ?1 returning user_id",
                params![token_hash, now()],
                |r| r.get(0),
            )
            .optional()?;
        match user_id {
            Some(id) => Ok(self.user(id)?.filter(|u| !u.disabled)),
            None => Ok(None),
        }
    }
}
