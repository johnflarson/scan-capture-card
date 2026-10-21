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

    /// The printing a pasted Scryfall link (or ID) points at.
    pub fn by_ref(&self, card: &CardRef) -> Result<Option<Card>> {
        match card {
            CardRef::Id(id) => self.by_id(id),
            CardRef::SetNumber {
                set,
                number,
                lang: None,
            } => self.by_set_number(set, number),
            CardRef::SetNumber {
                set,
                number,
                lang: Some(lang),
            } => {
                let path = format!(
                    "/cards/{}/{}/{}",
                    urlencode(set),
                    urlencode(number),
                    urlencode(lang)
                );
                self.get_card(&path, &[])
            }
        }
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

/// A printing as written in a Scryfall link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CardRef {
    Id(String),
    SetNumber {
        set: String,
        number: String,
        lang: Option<String>,
    },
}

/// Scryfall's language codes, as they appear in non-English card links.
const LANGS: [&str; 17] = [
    "en", "es", "fr", "de", "it", "pt", "ja", "ko", "ru", "zhs", "zht", "he", "la", "grc", "ar",
    "sa", "ph",
];

/// Understand a pasted Scryfall link: a card page
/// (`scryfall.com/card/{set}/{number}[/{lang}]/{name}`), an API link, an
/// image link, or a bare Scryfall ID.
pub fn parse_card_ref(text: &str) -> Option<CardRef> {
    let text = text.trim();
    // Any Scryfall ID in the text (API and image links carry one).
    let uuid =
        regex::Regex::new(r"(?i)\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b")
            .expect("valid regex");
    if let Some(m) = uuid.find(text) {
        return Some(CardRef::Id(m.as_str().to_lowercase()));
    }
    let path = text.split(['?', '#']).next().unwrap_or_default();
    let path = path.split_once("://").map(|(_, rest)| rest).unwrap_or(path);
    let (host, path) = path.split_once('/')?;
    if !host.to_lowercase().ends_with("scryfall.com") {
        return None;
    }
    let parts: Vec<String> = path
        .split('/')
        .filter(|p| !p.is_empty())
        .map(percent_decode)
        .collect();
    // scryfall.com/card/… or api.scryfall.com/cards/…
    let rest = match parts.first().map(String::as_str) {
        Some("card" | "cards") => &parts[1..],
        _ => return None,
    };
    let [set, number, more @ ..] = rest else {
        return None;
    };
    let lang = match more {
        [lang, _name, ..] => Some(lang.to_lowercase()),
        [only] if LANGS.contains(&only.to_lowercase().as_str()) => Some(only.to_lowercase()),
        _ => None,
    }
    .filter(|l| l != "en");
    Some(CardRef::SetNumber {
        set: set.to_lowercase(),
        number: number.clone(),
        lang,
    })
}

/// Decode `%XX` escapes (collector numbers can contain e.g. `★`).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(b)) => {
                out.push(b);
                i += 3;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn set_number(set: &str, number: &str, lang: Option<&str>) -> Option<CardRef> {
        Some(CardRef::SetNumber {
            set: set.into(),
            number: number.into(),
            lang: lang.map(Into::into),
        })
    }

    #[test]
    fn parses_card_page_links() {
        assert_eq!(
            parse_card_ref("https://scryfall.com/card/fdn/123/llanowar-elves"),
            set_number("fdn", "123", None)
        );
        assert_eq!(
            parse_card_ref("  scryfall.com/card/FDN/123/llanowar-elves?utm=x#top "),
            set_number("fdn", "123", None)
        );
        // Collector numbers with symbols are percent-encoded in links.
        assert_eq!(
            parse_card_ref("https://scryfall.com/card/sld/1%E2%98%85/sol-ring"),
            set_number("sld", "1★", None)
        );
        assert_eq!(
            parse_card_ref("https://scryfall.com/card/war/1/ja/karn-the-great-creator"),
            set_number("war", "1", Some("ja"))
        );
        assert_eq!(
            parse_card_ref("https://scryfall.com/card/war/1/en/karn"),
            set_number("war", "1", None)
        );
        assert_eq!(
            parse_card_ref("https://api.scryfall.com/cards/neo/12"),
            set_number("neo", "12", None)
        );
    }

    #[test]
    fn parses_ids_and_id_links() {
        let id = "0000579f-7b35-4ed3-b44c-db2a538066fe";
        assert_eq!(parse_card_ref(id), Some(CardRef::Id(id.into())));
        assert_eq!(
            parse_card_ref(&format!(
                "https://api.scryfall.com/cards/{}",
                id.to_uppercase()
            )),
            Some(CardRef::Id(id.into()))
        );
        assert_eq!(
            parse_card_ref(&format!(
                "https://cards.scryfall.io/normal/front/0/0/{id}.jpg?1562894979"
            )),
            Some(CardRef::Id(id.into()))
        );
    }

    #[test]
    fn rejects_other_links() {
        assert_eq!(parse_card_ref("https://moxfield.com/card/fdn/123"), None);
        assert_eq!(parse_card_ref("https://scryfall.com/sets/fdn"), None);
        assert_eq!(parse_card_ref("https://scryfall.com/card/fdn"), None);
        assert_eq!(parse_card_ref("llanowar elves"), None);
        assert_eq!(parse_card_ref(""), None);
    }
}
