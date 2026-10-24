-- Per-user collections (1.0): the same change as
-- migrations/postgres/0005_per_user_collection.sql.
--
-- Every row belongs to an account and records the copies' condition and
-- language, so one printing + finish can be several rows (a Near Mint
-- English copy and a Played Japanese one). Prices stay shared: they belong
-- to the printing + finish.
--
-- Existing rows go to the first admin. A collection from before accounts
-- gets an "admin" account without a password; the setup page then names
-- it and sets its password.

insert into app_user (name, password_hash, is_admin, created_at)
select 'admin', null, 1, strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
where exists (select 1 from collection)
  and not exists (select 1 from app_user where is_admin);

drop view collection_value;

create table collection_new (
  id           integer primary key,
  user_id      integer not null references app_user (id) on delete cascade,
  scryfall_id  text    not null,
  finish       text    not null default 'nonfoil'
               check (finish in ('nonfoil', 'foil', 'etched')),
  -- TCGplayer's conditions: Near Mint, Lightly / Moderately / Heavily Played, Damaged.
  condition    text    not null default 'nm'
               check (condition in ('nm', 'lp', 'mp', 'hp', 'dmg')),
  -- Scryfall language code of the copies, e.g. 'en', 'ja'.
  language     text    not null default 'en',
  quantity     integer not null check (quantity > 0),
  added_at     text    not null,
  updated_at   text    not null,
  unique (user_id, scryfall_id, finish, condition, language)
);

insert into collection_new (id, user_id, scryfall_id, finish, condition, language, quantity,
                            added_at, updated_at)
select c.id,
       (select min(id) from app_user where is_admin),
       c.scryfall_id, c.finish, 'nm',
       coalesce((select p.lang from card_printing p where p.scryfall_id = c.scryfall_id), 'en'),
       c.quantity, c.added_at, c.updated_at
from collection c;

drop table collection;
alter table collection_new rename to collection;
create index collection_user on collection (user_id);

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
  p.price_tcg / 100.0        as price_tcg,
  p.price_cardmarket / 100.0 as price_cardmarket,
  p.price_tix / 100.0        as price_tix,
  p.price_ck / 100.0         as price_ck,
  p.price_ck_buylist / 100.0 as price_ck_buylist,
  p.price_date,
  c.quantity * p.price_tcg / 100.0        as value_tcg,
  c.quantity * p.price_cardmarket / 100.0 as value_cardmarket,
  c.quantity * p.price_ck / 100.0         as value_ck,
  c.quantity * p.price_ck_buylist / 100.0 as value_ck_buylist,
  'https://api.scryfall.com/cards/' || c.scryfall_id as scryfall_url,
  c.added_at,
  c.updated_at
from collection c
left join price_history p
  on p.scryfall_id = c.scryfall_id
 and p.finish = c.finish
 and p.price_date = (
   select max(q.price_date) from price_history q
   where q.scryfall_id = c.scryfall_id and q.finish = c.finish
 );
