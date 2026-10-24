-- Each account's own settings (1.0). One JSON object per account (startup
-- options, Moxfield account, hidden decks, currency, card language); what it
-- leaves out comes from the server's settings file.
create table user_settings (
  user_id     bigint      primary key references app_user (id) on delete cascade,
  settings    jsonb       not null default '{}',
  updated_at  timestamptz not null default now()
);
