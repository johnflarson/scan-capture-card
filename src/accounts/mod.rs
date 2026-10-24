//! Accounts (1.0): users with argon2id passwords, server-side sessions,
//! invite and password-reset links, and API tokens for scripts.
//!
//! There is no public sign-up. The first account is created on the setup
//! page with a code the server prints when it starts without accounts; it is
//! the admin. Others join through invite links the admin makes, and the
//! admin makes reset links for forgotten passwords (`mtg-scan user
//! reset-link` does the same from the command line, for a locked-out admin).
//!
//! Only hashes are stored: the SHA-256 of each session, link and API token,
//! and argon2id for passwords. [`Accounts`] is what the web server uses; it
//! keeps signed-in sessions in memory for a minute so most requests don't
//! touch the database.

mod postgres;
mod sqlite;

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use base64::Engine as _;
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};

use crate::config::{Config, StorageBackend};

/// How long a browser stays signed in without being used.
pub const SESSION_DAYS: i64 = 30;
pub const INVITE_DAYS: i64 = 7;
pub const RESET_HOURS: i64 = 24;
/// Sessions are re-read from the database after this long.
const CACHE_FOR: Duration = Duration::from_secs(60);
/// A session's expiry moves forward at most this often.
const TOUCH_EVERY: chrono::TimeDelta = chrono::TimeDelta::hours(1);
/// Failed sign-ins allowed per name, and per address, in [`FAILURE_WINDOW`].
const MAX_FAILURES_PER_NAME: usize = 5;
const MAX_FAILURES_PER_ADDRESS: usize = 20;
const FAILURE_WINDOW: Duration = Duration::from_secs(15 * 60);
pub const MIN_PASSWORD: usize = 8;
const MAX_PASSWORD: usize = 256;
/// API tokens start with this, so they are easy to recognise (and to find
/// if one is pasted somewhere it shouldn't be).
pub const API_TOKEN_PREFIX: &str = "tome_";

#[derive(Debug, Clone, PartialEq)]
pub struct User {
    pub id: i64,
    pub name: String,
    pub is_admin: bool,
    pub disabled: bool,
    pub has_password: bool,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    Invite,
    Reset,
}

impl LinkKind {
    fn as_str(self) -> &'static str {
        match self {
            LinkKind::Invite => "invite",
            LinkKind::Reset => "reset",
        }
    }

    fn parse(text: &str) -> Result<Self> {
        match text {
            "invite" => Ok(LinkKind::Invite),
            "reset" => Ok(LinkKind::Reset),
            other => bail!("unknown link kind {other:?}"),
        }
    }
}

/// An unused, unexpired link.
#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    pub kind: LinkKind,
    /// Reset: whose password.
    pub user: Option<User>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ApiToken {
    pub id: i64,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

/// The account tables, in SQLite or PostgreSQL. Every method that checks
/// and then changes runs in one transaction.
pub trait AccountStore: Send {
    /// No account can sign in yet: none has a password.
    fn needs_setup(&mut self) -> Result<bool>;
    fn users(&mut self) -> Result<Vec<User>>;
    fn user(&mut self, id: i64) -> Result<Option<User>>;
    /// The user and their password hash.
    fn user_by_name(&mut self, name: &str) -> Result<Option<(User, Option<String>)>>;
    /// Create the first account, an admin, while no account has a password.
    /// An admin without a password (made for a collection from before
    /// accounts) is claimed: named and given the password. `None` once an
    /// account has a password.
    fn create_first_admin(&mut self, name: &str, password_hash: &str) -> Result<Option<User>>;
    /// Use an invite link to create an account. `None` if the link is
    /// unknown, used or expired.
    fn redeem_invite(
        &mut self,
        token_hash: &str,
        name: &str,
        password_hash: &str,
    ) -> Result<Option<User>>;
    /// Use a reset link to set a password, signing the user out everywhere.
    /// `None` if the link is unknown, used or expired.
    fn redeem_reset(&mut self, token_hash: &str, password_hash: &str) -> Result<Option<User>>;
    /// Set a password and end the user's sessions except `keep`.
    fn set_password(&mut self, user_id: i64, password_hash: &str, keep: Option<&str>)
    -> Result<()>;
    /// Change the admin and disabled flags. Refuses to leave no enabled
    /// admin. Disabling ends the user's sessions and deletes their tokens.
    fn set_flags(&mut self, user_id: i64, is_admin: bool, disabled: bool) -> Result<()>;
    fn create_session(
        &mut self,
        token_hash: &str,
        user_id: i64,
        expires_at: DateTime<Utc>,
        user_agent: &str,
    ) -> Result<()>;
    /// The enabled user of an unexpired session, and when it was last used.
    fn session_user(&mut self, token_hash: &str) -> Result<Option<(User, DateTime<Utc>)>>;
    /// Mark a session used now and move its expiry.
    fn touch_session(&mut self, token_hash: &str, expires_at: DateTime<Utc>) -> Result<()>;
    fn delete_session(&mut self, token_hash: &str) -> Result<()>;
    fn create_link(
        &mut self,
        token_hash: &str,
        kind: LinkKind,
        user_id: Option<i64>,
        created_by: Option<i64>,
        expires_at: DateTime<Utc>,
    ) -> Result<()>;
    /// An unused, unexpired link.
    fn link(&mut self, token_hash: &str) -> Result<Option<Link>>;
    fn create_api_token(&mut self, user_id: i64, name: &str, token_hash: &str) -> Result<i64>;
    fn api_tokens(&mut self, user_id: i64) -> Result<Vec<ApiToken>>;
    /// Returns whether the token existed (and was the user's).
    fn delete_api_token(&mut self, user_id: i64, id: i64) -> Result<bool>;
    /// The enabled user an API token belongs to; marks the token used.
    fn token_user(&mut self, token_hash: &str) -> Result<Option<User>>;
}

/// Open the account tables of the configured database (migrating SQLite).
pub fn open_store(cfg: &Config) -> Result<Box<dyn AccountStore>> {
    match cfg.storage.backend {
        StorageBackend::Sqlite => Ok(Box::new(sqlite::SqliteAccounts::open(
            &cfg.storage.sqlite_path(),
        )?)),
        StorageBackend::Postgres => Ok(Box::new(postgres::PostgresAccounts::connect(cfg)?)),
        StorageBackend::Markdown => bail!(
            "accounts need a database: set [storage] backend = \"sqlite\" (the default) or \
             \"postgres\""
        ),
    }
}

/// A new random secret: 32 bytes, URL-safe base64 (43 characters).
pub fn new_secret() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| anyhow!("no secure random source: {e}"))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

/// How a secret is stored: SHA-256, hex. The secrets are random, so a fast
/// hash is enough (unlike passwords).
pub fn secret_hash(secret: &str) -> String {
    Sha256::digest(secret.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn hash_password(password: &str) -> Result<String> {
    let mut salt = [0u8; 16];
    getrandom::fill(&mut salt).map_err(|e| anyhow!("no secure random source: {e}"))?;
    let salt = SaltString::encode_b64(&salt).map_err(|e| anyhow!("bad salt: {e}"))?;
    // argon2id, 19 MiB, 2 passes (the OWASP recommendation).
    Ok(Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow!("could not hash the password: {e}"))?
        .to_string())
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash).is_ok_and(|parsed| {
        Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok()
    })
}

/// Sign-in names: 1 to 40 letters, digits, spaces, `.`, `-` and `_`, not
/// starting or ending with a space.
pub fn check_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.chars().count() <= 40
        && name.trim() == name
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '.' | '-' | '_'));
    if !ok {
        bail!("Use 1 to 40 letters, digits, spaces, dots, dashes or underscores for the name");
    }
    Ok(())
}

pub fn check_password(password: &str) -> Result<()> {
    let n = password.chars().count();
    if n < MIN_PASSWORD {
        bail!("Use at least {MIN_PASSWORD} characters for the password");
    }
    if n > MAX_PASSWORD {
        bail!("Use at most {MAX_PASSWORD} characters for the password");
    }
    Ok(())
}

/// A signed-in browser or script.
#[derive(Debug, Clone, PartialEq)]
pub struct Signed {
    pub user: User,
    /// The session's hash, for signing it out (none for API tokens).
    pub session: Option<String>,
}

/// Why signing in failed.
#[derive(Debug, PartialEq)]
pub enum SignInError {
    Wrong,
    /// Too many failures lately for this name or address.
    TooMany,
    Other(String),
}

/// What the web server uses. Every method blocks (database, argon2).
pub struct Accounts {
    db: Mutex<Box<dyn AccountStore>>,
    /// Session hash -> user, and when it was read.
    cache: Mutex<HashMap<String, (User, Instant)>>,
    /// Failed sign-ins per "name:<name>" and "ip:<address>".
    failures: Mutex<HashMap<String, Vec<Instant>>>,
    /// The code the setup page needs, while there is no account.
    setup_code: Mutex<Option<String>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Accounts {
    pub fn new(store: Box<dyn AccountStore>) -> Result<Self> {
        let accounts = Self {
            db: Mutex::new(store),
            cache: Mutex::new(HashMap::new()),
            failures: Mutex::new(HashMap::new()),
            setup_code: Mutex::new(None),
        };
        if accounts.db().needs_setup()? {
            *lock(&accounts.setup_code) = Some(new_secret()?);
        }
        Ok(accounts)
    }

    fn db(&self) -> MutexGuard<'_, Box<dyn AccountStore>> {
        lock(&self.db)
    }

    /// The setup code, while no account exists.
    pub fn setup_code(&self) -> Option<String> {
        lock(&self.setup_code).clone()
    }

    /// Create the first account (an admin) with the setup code.
    pub fn set_up(&self, code: &str, name: &str, password: &str) -> Result<User> {
        let expected = self
            .setup_code()
            .ok_or_else(|| anyhow!("This server already has an admin. Sign in instead."))?;
        if !same(code.trim(), &expected) {
            bail!("That setup code isn't right. It's printed where the server was started.");
        }
        check_name(name)?;
        check_password(password)?;
        let hash = hash_password(password)?;
        let user = self
            .db()
            .create_first_admin(name, &hash)?
            .ok_or_else(|| anyhow!("This server already has an admin. Sign in instead."))?;
        *lock(&self.setup_code) = None;
        Ok(user)
    }

    /// Check a name and password. Failures are counted per name and per
    /// address, and refused for a while once there are too many.
    pub fn sign_in(&self, name: &str, password: &str, from: IpAddr) -> Result<User, SignInError> {
        let keys = [
            format!("name:{}", name.trim().to_lowercase()),
            format!("ip:{}", from.to_canonical()),
        ];
        if self.too_many_failures(&keys) {
            return Err(SignInError::TooMany);
        }
        let found = self
            .db()
            .user_by_name(name.trim())
            .map_err(|e| SignInError::Other(format!("{e:#}")))?;
        let ok = match &found {
            Some((user, Some(hash))) => verify_password(password, hash) && !user.disabled,
            _ => {
                // Spend the same time as a real check, so timing doesn't
                // tell which names exist.
                let _ = verify_password(password, DUMMY_HASH);
                false
            }
        };
        if !ok {
            self.record_failure(&keys);
            return Err(SignInError::Wrong);
        }
        Ok(found.map(|(user, _)| user).expect("checked above"))
    }

    fn too_many_failures(&self, keys: &[String; 2]) -> bool {
        let mut failures = lock(&self.failures);
        let now = Instant::now();
        failures.retain(|_, times| {
            times.retain(|t| now.duration_since(*t) < FAILURE_WINDOW);
            !times.is_empty()
        });
        let count = |key: &String| failures.get(key).map_or(0, Vec::len);
        count(&keys[0]) >= MAX_FAILURES_PER_NAME || count(&keys[1]) >= MAX_FAILURES_PER_ADDRESS
    }

    fn record_failure(&self, keys: &[String; 2]) {
        let mut failures = lock(&self.failures);
        for key in keys {
            failures
                .entry(key.clone())
                .or_default()
                .push(Instant::now());
        }
    }

    /// Start a session; returns the secret for the cookie.
    pub fn start_session(&self, user: &User, user_agent: &str) -> Result<String> {
        let secret = new_secret()?;
        let agent: String = user_agent.chars().take(200).collect();
        self.db().create_session(
            &secret_hash(&secret),
            user.id,
            Utc::now() + chrono::TimeDelta::days(SESSION_DAYS),
            &agent,
        )?;
        Ok(secret)
    }

    /// The user signed in with this session cookie.
    pub fn session(&self, secret: &str) -> Result<Option<Signed>> {
        let hash = secret_hash(secret);
        if let Some((user, read)) = lock(&self.cache).get(&hash)
            && read.elapsed() < CACHE_FOR
        {
            return Ok(Some(Signed {
                user: user.clone(),
                session: Some(hash),
            }));
        }
        let mut db = self.db();
        let Some((user, last_seen)) = db.session_user(&hash)? else {
            lock(&self.cache).remove(&hash);
            return Ok(None);
        };
        if Utc::now() - last_seen > TOUCH_EVERY {
            db.touch_session(&hash, Utc::now() + chrono::TimeDelta::days(SESSION_DAYS))?;
        }
        drop(db);
        lock(&self.cache).insert(hash.clone(), (user.clone(), Instant::now()));
        Ok(Some(Signed {
            user,
            session: Some(hash),
        }))
    }

    /// The user an API token belongs to.
    pub fn api_token(&self, token: &str) -> Result<Option<Signed>> {
        if !token.starts_with(API_TOKEN_PREFIX) {
            return Ok(None);
        }
        Ok(self
            .db()
            .token_user(&secret_hash(token))?
            .map(|user| Signed {
                user,
                session: None,
            }))
    }

    pub fn sign_out(&self, session_hash: &str) -> Result<()> {
        lock(&self.cache).remove(session_hash);
        self.db().delete_session(session_hash)
    }

    /// Forget every cached session (after a change to who may sign in).
    fn forget_sessions(&self) {
        lock(&self.cache).clear();
    }

    pub fn change_password(
        &self,
        user: &User,
        current: &str,
        new: &str,
        keep_session: Option<&str>,
    ) -> Result<()> {
        let hash = self.db().user_by_name(&user.name)?.and_then(|(_, h)| h);
        if !hash.is_some_and(|h| verify_password(current, &h)) {
            bail!("The current password isn't right");
        }
        check_password(new)?;
        let new_hash = hash_password(new)?;
        self.db().set_password(user.id, &new_hash, keep_session)?;
        self.forget_sessions();
        Ok(())
    }

    pub fn users(&self) -> Result<Vec<User>> {
        self.db().users()
    }

    pub fn user(&self, id: i64) -> Result<Option<User>> {
        self.db().user(id)
    }

    /// The account with this sign-in name (any case).
    pub fn find(&self, name: &str) -> Result<Option<User>> {
        Ok(self.db().user_by_name(name.trim())?.map(|(user, _)| user))
    }

    pub fn set_flags(&self, id: i64, is_admin: bool, disabled: bool) -> Result<()> {
        self.db().set_flags(id, is_admin, disabled)?;
        self.forget_sessions();
        Ok(())
    }

    /// A new invite link's code.
    pub fn invite(&self, by: &User) -> Result<(String, DateTime<Utc>)> {
        let code = new_secret()?;
        let expires = Utc::now() + chrono::TimeDelta::days(INVITE_DAYS);
        self.db().create_link(
            &secret_hash(&code),
            LinkKind::Invite,
            None,
            Some(by.id),
            expires,
        )?;
        Ok((code, expires))
    }

    /// A new password-reset link's code for `user_id`.
    pub fn reset_link(&self, user_id: i64, by: Option<&User>) -> Result<(String, DateTime<Utc>)> {
        let code = new_secret()?;
        let expires = Utc::now() + chrono::TimeDelta::hours(RESET_HOURS);
        self.db().create_link(
            &secret_hash(&code),
            LinkKind::Reset,
            Some(user_id),
            by.map(|u| u.id),
            expires,
        )?;
        Ok((code, expires))
    }

    pub fn link(&self, code: &str) -> Result<Option<Link>> {
        self.db().link(&secret_hash(code.trim()))
    }

    /// Create an account with an invite link, or set a password with a
    /// reset link. `name` is only used by invites.
    pub fn redeem(&self, code: &str, name: &str, password: &str) -> Result<User> {
        const GONE: &str =
            "This link was already used or has expired. Ask the admin for a new one.";
        let hash = secret_hash(code.trim());
        let link = self.db().link(&hash)?.ok_or_else(|| anyhow!(GONE))?;
        check_password(password)?;
        let password_hash = hash_password(password)?;
        let user = match link.kind {
            LinkKind::Invite => {
                check_name(name)?;
                if self.db().user_by_name(name)?.is_some() {
                    bail!("The name {name} is taken; choose another");
                }
                self.db().redeem_invite(&hash, name, &password_hash)?
            }
            LinkKind::Reset => self.db().redeem_reset(&hash, &password_hash)?,
        };
        let user = user.ok_or_else(|| anyhow!(GONE))?;
        self.forget_sessions();
        Ok(user)
    }

    /// A new API token for `user`; the token is only ever shown now.
    pub fn new_api_token(&self, user: &User, name: &str) -> Result<(ApiToken, String)> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 60 {
            bail!("Name the token (up to 60 characters), e.g. what will use it");
        }
        let token = format!("{API_TOKEN_PREFIX}{}", new_secret()?);
        let mut db = self.db();
        let id = db.create_api_token(user.id, name, &secret_hash(&token))?;
        let created = db
            .api_tokens(user.id)?
            .into_iter()
            .find(|t| t.id == id)
            .context("the new token wasn't saved")?;
        Ok((created, token))
    }

    pub fn api_tokens(&self, user: &User) -> Result<Vec<ApiToken>> {
        self.db().api_tokens(user.id)
    }

    pub fn delete_api_token(&self, user: &User, id: i64) -> Result<bool> {
        self.db().delete_api_token(user.id, id)
    }
}

/// An argon2id hash of a random password, for timing failed sign-ins of
/// names that don't exist.
const DUMMY_HASH: &str =
    "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHRzYWx0$Wj3qL9Zk0nJ0xBq1i0vZ3L4e1s6zq0vC6r4v2bq0c8U";

/// Same length and same bytes, without stopping at the first difference.
pub fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

/// `YYYY-MM-DDTHH:MM:SSZ`, the text form SQLite stores (it sorts in time
/// order, so SQL can compare it).
pub(crate) fn time_text(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

pub(crate) fn parse_time(text: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(text)
        .with_context(|| format!("bad timestamp {text:?}"))?
        .with_timezone(&Utc))
}

/// Behaviour both account stores must share (in-memory SQLite in every
/// `cargo test`, PostgreSQL in the integration test).
#[cfg(test)]
pub(crate) fn conformance(store: Box<dyn AccountStore>) {
    let accounts = Accounts::new(store).unwrap();
    let code = accounts.setup_code().expect("no accounts yet");
    let here: IpAddr = "127.0.0.1".parse().unwrap();

    // Setup needs the code, then makes the first account an admin.
    assert!(accounts.set_up("wrong", "Ada", "correct horse").is_err());
    let ada = accounts.set_up(&code, "Ada", "correct horse").unwrap();
    assert!(ada.is_admin && ada.has_password);
    assert!(accounts.setup_code().is_none());
    assert!(accounts.set_up(&code, "Eve", "correct horse").is_err());

    // Sign-in is case-insensitive in the name, exact in the password.
    assert_eq!(
        accounts.sign_in("ada", "correct horse", here).unwrap().id,
        ada.id
    );
    assert_eq!(
        accounts.sign_in("Ada", "Correct horse", here),
        Err(SignInError::Wrong)
    );
    assert_eq!(
        accounts.sign_in("nobody", "x", here),
        Err(SignInError::Wrong)
    );

    // Sessions.
    let secret = accounts.start_session(&ada, "test").unwrap();
    let signed = accounts.session(&secret).unwrap().unwrap();
    assert_eq!(signed.user.name, "Ada");
    assert!(accounts.session("not-a-session").unwrap().is_none());

    // An invite makes a regular account, once.
    let (invite, _) = accounts.invite(&ada).unwrap();
    assert_eq!(
        accounts.link(&invite).unwrap().unwrap().kind,
        LinkKind::Invite
    );
    assert!(
        accounts.redeem(&invite, "ada", "another pass").is_err(),
        "name taken"
    );
    assert!(accounts.redeem(&invite, "Bob", "short").is_err());
    let bob = accounts.redeem(&invite, "Bob", "bobs password").unwrap();
    assert!(!bob.is_admin);
    assert!(accounts.link(&invite).unwrap().is_none(), "used");
    assert!(
        accounts
            .redeem(&invite, "Carol", "carols password")
            .is_err()
    );

    // A reset link sets a new password and signs the user out everywhere.
    let bob_session = accounts.start_session(&bob, "test").unwrap();
    let (reset, _) = accounts.reset_link(bob.id, Some(&ada)).unwrap();
    let link = accounts.link(&reset).unwrap().unwrap();
    assert_eq!(link.user.as_ref().map(|u| u.id), Some(bob.id));
    accounts.redeem(&reset, "", "new bob password").unwrap();
    assert!(accounts.session(&bob_session).unwrap().is_none());
    assert!(accounts.sign_in("bob", "new bob password", here).is_ok());

    // Changing one's own password keeps the current session only.
    let keep = accounts.start_session(&bob, "a").unwrap();
    let other = accounts.start_session(&bob, "b").unwrap();
    let keep_hash = secret_hash(&keep);
    assert!(
        accounts
            .change_password(&bob, "wrong", "newer password", Some(&keep_hash))
            .is_err()
    );
    accounts
        .change_password(&bob, "new bob password", "newer password", Some(&keep_hash))
        .unwrap();
    assert!(accounts.session(&keep).unwrap().is_some());
    assert!(accounts.session(&other).unwrap().is_none());

    // API tokens.
    let (token, secret_token) = accounts.new_api_token(&bob, "price script").unwrap();
    assert!(secret_token.starts_with(API_TOKEN_PREFIX));
    assert_eq!(
        accounts.api_token(&secret_token).unwrap().unwrap().user.id,
        bob.id
    );
    assert_eq!(accounts.api_tokens(&bob).unwrap().len(), 1);
    assert!(
        !accounts.delete_api_token(&ada, token.id).unwrap(),
        "only the owner can revoke it"
    );

    // The last admin can't be demoted or disabled; disabling signs out
    // and revokes tokens.
    assert!(accounts.set_flags(ada.id, false, false).is_err());
    assert!(accounts.set_flags(ada.id, true, true).is_err());
    accounts.set_flags(bob.id, false, true).unwrap();
    assert!(accounts.session(&keep).unwrap().is_none());
    assert!(accounts.api_token(&secret_token).unwrap().is_none());
    assert_eq!(
        accounts.sign_in("bob", "newer password", here),
        Err(SignInError::Wrong)
    );
    accounts.set_flags(bob.id, true, false).unwrap();
    accounts.set_flags(ada.id, false, false).unwrap();
    // (The database may hold other accounts, e.g. from the store tests.)
    let users = accounts.users().unwrap();
    assert!(users.iter().any(|u| u.id == ada.id && !u.is_admin));
    assert!(users.iter().any(|u| u.id == bob.id && u.is_admin));

    // Sign out.
    accounts.sign_out(&secret_hash(&secret)).unwrap();
    assert!(accounts.session(&secret).unwrap().is_none());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_claims_the_admin_made_for_an_old_collection() {
        let store = crate::store::sqlite::SqliteStore::open_in_memory().unwrap();
        // A collection from before accounts: its rows belong to a password-less admin.
        use crate::store::CollectionStore;
        let owner = store.user().unwrap();
        let accounts = Accounts::new(Box::new(sqlite::SqliteAccounts::from_store(store))).unwrap();
        let code = accounts.setup_code().expect("nobody can sign in yet");
        let ada = accounts.set_up(&code, "Ada", "correct horse").unwrap();
        assert_eq!(ada.id, owner, "the collection's owner is now Ada");
        assert!(ada.is_admin && ada.has_password);
        assert_eq!(accounts.users().unwrap().len(), 1);
    }

    #[test]
    fn passwords_hash_with_argon2id_and_verify() {
        let hash = hash_password("correct horse").unwrap();
        assert!(hash.starts_with("$argon2id$"));
        assert!(verify_password("correct horse", &hash));
        assert!(!verify_password("correct horsE", &hash));
        assert!(!verify_password("x", "not a hash"));
        assert_ne!(hash, hash_password("correct horse").unwrap(), "salted");
        assert!(PasswordHash::new(DUMMY_HASH).is_ok());
    }

    #[test]
    fn secrets_are_random_and_hashed() {
        let a = new_secret().unwrap();
        assert_eq!(a.len(), 43);
        assert_ne!(a, new_secret().unwrap());
        assert_eq!(secret_hash("abc").len(), 64);
        assert_eq!(secret_hash("abc"), secret_hash("abc"));
    }

    #[test]
    fn names_and_passwords_are_checked() {
        for ok in ["Ada", "ada lovelace", "j.larson", "x_1-2", "Zoë"] {
            assert!(check_name(ok).is_ok(), "{ok}");
        }
        for bad in ["", " ada", "ada ", "a<b", "a/b", &"x".repeat(41)] {
            assert!(check_name(bad).is_err(), "{bad}");
        }
        assert!(check_password("1234567").is_err());
        assert!(check_password("12345678").is_ok());
    }

    #[test]
    fn repeated_failures_are_refused_for_a_while() {
        let accounts =
            Accounts::new(Box::new(sqlite::SqliteAccounts::in_memory().unwrap())).unwrap();
        let code = accounts.setup_code().unwrap();
        accounts.set_up(&code, "Ada", "correct horse").unwrap();
        let here: IpAddr = "192.168.1.5".parse().unwrap();
        for _ in 0..MAX_FAILURES_PER_NAME {
            assert_eq!(
                accounts.sign_in("Ada", "nope", here),
                Err(SignInError::Wrong)
            );
        }
        assert_eq!(
            accounts.sign_in("Ada", "correct horse", here),
            Err(SignInError::TooMany),
            "even the right password waits"
        );
    }

    #[test]
    fn sqlite_accounts_behave() {
        conformance(Box::new(sqlite::SqliteAccounts::in_memory().unwrap()));
    }

    #[test]
    fn times_round_trip_as_sortable_text() {
        let t = parse_time("2026-10-24T12:00:00Z").unwrap();
        assert_eq!(time_text(t), "2026-10-24T12:00:00Z");
        assert!(time_text(t) < time_text(t + chrono::TimeDelta::seconds(1)));
    }
}
