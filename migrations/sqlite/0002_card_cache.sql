-- Local copy of Scryfall's card data: the same table as
-- migrations/postgres/0002_card_cache.sql. Prices are integer cents.
create table card_printing (
  scryfall_id         text primary key,
  oracle_id           text,
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
  tcgplayer_id        integer,
  tcgplayer_etched_id integer,
  cardmarket_id       integer,
  image_small         text,
  image_normal        text,
  -- Scryfall's daily prices in cents: TCGplayer USD, Cardmarket EUR, MTGO tix.
  usd                 integer,
  usd_foil            integer,
  usd_etched          integer,
  eur                 integer,
  eur_foil            integer,
  tix                 integer,
  synced_at           text not null
);

create index card_printing_name on card_printing (name);

-- Background jobs inside the app (e.g. the daily card data sync).
create table app_job (
  name     text primary key,
  done_at  text not null,
  detail   text not null default ''
);
