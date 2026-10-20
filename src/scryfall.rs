//! Scryfall API client: the canonical source for card identity.
//! <https://scryfall.com/docs/api> asks for a User-Agent, an Accept header and
//! no more than ~10 requests per second (2 per second for name lookups and
//! searches).

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use reqwest::StatusCode;
use reqwest::blocking::{Client, RequestBuilder};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::json;

use crate::cache;
use crate::model::{Card, Finish};

const API: &str = "https://api.scryfall.com";
const MIN_INTERVAL: Duration = Duration::from_millis(110);
const MIN_INTERVAL_STRICT: Duration = Duration::from_millis(550);
const STRICT_ENDPOINTS: [&str; 4] = [
    "/cards/named",
    "/cards/search",
    "/cards/random",
    "/cards/collection",
];
const SETS_CACHE: &str = "scryfall_sets.json";
const SETS_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 3600);
const NAMES_CACHE: &str = "scryfall_card_names.json";
/// Max identifiers per `/cards/collection` request.
const COLLECTION_BATCH: usize = 75;

#[derive(Deserialize)]
struct Catalog {
    data: Vec<String>,
}

pub struct Scryfall {
    http: Client,
    last_request: Cell<Option<Instant>>,
    last_strict: Cell<Option<Instant>>,
    printings_cache: RefCell<HashMap<String, Vec<Card>>>,
}

#[derive(Deserialize)]
struct ApiCard {
    id: String,
    name: String,
    set: String,
    set_name: String,
    collector_number: String,
    rarity: String,
    #[serde(default)]
    finishes: Vec<String>,
    tcgplayer_id: Option<u64>,
    tcgplayer_etched_id: Option<u64>,
    image_uris: Option<ImageUris>,
    card_faces: Option<Vec<ApiFace>>,
    prices: ApiPrices,
}

#[derive(Deserialize)]
struct ApiFace {
    image_uris: Option<ImageUris>,
}

#[derive(Deserialize)]
struct ImageUris {
    small: Option<String>,
}

#[derive(Deserialize)]
struct SearchPage {
    data: Vec<ApiCard>,
}

#[derive(Deserialize)]
struct ApiPrices {
    usd: Option<String>,
    usd_foil: Option<String>,
    usd_etched: Option<String>,
}

#[derive(Deserialize)]
struct SetList {
    data: Vec<SetEntry>,
}

#[derive(Deserialize)]
struct SetEntry {
    code: String,
}

impl From<ApiCard> for Card {
    fn from(c: ApiCard) -> Self {
        let parse = |s: Option<String>| s.and_then(|v| v.parse::<f64>().ok());
        // Double-faced cards keep images per face; use the front.
        let image_small = c.image_uris.and_then(|i| i.small).or_else(|| {
            c.card_faces?
                .into_iter()
                .find_map(|f| f.image_uris.and_then(|i| i.small))
        });
        Card {
            scryfall_id: c.id,
            name: c.name,
            set_code: c.set,
            set_name: c.set_name,
            collector_number: c.collector_number,
            rarity: c.rarity,
            finishes: c
                .finishes
                .iter()
                .filter_map(|f| f.parse::<Finish>().ok())
                .collect(),
            tcgplayer_id: c.tcgplayer_id,
            tcgplayer_etched_id: c.tcgplayer_etched_id,
            image_small,
            scryfall_usd: parse(c.prices.usd),
            scryfall_usd_foil: parse(c.prices.usd_foil),
            scryfall_usd_etched: parse(c.prices.usd_etched),
        }
    }
}

impl Scryfall {
    pub fn new(http: Client) -> Self {
        Self {
            http,
            last_request: Cell::new(None),
            last_strict: Cell::new(None),
            printings_cache: RefCell::new(HashMap::new()),
        }
    }

    /// Space requests out. Name lookups and searches have a stricter limit
    /// (2/s) than the rest of the API (10/s).
    fn throttle(&self, path: &str) {
        let strict = STRICT_ENDPOINTS.iter().any(|p| path.starts_with(p));
        let (cell, min) = if strict {
            (&self.last_strict, MIN_INTERVAL_STRICT)
        } else {
            (&self.last_request, MIN_INTERVAL)
        };
        if let Some(last) = cell.get() {
            let elapsed = last.elapsed();
            if elapsed < min {
                std::thread::sleep(min - elapsed);
            }
        }
        cell.set(Some(Instant::now()));
    }

    /// GET an API path, retrying on 429. `Ok(None)` on 404.
    fn api_get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<Option<T>> {
        self.api_request(path, || self.http.get(format!("{API}{path}")).query(query))
    }

    /// Send a request built by `build`, retrying on 429. `Ok(None)` on 404.
    fn api_request<T: DeserializeOwned>(
        &self,
        path: &str,
        build: impl Fn() -> RequestBuilder,
    ) -> Result<Option<T>> {
        let mut backoff = Duration::from_secs(1);
        for attempt in 0..4 {
            self.throttle(path);
            let resp = build()
                .header("Accept", "application/json")
                .send()
                .with_context(|| format!("Scryfall request failed: {path}"))?;
            match resp.status() {
                StatusCode::NOT_FOUND => return Ok(None),
                StatusCode::TOO_MANY_REQUESTS if attempt < 3 => {
                    let wait = resp
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse().ok())
                        .map(Duration::from_secs)
                        .unwrap_or(backoff);
                    std::thread::sleep(wait);
                    backoff *= 2;
                }
                _ => {
                    return resp
                        .error_for_status()
                        .with_context(|| format!("Scryfall returned an error for {path}"))?
                        .json()
                        .map(Some)
                        .with_context(|| format!("unexpected Scryfall response for {path}"));
                }
            }
        }
        unreachable!("the last attempt always returns")
    }

    fn get_card(&self, path: &str, query: &[(&str, &str)]) -> Result<Option<Card>> {
        Ok(self.api_get::<ApiCard>(path, query)?.map(Card::from))
    }

    /// Look up many printings by Scryfall ID (75 per request). IDs Scryfall
    /// doesn't know are simply absent from the result.
    pub fn cards_by_id(&self, ids: &[String]) -> Result<HashMap<String, Card>> {
        let mut out = HashMap::new();
        for chunk in ids.chunks(COLLECTION_BATCH) {
            let body = json!({
                "identifiers": chunk.iter().map(|id| json!({"id": id})).collect::<Vec<_>>()
            });
            let page: Option<SearchPage> = self.api_request("/cards/collection", || {
                self.http
                    .post(format!("{API}/cards/collection"))
                    .json(&body)
            })?;
            for card in page.map(|p| p.data).unwrap_or_default() {
                let card = Card::from(card);
                out.insert(card.scryfall_id.clone(), card);
            }
        }
        Ok(out)
    }

    pub fn by_set_number(&self, set: &str, number: &str) -> Result<Option<Card>> {
        let path = format!(
            "/cards/{}/{}",
            urlencode(&set.to_lowercase()),
            urlencode(number)
        );
        self.get_card(&path, &[])
    }

    pub fn by_id(&self, id: &str) -> Result<Option<Card>> {
        self.get_card(&format!("/cards/{}", urlencode(id)), &[])
    }

    /// Fuzzy name lookup, optionally restricted to a set.
    pub fn named_fuzzy(&self, name: &str, set: Option<&str>) -> Result<Option<Card>> {
        let mut query = vec![("fuzzy", name)];
        let set_lower = set.map(str::to_lowercase);
        if let Some(s) = set_lower.as_deref() {
            query.push(("set", s));
        }
        self.get_card("/cards/named", &query)
    }

    /// Every paper printing of the card with this exact name, newest first
    /// (first page only: up to 175 printings).
    pub fn printings(&self, exact_name: &str) -> Result<Vec<Card>> {
        if let Some(hit) = self.printings_cache.borrow().get(exact_name) {
            return Ok(hit.clone());
        }
        let cards = self.fetch_printings(exact_name)?;
        self.printings_cache
            .borrow_mut()
            .insert(exact_name.to_string(), cards.clone());
        Ok(cards)
    }

    fn fetch_printings(&self, exact_name: &str) -> Result<Vec<Card>> {
        let query = format!("!\"{exact_name}\" game:paper");
        let page: Option<SearchPage> = self.api_get(
            "/cards/search",
            &[
                ("q", query.as_str()),
                ("unique", "prints"),
                ("order", "released"),
                ("dir", "desc"),
            ],
        )?;
        Ok(page
            .map(|p| p.data.into_iter().map(Card::from).collect())
            .unwrap_or_default())
    }

    /// Download (or read from the disk cache) a card's small image.
    pub fn small_image(&self, card: &Card) -> Result<Option<image::DynamicImage>> {
        let Some(url) = &card.image_small else {
            return Ok(None);
        };
        let path = cache::path(&format!("images/{}.jpg", card.scryfall_id));
        if let Ok(img) = image::open(&path) {
            return Ok(Some(img));
        }
        // Image CDN requests aren't subject to the API rate limit.
        let bytes = self
            .http
            .get(url)
            .send()
            .and_then(|r| r.error_for_status())
            .and_then(|r| r.bytes())
            .with_context(|| format!("failed to download {url}"))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, &bytes)?;
        Ok(Some(image::load_from_memory(&bytes)?))
    }

    /// Every card name (double-faced cards as "Front // Back"), cached for a week.
    pub fn card_names(&self) -> Result<Vec<String>> {
        if let Some(cached) = cache::read_fresh::<Vec<String>>(NAMES_CACHE, SETS_MAX_AGE) {
            return Ok(cached);
        }
        let catalog: Catalog = self
            .api_get("/catalog/card-names", &[])?
            .context("Scryfall card-name catalog not found")?;
        cache::write(NAMES_CACHE, &catalog.data)?;
        Ok(catalog.data)
    }

    /// All known set codes (lowercase), cached on disk for a week. Used to
    /// reject OCR noise that merely looks like a set code.
    pub fn set_codes(&self) -> Result<HashSet<String>> {
        if let Some(cached) = cache::read_fresh::<Vec<String>>(SETS_CACHE, SETS_MAX_AGE) {
            return Ok(cached.into_iter().collect());
        }
        let list: SetList = self
            .api_get("/sets", &[])?
            .context("Scryfall set list not found")?;
        let codes: Vec<String> = list
            .data
            .into_iter()
            .map(|s| s.code.to_lowercase())
            .collect();
        cache::write(SETS_CACHE, &codes)?;
        Ok(codes.into_iter().collect())
    }
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}
