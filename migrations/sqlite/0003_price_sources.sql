-- Prices from Scryfall's daily data (1.0): the same columns as
-- migrations/postgres/0003_price_sources.sql (cents; tix in hundredths).
alter table price_history add column price_cardmarket integer check (price_cardmarket >= 0);
alter table price_history add column price_tix integer check (price_tix >= 0);

drop view collection_value;

-- Collection with its latest prices and value, plus a link to the card data.
create view collection_value as
select
  c.id,
  c.scryfall_id,
  c.finish,
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
