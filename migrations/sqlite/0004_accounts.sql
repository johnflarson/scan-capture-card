-- Accounts (1.0): the same tables as migrations/postgres/0004_accounts.sql.
-- Secrets are never stored: sessions, links and API tokens are kept as the
-- SHA-256 of the random value the browser or script holds, passwords as
-- argon2id hashes. Timestamps are UTC RFC 3339 text.

create table app_user (
  id            integer primary key,
  -- Sign-in name, unique regardless of case.
  name          text    not null collate nocase unique,
  -- argon2id PHC string; null until a password is set.
  password_hash text,
  is_admin      integer not null default 0,
  -- Can't sign in; sessions and tokens were revoked.
  disabled      integer not null default 0,
  created_at    text    not null,
  last_seen_at  text
);

-- A signed-in browser.
create table user_session (
  token_hash   text    primary key,
  user_id      integer not null references app_user (id) on delete cascade,
  created_at   text    not null,
  last_seen_at text    not null,
  expires_at   text    not null,
  user_agent   text    not null default ''
);

create index user_session_user on user_session (user_id);

-- One-time links: an invite creates an account, a reset sets a password.
create table account_link (
  token_hash  text    primary key,
  kind        text    not null check (kind in ('invite', 'reset')),
  -- Reset: whose password. Invite: null.
  user_id     integer references app_user (id) on delete cascade,
  created_by  integer references app_user (id) on delete set null,
  created_at  text    not null,
  expires_at  text    not null,
  used_at     text
);

-- Tokens for scripts (`Authorization: Bearer ...`).
create table api_token (
  id           integer primary key,
  user_id      integer not null references app_user (id) on delete cascade,
  name         text    not null,
  token_hash   text    not null unique,
  created_at   text    not null,
  last_used_at text
);

create index api_token_user on api_token (user_id);
