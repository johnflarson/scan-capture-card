-- Per-user collections (1.0). Every row belongs to an account and records
-- the copies' condition and language, so one printing + finish can be
-- several rows (a Near Mint English copy and a Played Japanese one). Prices
-- stay shared: they belong to the printing + finish.
--
-- Existing rows go to the first admin. A collection from before accounts
-- gets an "admin" account without a password; the setup page then names
-- it and sets its password.

insert into app_user (name, password_hash, is_admin)
select 'admin', null, true
where exists (select 1 from collection)
  and not exists (select 1 from app_user where is_admin);

drop view collection_value;

alter table collection
  add column user_id   bigint references app_user (id) on delete cascade,
  -- TCGplayer's conditions: Near Mint, Lightly / Moderately / Heavily Played, Damaged.
  add column condition text not null default 'nm'
             check (condition in ('nm', 'lp', 'mp', 'hp', 'dmg')),
  -- Scryfall language code of the copies, e.g. 'en', 'ja'.
  add column language  text not null default 'en';

update collection c
set user_id = (select min(id) from app_user where is_admin),
    language = coalesce((select p.lang from card_printing p where p.scryfall_id = c.scryfall_id),
                        'en');

alter table collection
  alter column user_id set not null,
  drop constraint collection_printing_key,
  add constraint collection_row_key unique (user_id, scryfall_id, finish, condition, language);

create index collection_user on collection (user_id);

comment on table collection is
  'Owned cards: one row per account, Scryfall printing, finish, condition and language. Card details: https://api.scryfall.com/cards/{scryfall_id}';

-- Collection rows with their latest prices and value, plus a link to the card data.
create view collection_value as
select
  c.id,
  c.user_id,
  c.scryfall_id,
  c.finish,
  c.condition,
  c.language,
  c.quantity,
  p.price_tcg,
  p.price_cardmarket,
  p.price_tix,
  p.price_ck,
  p.price_ck_buylist,
  p.price_date,
  c.quantity * p.price_tcg        as value_tcg,
  c.quantity * p.price_cardmarket as value_cardmarket,
  c.quantity * p.price_ck         as value_ck,
  c.quantity * p.price_ck_buylist as value_ck_buylist,
  'https://api.scryfall.com/cards/' || c.scryfall_id as scryfall_url,
  c.added_at,
  c.updated_at
from collection c
left join lateral (
  select ph.price_tcg, ph.price_cardmarket, ph.price_tix, ph.price_ck, ph.price_ck_buylist,
         ph.price_date
  from price_history ph
  where ph.scryfall_id = c.scryfall_id
    and ph.finish = c.finish
  order by ph.price_date desc
  limit 1
) p on true;
