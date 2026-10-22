-- mtg-scan schema v1 for SQLite: the same tables as migrations/postgres/0001_init.sql.
--
-- Card data (name, set, collector number, rarity, images, ...) is NOT stored:
-- it lives on Scryfall and is looked up by `scryfall_id`. A Scryfall ID
-- identifies one *printing*, so every printing of a card is its own row,
-- and foil / nonfoil / etched copies of a printing are separate rows too.
--
-- SQLite differences: UUIDs are lowercase text, dates are ISO-8601 text
-- (`YYYY-MM-DD`, local date; timestamps in UTC), money is integer cents.

-- What you own: one row per printing + finish.
create table collection (
  id           integer primary key,
  scryfall_id  text    not null,
  finish       text    not null default 'nonfoil'
               check (finish in ('nonfoil', 'foil', 'etched')),
  quantity     integer not null check (quantity > 0),
  -- Local date the row was first added.
  added_at     text    not null,
  updated_at   text    not null,
  unique (scryfall_id, finish)
);

-- Prices observed for a printing + finish, at most one row per day (later
-- observations the same day replace earlier ones). Not tied to `collection`
-- by a foreign key, so history survives selling a card and buying it back.
-- price_tcg = TCGplayer market, price_ck = Card Kingdom retail,
-- price_ck_buylist = Card Kingdom buylist; all USD cents.
create table price_history (
  scryfall_id       text    not null,
  finish            text    not null
                    check (finish in ('nonfoil', 'foil', 'etched')),
  price_date        text    not null,
  price_tcg         integer check (price_tcg >= 0),
  price_ck          integer check (price_ck >= 0),
  price_ck_buylist  integer check (price_ck_buylist >= 0),
  captured_at       text    not null,
  primary key (scryfall_id, finish, price_date)
) without rowid;

-- Collection with its latest prices and value (USD), plus a link to the card data.
create view collection_value as
select
  c.id,
  c.scryfall_id,
  c.finish,
  c.quantity,
  p.price_tcg / 100.0        as price_tcg,
  p.price_ck / 100.0         as price_ck,
  p.price_ck_buylist / 100.0 as price_ck_buylist,
  p.price_date,
  c.quantity * p.price_tcg / 100.0        as value_tcg,
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
