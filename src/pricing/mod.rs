pub mod cardkingdom;
pub mod tcgplayer;

use reqwest::blocking::Client;

use crate::model::{Card, Finish, Prices};
use cardkingdom::CardKingdom;
use tcgplayer::TcgPlayer;

pub struct PriceService {
    pub tcgplayer: TcgPlayer,
    pub cardkingdom: CardKingdom,
}

impl PriceService {
    pub fn new(http: Client) -> Self {
        Self {
            tcgplayer: TcgPlayer::new(http.clone()),
            cardkingdom: CardKingdom::new(http),
        }
    }

    pub fn price(&self, card: &Card, finish: Finish) -> Prices {
        let tcg = self.tcgplayer.price(card, finish);
        let ck = self.cardkingdom.price(&card.scryfall_id, finish);
        Prices {
            tcgplayer: tcg.market,
            tcgplayer_url: tcg.url,
            cardkingdom: ck.as_ref().and_then(|c| c.retail),
            cardkingdom_buylist: ck.as_ref().and_then(|c| c.buylist),
            cardkingdom_url: ck.map(|c| c.url),
        }
    }
}
