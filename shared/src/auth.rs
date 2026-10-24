//! Accounts: signing in (`/api/auth`), one's own account (`/api/account`)
//! and, for admins, the users (`/api/users`). Passwords only ever travel
//! from the browser to the server; API tokens and link codes are shown once.

use serde::{Deserialize, Serialize};

/// The signed-in user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Me {
    pub id: i64,
    pub name: String,
    pub is_admin: bool,
}

/// `GET /api/auth`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthStatus {
    pub user: Option<Me>,
    /// No account exists yet: the setup page creates the admin.
    pub setup_needed: bool,
}

/// `POST /api/auth/setup`: the first account, with the code the server
/// printed when it started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetUp {
    pub code: String,
    pub name: String,
    pub password: String,
}

/// `POST /api/auth/sign-in`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignIn {
    pub name: String,
    pub password: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkKind {
    /// Creates an account.
    Invite,
    /// Sets a forgotten password.
    Reset,
}

/// `GET /api/auth/link?code=`: what an unused link is for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkInfo {
    pub kind: LinkKind,
    /// Reset: whose password.
    pub name: Option<String>,
    /// RFC 3339.
    pub expires: String,
}

/// `POST /api/auth/link`: use an invite (`name` and `password`) or a reset
/// link (`password`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Redeem {
    pub code: String,
    #[serde(default)]
    pub name: String,
    pub password: String,
}

/// `POST /api/account/password`. Signs out every other browser.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PasswordChange {
    pub current: String,
    pub new: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiTokenView {
    pub id: i64,
    pub name: String,
    /// RFC 3339.
    pub created: String,
    pub last_used: Option<String>,
}

/// `POST /api/account/tokens`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewApiToken {
    pub name: String,
}

/// A new API token; `secret` is never shown again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatedApiToken {
    pub token: ApiTokenView,
    pub secret: String,
}

/// A user, as admins see them (`GET /api/users`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserView {
    pub id: i64,
    pub name: String,
    pub is_admin: bool,
    pub disabled: bool,
    /// False for an account whose password hasn't been set yet.
    pub has_password: bool,
    /// RFC 3339.
    pub created: String,
    pub last_seen: Option<String>,
}

/// `POST /api/users/{id}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserChange {
    pub is_admin: bool,
    pub disabled: bool,
}

/// A new invite or reset link: the page adds its own address in front of
/// `path`, so the link works however this server is reached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkView {
    /// E.g. `/join?code=...`.
    pub path: String,
    /// RFC 3339.
    pub expires: String,
}
