-- mtg-scan schema v1.
--
-- Card data (name, set, collector number, rarity, images, ...) is NOT stored:
-- it lives on Scryfall and is looked up by `scryfall_id`. A Scryfall ID
-- identifies one *printing*, so every printing of a card is its own row,
-- and foil / nonfoil / etched copies of a printing are separate rows too.

-- What you own: one row per printing + finish.
create table collection (
  id           bigint generated always as identity primary key,
  scryfall_id  uuid        not null,
  finish       text        not null default 'nonfoil'
               check (finish in ('nonfoil', 'foil', 'etched')),
  quantity     integer     not null check (quantity > 0),
  added_at     timestamptz not null default now(),
  updated_at   timestamptz not null default now(),
  constraint collection_printing_key unique (scryfall_id, finish)
);

comment on table collection is
  'Owned cards, one row per Scryfall printing + finish. Card details: https://api.scryfall.com/cards/{scryfall_id}';

-- Prices observed for a printing + finish, at most one row per day (later
-- observations the same day replace earlier ones). Not tied to `collection`
-- by a foreign key, so history survives selling a card and buying it back.
create table price_history (
  id                bigint generated always as identity primary key,
  scryfall_id       uuid          not null,
  finish            text          not null
                    check (finish in ('nonfoil', 'foil', 'etched')),
  price_date        date          not null default current_date,
  price_tcg         numeric(10,2) check (price_tcg >= 0),
  price_ck          numeric(10,2) check (price_ck >= 0),
  price_ck_buylist  numeric(10,2) check (price_ck_buylist >= 0),
  captured_at       timestamptz   not null default now(),
  -- Also the index for "latest price of a printing" lookups (equality
  -- columns first, then the date range / sort column).
  constraint price_history_daily_key unique (scryfall_id, finish, price_date)
);

comment on table price_history is
  'Daily price points per printing + finish. price_tcg = TCGplayer market, price_ck = Card Kingdom retail, price_ck_buylist = Card Kingdom buylist (USD).';

-- Collection with its latest prices and value, plus a link to the card data.
create view collection_value as
select
  c.id,
  c.scryfall_id,
  c.finish,
  c.quantity,
  p.price_tcg,
  p.price_ck,
  p.price_ck_buylist,
  p.price_date,
  c.quantity * p.price_tcg        as value_tcg,
  c.quantity * p.price_ck         as value_ck,
  c.quantity * p.price_ck_buylist as value_ck_buylist,
  'https://api.scryfall.com/cards/' || c.scryfall_id as scryfall_url,
  c.added_at,
  c.updated_at
from collection c
left join lateral (
  select ph.price_tcg, ph.price_ck, ph.price_ck_buylist, ph.price_date
  from price_history ph
  where ph.scryfall_id = c.scryfall_id
    and ph.finish = c.finish
  order by ph.price_date desc
  limit 1
) p on true;
