//! Card Kingdom retail and buylist prices from Card Kingdom's public price
//! list (<https://api.cardkingdom.com/api/v2/pricelist>). The full list is
//! ~70 MB, so it is downloaded at most once a day and reduced to a compact
//! index keyed by Scryfall ID.

use std::cell::OnceCell;
use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};

use crate::cache;
use crate::model::Finish;

const PRICELIST: &str = "https://api.cardkingdom.com/api/v2/pricelist";
const INDEX_CACHE: &str = "cardkingdom_index.json";
const MAX_AGE: Duration = Duration::from_secs(24 * 3600);

#[derive(Deserialize)]
struct PriceList {
    meta: Meta,
    data: Vec<Product>,
}

#[derive(Deserialize)]
struct Meta {
    base_url: String,
}

#[derive(Deserialize)]
struct Product {
    scryfall_id: Option<String>,
    url: String,
    variation: Option<String>,
    is_foil: String,
    price_retail: String,
    price_buy: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CkPrice {
    pub retail: Option<f64>,
    pub buylist: Option<f64>,
    pub url: String,
}

#[derive(Default, Serialize, Deserialize)]
struct Index {
    nonfoil: HashMap<String, CkPrice>,
    foil: HashMap<String, CkPrice>,
}

pub struct CardKingdom {
    http: Client,
    index: OnceCell<Index>,
}

impl CardKingdom {
    pub fn new(http: Client) -> Self {
        Self {
            http,
            index: OnceCell::new(),
        }
    }

    pub fn price(&self, scryfall_id: &str, finish: Finish) -> Option<CkPrice> {
        let index = self.index.get_or_init(|| self.load_index());
        let map = match finish {
            Finish::Nonfoil => &index.nonfoil,
            Finish::Foil | Finish::Etched => &index.foil,
        };
        map.get(scryfall_id).cloned()
    }

    /// Drop the cached index so the next lookup re-downloads the price list.
    pub fn force_refresh(&self) -> Result<()> {
        let index = self.download()?;
        cache::write(INDEX_CACHE, &index)?;
        let _ = self.index.set(index);
        Ok(())
    }

    fn load_index(&self) -> Index {
        if let Some(index) = cache::read_fresh::<Index>(INDEX_CACHE, MAX_AGE) {
            return index;
        }
        eprintln!("  Downloading Card Kingdom price list (daily, ~70 MB)...");
        match self.download() {
            Ok(index) => {
                if let Err(e) = cache::write(INDEX_CACHE, &index) {
                    eprintln!("  ! could not cache Card Kingdom prices: {e:#}");
                }
                index
            }
            Err(e) => {
                eprintln!("  ! Card Kingdom price list unavailable ({e:#})");
                cache::read_any(INDEX_CACHE).unwrap_or_default()
            }
        }
    }

    fn download(&self) -> Result<Index> {
        let list: PriceList = self
            .http
            .get(PRICELIST)
            .timeout(Duration::from_secs(300))
            .send()
            .and_then(|r| r.error_for_status())
            .context("download failed")?
            .json()
            .context("unexpected price list format")?;
        Ok(build_index(list))
    }
}

fn build_index(list: PriceList) -> Index {
    let mut index = Index::default();
    for p in list.data {
        let Some(id) = p.scryfall_id.filter(|s| !s.is_empty()) else {
            continue;
        };
        let price = CkPrice {
            retail: p.price_retail.parse().ok().filter(|v: &f64| *v > 0.0),
            buylist: p.price_buy.parse().ok().filter(|v: &f64| *v > 0.0),
            url: format!("{}{}", list.meta.base_url, p.url),
        };
        let map = if p.is_foil == "true" {
            &mut index.foil
        } else {
            &mut index.nonfoil
        };
        // Several CK SKUs can share a Scryfall ID (e.g. variants); prefer the
        // plain one without a variation label.
        let plain = p.variation.as_deref().unwrap_or("").is_empty();
        if plain || !map.contains_key(&id) {
            map.insert(id, price);
        }
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_index_by_finish() {
        let json = r#"{"meta":{"base_url":"https://www.cardkingdom.com/"},"data":[
            {"scryfall_id":"abc","url":"mtg/x/card","variation":"","is_foil":"false","price_retail":"1.50","price_buy":"0.75"},
            {"scryfall_id":"abc","url":"mtg/x/card-foil","variation":"","is_foil":"true","price_retail":"4.00","price_buy":"0.00"},
            {"scryfall_id":"abc","url":"mtg/x/card-alt","variation":"Alt","is_foil":"false","price_retail":"9.99","price_buy":"1.00"}
        ]}"#;
        let index = build_index(serde_json::from_str(json).unwrap());
        let nf = &index.nonfoil["abc"];
        assert_eq!(nf.retail, Some(1.50));
        assert_eq!(nf.url, "https://www.cardkingdom.com/mtg/x/card");
        let f = &index.foil["abc"];
        assert_eq!(f.retail, Some(4.00));
        assert_eq!(f.buylist, None);
    }
}
