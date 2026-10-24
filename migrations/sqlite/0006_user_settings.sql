-- Each account's own settings (1.0): the same table as
-- migrations/postgres/0006_user_settings.sql. One JSON object per account
-- (startup options, Moxfield account, hidden decks, currency, card
-- language); what it leaves out comes from the server's settings file.
create table user_settings (
  user_id     integer primary key references app_user (id) on delete cascade,
  settings    text    not null default '{}',
  updated_at  text    not null
);
