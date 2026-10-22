-- Local copy of Scryfall's card data: filled from Scryfall's daily bulk file
-- (and from the API for cards not synced yet), so listing the collection
-- doesn't call Scryfall each time. A cache only: it can be rebuilt from
-- Scryfall at any time, so nothing else references it.
create table card_printing (
  scryfall_id         uuid primary key,
  oracle_id           uuid,
  name                text not null,
  lang                text not null default 'en',
  set_code            text not null,
  set_name            text not null default '',
  collector_number    text not null,
  rarity              text not null default '',
  type_line           text not null default '',
  -- Color letters in WUBRG order, e.g. 'WU'; '' for colorless.
  colors              text not null default '',
  -- Comma-separated: 'nonfoil,foil'.
  finishes            text not null default '',
  tcgplayer_id        bigint,
  tcgplayer_etched_id bigint,
  cardmarket_id       bigint,
  image_small         text,
  image_normal        text,
  -- Scryfall's daily prices: TCGplayer USD, Cardmarket EUR, MTGO tix.
  usd                 numeric(10,2),
  usd_foil            numeric(10,2),
  usd_etched          numeric(10,2),
  eur                 numeric(10,2),
  eur_foil            numeric(10,2),
  tix                 numeric(10,2),
  synced_at           timestamptz not null default now()
);

create index card_printing_name on card_printing (name);

comment on table card_printing is
  'Cache of Scryfall card data (https://scryfall.com/docs/api/bulk-data), one row per printing.';

-- Background jobs inside the app (e.g. the daily card data sync): when each
-- last finished, and what it saw (e.g. the bulk file''s date).
create table app_job (
  name     text primary key,
  done_at  timestamptz not null,
  detail   text not null default ''
);
