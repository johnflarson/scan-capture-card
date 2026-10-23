//! Prices for a printing + finish. Since 1.0 they come from Scryfall's card
//! data (TCGplayer USD, Cardmarket EUR, MTGO tix; refreshed daily, see
//! `bulk`), plus Card Kingdom's own price list when `[prices] card_kingdom`
//! is on.

pub mod cardkingdom;

use reqwest::blocking::Client;

use crate::config::PricesConfig;
use crate::model::{Card, Finish, Prices};
use cardkingdom::CardKingdom;

pub struct PriceService {
    /// `None` unless `[prices] card_kingdom = true`.
    pub cardkingdom: Option<CardKingdom>,
}

impl PriceService {
    pub fn new(http: Client, cfg: &PricesConfig) -> Self {
        Self {
            cardkingdom: cfg.card_kingdom.then(|| CardKingdom::new(http)),
        }
    }

    pub fn price(&self, card: &Card, finish: Finish) -> Prices {
        let mut prices = scryfall_prices(card, finish);
        if let Some(ck) = self
            .cardkingdom
            .as_ref()
            .and_then(|ck| ck.price(&card.scryfall_id, finish))
        {
            prices.cardkingdom = ck.retail;
            prices.cardkingdom_buylist = ck.buylist;
            prices.cardkingdom_url = Some(ck.url);
        }
        prices
    }
}

/// A printing's prices for `finish` from Scryfall's card data.
pub fn scryfall_prices(card: &Card, finish: Finish) -> Prices {
    let (usd, eur) = match finish {
        Finish::Nonfoil => (card.scryfall_usd, card.scryfall_eur),
        Finish::Foil => (card.scryfall_usd_foil, card.scryfall_eur_foil),
        // Scryfall has no Cardmarket price for etched foils.
        Finish::Etched => (card.scryfall_usd_etched, None),
    };
    Prices {
        tcgplayer: usd,
        tcgplayer_url: tcgplayer_url(card, finish),
        cardmarket: eur,
        // MTGO prices are per card, not per paper finish.
        tix: card.scryfall_tix,
        ..Prices::default()
    }
}

/// The printing's product page on TCGplayer (etched foils are a separate product).
pub fn tcgplayer_url(card: &Card, finish: Finish) -> Option<String> {
    let product = match finish {
        Finish::Etched => card.tcgplayer_etched_id.or(card.tcgplayer_id),
        _ => card.tcgplayer_id,
    };
    product.map(|id| format!("https://www.tcgplayer.com/product/{id}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card() -> Card {
        Card {
            scryfall_id: "x".into(),
            oracle_id: None,
            name: "Sol Ring".into(),
            lang: "en".into(),
            set_code: "c21".into(),
            set_name: String::new(),
            collector_number: "263".into(),
            rarity: "uncommon".into(),
            colors: Vec::new(),
            type_line: "Artifact".into(),
            finishes: vec![Finish::Nonfoil, Finish::Foil, Finish::Etched],
            tcgplayer_id: Some(1),
            tcgplayer_etched_id: Some(2),
            cardmarket_id: None,
            image_small: None,
            image_normal: None,
            scryfall_usd: Some(2.03),
            scryfall_usd_foil: Some(5.0),
            scryfall_usd_etched: Some(9.0),
            scryfall_eur: Some(1.5),
            scryfall_eur_foil: Some(4.0),
            scryfall_tix: Some(0.1),
        }
    }

    #[test]
    fn each_finish_gets_its_own_scryfall_prices() {
        let c = card();
        let p = scryfall_prices(&c, Finish::Nonfoil);
        assert_eq!(
            (p.tcgplayer, p.cardmarket, p.tix),
            (Some(2.03), Some(1.5), Some(0.1))
        );
        assert_eq!(
            p.tcgplayer_url.as_deref(),
            Some("https://www.tcgplayer.com/product/1")
        );
        let p = scryfall_prices(&c, Finish::Foil);
        assert_eq!((p.tcgplayer, p.cardmarket), (Some(5.0), Some(4.0)));
        let p = scryfall_prices(&c, Finish::Etched);
        assert_eq!((p.tcgplayer, p.cardmarket), (Some(9.0), None));
        assert_eq!(
            p.tcgplayer_url.as_deref(),
            Some("https://www.tcgplayer.com/product/2")
        );
        assert_eq!(p.cardkingdom, None, "Card Kingdom only when switched on");
    }

    #[test]
    fn card_kingdom_is_off_by_default() {
        crate::tls::init();
        let service = PriceService::new(Client::new(), &PricesConfig::default());
        assert!(service.cardkingdom.is_none());
        let p = service.price(&card(), Finish::Nonfoil);
        assert_eq!((p.tcgplayer, p.cardkingdom), (Some(2.03), None));
    }
}
