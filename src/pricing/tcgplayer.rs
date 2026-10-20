//! TCGplayer market prices via the same price-points endpoint the
//! tcgplayer.com product pages use. Falls back to Scryfall's daily
//! TCGplayer-sourced price when the live lookup fails.

use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde::Deserialize;

use crate::model::{Card, Finish};

const PRICEPOINTS: &str = "https://mpapi.tcgplayer.com/v2/product";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PricePoint {
    printing_type: String,
    market_price: Option<f64>,
    listed_median_price: Option<f64>,
}

pub struct TcgPlayer {
    http: Client,
}

pub struct TcgPrice {
    pub market: Option<f64>,
    pub url: Option<String>,
}

impl TcgPlayer {
    pub fn new(http: Client) -> Self {
        Self { http }
    }

    pub fn price(&self, card: &Card, finish: Finish) -> TcgPrice {
        // Etched foils are listed as a separate TCGplayer product.
        let product = match finish {
            Finish::Etched => card.tcgplayer_etched_id.or(card.tcgplayer_id),
            _ => card.tcgplayer_id,
        };
        let fallback = match finish {
            Finish::Nonfoil => card.scryfall_usd,
            Finish::Foil => card.scryfall_usd_foil,
            Finish::Etched => card.scryfall_usd_etched,
        };
        let Some(product) = product else {
            return TcgPrice {
                market: fallback,
                url: None,
            };
        };
        let live = self.live_price(product, finish).unwrap_or_else(|e| {
            eprintln!("  ! TCGplayer lookup failed ({e:#}); using Scryfall's cached price");
            None
        });
        TcgPrice {
            market: live.or(fallback),
            url: Some(format!("https://www.tcgplayer.com/product/{product}")),
        }
    }

    fn live_price(&self, product: u64, finish: Finish) -> Result<Option<f64>> {
        let points: Vec<PricePoint> = self
            .http
            .get(format!("{PRICEPOINTS}/{product}/pricepoints"))
            .send()
            .and_then(|r| r.error_for_status())
            .context("request failed")?
            .json()
            .context("unexpected response")?;
        let wanted = match finish {
            Finish::Nonfoil => "Normal",
            Finish::Foil | Finish::Etched => "Foil",
        };
        let point = points
            .iter()
            .find(|p| p.printing_type.eq_ignore_ascii_case(wanted))
            // Foil-only products (e.g. etched) sometimes list a single printing.
            .or_else(|| (points.len() == 1).then(|| &points[0]));
        Ok(point.and_then(|p| p.market_price.or(p.listed_median_price)))
    }
}
