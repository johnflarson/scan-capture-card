-- Prices from Scryfall's daily data (1.0): besides TCGplayer (price_tcg, USD)
-- each day records Cardmarket (EUR) and MTGO (tix). Card Kingdom (price_ck,
-- price_ck_buylist) is filled only when that source is switched on.
alter table price_history
  add column price_cardmarket numeric(10,2) check (price_cardmarket >= 0),
  add column price_tix        numeric(10,2) check (price_tix >= 0);

comment on table price_history is
  'Daily price points per printing + finish. price_tcg = TCGplayer market (USD), price_cardmarket = Cardmarket trend (EUR), price_tix = MTGO (Cardhoarder tix), all from Scryfall; price_ck / price_ck_buylist = Card Kingdom retail / buylist (USD), when enabled.';

drop view collection_value;

-- Collection with its latest prices and value, plus a link to the card data.
create view collection_value as
select
  c.id,
  c.scryfall_id,
  c.finish,
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
