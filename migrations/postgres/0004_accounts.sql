-- Accounts (1.0). Secrets are never stored: sessions, links and API tokens
-- are kept as the SHA-256 of the random value the browser or script holds,
-- passwords as argon2id hashes.

create table app_user (
  id            bigint generated always as identity primary key,
  -- Sign-in name, unique regardless of case.
  name          text        not null,
  -- argon2id PHC string; null until a password is set.
  password_hash text,
  is_admin      boolean     not null default false,
  -- Can't sign in; sessions and tokens were revoked.
  disabled      boolean     not null default false,
  created_at    timestamptz not null default now(),
  last_seen_at  timestamptz
);

create unique index app_user_name on app_user (lower(name));

-- A signed-in browser.
create table user_session (
  token_hash   text        primary key,
  user_id      bigint      not null references app_user (id) on delete cascade,
  created_at   timestamptz not null default now(),
  last_seen_at timestamptz not null default now(),
  expires_at   timestamptz not null,
  user_agent   text        not null default ''
);

create index user_session_user on user_session (user_id);

-- One-time links: an invite creates an account, a reset sets a password.
create table account_link (
  token_hash  text        primary key,
  kind        text        not null check (kind in ('invite', 'reset')),
  -- Reset: whose password. Invite: null.
  user_id     bigint      references app_user (id) on delete cascade,
  created_by  bigint      references app_user (id) on delete set null,
  created_at  timestamptz not null default now(),
  expires_at  timestamptz not null,
  used_at     timestamptz
);

-- Tokens for scripts (`Authorization: Bearer ...`).
create table api_token (
  id           bigint generated always as identity primary key,
  user_id      bigint      not null references app_user (id) on delete cascade,
  name         text        not null,
  token_hash   text        not null unique,
  created_at   timestamptz not null default now(),
  last_used_at timestamptz
);

create index api_token_user on api_token (user_id);
