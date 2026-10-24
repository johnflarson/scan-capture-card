//! Moxfield decks for the web UI's Decks page.
//!
//! Moxfield has no documented public API; this uses the JSON endpoints its
//! own site calls on `api2.moxfield.com`:
//! - `GET /v2/decks/search?authorUserNames=<user>&showIllegal=true` lists a
//!   user's public decks, including those not legal in their format;
//! - `GET /v3/decks/all/<publicId>` returns one deck with its cards (Scryfall
//!   IDs, type lines and prices included).
//!
//! Requests are spaced a second apart and cached under the cache directory:
//! the deck list for 15 minutes, each deck until Moxfield reports a newer
//! change. When Moxfield can't be reached the cached copy is served with a
//! warning.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use mtg_scan_shared::{Board, DeckCard, DeckSummary, DeckView, DecksView, Finish};
use reqwest::StatusCode;
use reqwest::blocking::Client;
use serde::Deserialize;

use crate::cache;

const API: &str = "https://api2.moxfield.com";
/// `.env` / environment setting: a Moxfield user name or profile link.
pub const USER_VAR: &str = "MTG_SCAN_MOXFIELD_USER";
const MIN_INTERVAL: Duration = Duration::from_secs(1);
const LIST_MAX_AGE: Duration = Duration::from_secs(15 * 60);
const PAGE_SIZE: u32 = 100;

/// One request at a time across the whole process, a second apart.
static LAST_REQUEST: Mutex<Option<Instant>> = Mutex::new(None);

/// Where the Moxfield account setting comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// `[decks] moxfield_user` in the settings file.
    Settings,
    /// `MTG_SCAN_MOXFIELD_USER` (normally from `.env`).
    Env,
}

/// The Moxfield setting as entered, and its source. The settings file wins
/// over the environment; an empty value there means no account.
pub fn configured(setting: Option<&str>) -> Option<(String, Source)> {
    choose(setting, std::env::var(USER_VAR).ok())
}

fn choose(setting: Option<&str>, env: Option<String>) -> Option<(String, Source)> {
    let set = |v: &str| Some(v.trim().to_string()).filter(|v| !v.is_empty());
    match setting {
        Some(v) => set(v).map(|v| (v, Source::Settings)),
        None => env.as_deref().and_then(set).map(|v| (v, Source::Env)),
    }
}

/// A user name from `Johnfredone` or `https://moxfield.com/users/Johnfredone`.
pub fn parse_user(input: &str) -> Option<String> {
    let input = input.trim();
    let name = match input.to_lowercase().find("moxfield.com/users/") {
        Some(at) => &input[at + "moxfield.com/users/".len()..],
        None if input.contains('/') => return None,
        None => input,
    };
    let name = name.split(['/', '?', '#']).next().unwrap_or_default();
    valid_id(name).then(|| name.to_string())
}

/// User names and deck IDs: letters, digits, `-`, `_`, `.`. Checked before
/// they go into a URL or a cache file name.
pub fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !s.starts_with('.')
}

pub struct Moxfield {
    http: Client,
}

impl Moxfield {
    pub fn new(http: Client) -> Self {
        Self { http }
    }

    fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        for attempt in 0..3 {
            {
                let mut last = LAST_REQUEST.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(t) = *last {
                    let elapsed = t.elapsed();
                    if elapsed < MIN_INTERVAL {
                        std::thread::sleep(MIN_INTERVAL - elapsed);
                    }
                }
                *last = Some(Instant::now());
            }
            let resp = self
                .http
                .get(format!("{API}{path}"))
                .header("Accept", "application/json")
                .send()
                .with_context(|| format!("Moxfield request failed: {path}"))?;
            match resp.status() {
                StatusCode::TOO_MANY_REQUESTS if attempt < 2 => {
                    std::thread::sleep(Duration::from_secs(2 << attempt));
                }
                StatusCode::NOT_FOUND => bail!("Moxfield has nothing at {path}"),
                _ => {
                    return resp
                        .error_for_status()
                        .with_context(|| format!("Moxfield request failed: {path}"))?
                        .json()
                        .with_context(|| format!("unexpected reply from Moxfield: {path}"));
                }
            }
        }
        bail!("Moxfield keeps asking to slow down; try again in a minute")
    }

    /// Every public deck of `user`, most recently changed first. Served from
    /// the cache when fresh (unless `refresh`), or when Moxfield fails (with
    /// a warning).
    pub fn decks(&self, user: &str, refresh: bool) -> DecksView {
        // "v2": lists cached before `showIllegal` (#45) lack decks; ignore them.
        let name = format!("moxfield-decks-v2-{}.json", user.to_lowercase());
        let profile_url = Some(format!("https://moxfield.com/users/{user}"));
        let view = |decks, warning| DecksView {
            user: Some(user.to_string()),
            profile_url: profile_url.clone(),
            decks,
            warning,
        };
        if !refresh && let Some(decks) = cache::read_fresh::<Vec<DeckSummary>>(&name, LIST_MAX_AGE)
        {
            return view(decks, None);
        }
        match self.fetch_decks(user) {
            Ok(decks) => {
                if let Err(e) = cache::write(&name, &decks) {
                    eprintln!("  ! {e:#}");
                }
                view(decks, None)
            }
            Err(e) => {
                let cached = cache::read_any::<Vec<DeckSummary>>(&name);
                let note = if cached.is_some() {
                    "showing the last list fetched"
                } else {
                    "no saved copy yet"
                };
                view(
                    cached.unwrap_or_default(),
                    Some(format!("Couldn't reach Moxfield ({e:#}); {note}.")),
                )
            }
        }
    }

    fn fetch_decks(&self, user: &str) -> Result<Vec<DeckSummary>> {
        let mut decks = Vec::new();
        let mut page = 1;
        loop {
            let reply: SearchPage = self.get(&search_path(user, page))?;
            // The search matches authors loosely; keep only this user's decks.
            decks.extend(
                reply
                    .data
                    .into_iter()
                    .filter(|d| {
                        d.created_by_user
                            .as_ref()
                            .is_none_or(|u| u.user_name.eq_ignore_ascii_case(user))
                    })
                    .filter(|d| valid_id(&d.public_id))
                    .map(RawSummary::into_summary),
            );
            if page >= reply.total_pages {
                return Ok(decks);
            }
            page += 1;
        }
    }

    /// The deck `summary` describes, with its cards: from the cache unless
    /// Moxfield reports a newer change.
    pub fn deck(&self, summary: &DeckSummary) -> Result<DeckView> {
        let name = format!("moxfield-deck-{}.json", summary.id);
        let cached = cache::read_any::<DeckView>(&name);
        if let Some(deck) = &cached
            && deck.summary.updated == summary.updated
        {
            return Ok(deck.clone());
        }
        match self.get::<RawDeck>(&format!("/v3/decks/all/{}", summary.id)) {
            Ok(raw) => {
                let deck = raw.into_view(summary.clone());
                if let Err(e) = cache::write(&name, &deck) {
                    eprintln!("  ! {e:#}");
                }
                Ok(deck)
            }
            Err(e) => match cached {
                Some(mut deck) => {
                    deck.warning = Some(format!(
                        "Couldn't reach Moxfield ({e:#}); showing the copy from {}.",
                        &deck.summary.updated[..10.min(deck.summary.updated.len())]
                    ));
                    Ok(deck)
                }
                None => Err(e),
            },
        }
    }
}

/// One page of `user`'s decks. Without `showIllegal=true` the search leaves
/// out decks Moxfield marks as not legal in their format (a 101-card
/// Commander deck, a banned card), which is often a third of a collection.
/// Private and unlisted decks need a Moxfield sign-in, so they never appear.
fn search_path(user: &str, page: u32) -> String {
    format!(
        "/v2/decks/search?authorUserNames={user}&pageNumber={page}&pageSize={PAGE_SIZE}\
         &sortType=updated&sortDirection=descending&showIllegal=true"
    )
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchPage {
    total_pages: u32,
    #[serde(default)]
    data: Vec<RawSummary>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawUser {
    user_name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSummary {
    public_id: String,
    name: String,
    #[serde(default)]
    format: String,
    #[serde(default)]
    public_url: String,
    #[serde(default)]
    color_identity: Vec<String>,
    #[serde(default)]
    mainboard_count: u32,
    #[serde(default)]
    sideboard_count: u32,
    #[serde(default)]
    last_updated_at_utc: String,
    created_by_user: Option<RawUser>,
}

impl RawSummary {
    fn into_summary(self) -> DeckSummary {
        let url = if self.public_url.is_empty() {
            format!("https://moxfield.com/decks/{}", self.public_id)
        } else {
            self.public_url
        };
        DeckSummary {
            id: self.public_id,
            name: self.name,
            format: self.format,
            url,
            colors: wubrg(self.color_identity),
            mainboard: self.mainboard_count,
            sideboard: self.sideboard_count,
            updated: self.last_updated_at_utc,
        }
    }
}

#[derive(Deserialize)]
struct RawDeck {
    #[serde(default)]
    boards: HashMap<String, RawBoard>,
}

#[derive(Deserialize)]
struct RawBoard {
    #[serde(default)]
    cards: HashMap<String, RawEntry>,
}

#[derive(Deserialize)]
struct RawEntry {
    quantity: u32,
    #[serde(default)]
    finish: String,
    card: RawCard,
}

/// Moxfield's card objects mirror Scryfall's (snake_case) fields.
#[derive(Deserialize)]
struct RawCard {
    scryfall_id: String,
    name: String,
    #[serde(default)]
    set: String,
    #[serde(default)]
    set_name: String,
    #[serde(default)]
    cn: String,
    #[serde(default)]
    type_line: String,
    #[serde(default)]
    rarity: String,
    #[serde(default)]
    colors: Vec<String>,
    #[serde(default)]
    card_faces: Vec<RawFace>,
    #[serde(default)]
    prices: RawPrices,
}

#[derive(Deserialize)]
struct RawFace {
    #[serde(default)]
    colors: Vec<String>,
}

#[derive(Deserialize, Default)]
struct RawPrices {
    usd: Option<f64>,
    usd_foil: Option<f64>,
    usd_etched: Option<f64>,
    ck: Option<f64>,
    ck_foil: Option<f64>,
}

impl RawDeck {
    fn into_view(self, summary: DeckSummary) -> DeckView {
        let mut cards: Vec<DeckCard> = self
            .boards
            .into_iter()
            .filter_map(|(name, board)| board_of(&name).map(|b| (b, board)))
            .flat_map(|(board, raw)| {
                raw.cards
                    .into_values()
                    .map(move |entry| entry.into_card(board))
            })
            .collect();
        cards.sort_by(|a, b| {
            a.board
                .cmp(&b.board)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        DeckView {
            summary,
            cards,
            warning: None,
        }
    }
}

/// Moxfield board name to [`Board`]; tokens aren't part of the deck.
fn board_of(name: &str) -> Option<Board> {
    Some(match name {
        "commanders" => Board::Commander,
        "companions" => Board::Companion,
        "mainboard" => Board::Main,
        "sideboard" => Board::Side,
        "maybeboard" => Board::Maybe,
        "tokens" => return None,
        _ => Board::Other,
    })
}

impl RawEntry {
    fn into_card(self, board: Board) -> DeckCard {
        let c = self.card;
        let finish = match self.finish.as_str() {
            "foil" => Finish::Foil,
            "etched" => Finish::Etched,
            _ => Finish::Nonfoil,
        };
        let p = &c.prices;
        let (tcgplayer, cardkingdom) = match finish {
            Finish::Nonfoil => (p.usd, p.ck),
            Finish::Foil => (p.usd_foil.or(p.usd), p.ck_foil.or(p.ck)),
            Finish::Etched => (p.usd_etched.or(p.usd_foil), p.ck_foil.or(p.ck)),
        };
        // Double-faced cards carry their colors on the faces.
        let colors = if c.colors.is_empty() {
            c.card_faces.into_iter().flat_map(|f| f.colors).collect()
        } else {
            c.colors
        };
        DeckCard {
            board,
            quantity: self.quantity,
            scryfall_id: c.scryfall_id,
            name: c.name,
            set_code: c.set.to_uppercase(),
            set_name: c.set_name,
            number: c.cn,
            finish,
            type_line: c.type_line,
            rarity: c.rarity,
            colors: wubrg(colors),
            tcgplayer,
            cardkingdom,
        }
    }
}

/// Color letters in WUBRG order, without duplicates.
fn wubrg(colors: Vec<String>) -> Vec<String> {
    "WUBRG"
        .chars()
        .map(String::from)
        .filter(|c| colors.contains(c))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_file_wins_over_env() {
        let env = || Some("FromEnv".to_string());
        assert_eq!(choose(None, env()), Some(("FromEnv".into(), Source::Env)));
        assert_eq!(
            choose(Some(" Mine "), env()),
            Some(("Mine".into(), Source::Settings))
        );
        // Cleared on the Settings page: no account, even with .env set.
        assert_eq!(choose(Some(""), env()), None);
        assert_eq!(choose(None, Some("  ".into())), None);
        assert_eq!(choose(None, None), None);
    }

    #[test]
    fn deck_search_includes_decks_moxfield_marks_illegal() {
        let path = search_path("Johnfredone", 2);
        assert!(path.contains("authorUserNames=Johnfredone"), "{path}");
        assert!(path.contains("pageNumber=2"), "{path}");
        assert!(path.contains("&showIllegal=true"), "{path}");
    }

    #[test]
    fn parses_user_names_and_profile_links() {
        let p = parse_user;
        assert_eq!(p("Johnfredone").as_deref(), Some("Johnfredone"));
        assert_eq!(
            p(" https://moxfield.com/users/Johnfredone ").as_deref(),
            Some("Johnfredone")
        );
        assert_eq!(
            p("https://www.moxfield.com/users/Some_User-1/decks?x=1").as_deref(),
            Some("Some_User-1")
        );
        assert_eq!(p("moxfield.com/users/abc").as_deref(), Some("abc"));
        assert_eq!(p(""), None);
        assert_eq!(p("https://moxfield.com/decks/abc"), None);
        assert_eq!(p("../etc"), None);
        assert_eq!(p("a b"), None);
    }

    #[test]
    fn reads_a_moxfield_deck() {
        let json = r#"{
          "boards": {
            "commanders": {"count": 1, "cards": {"x": {"quantity": 1, "finish": "nonFoil",
              "card": {"scryfall_id": "c1", "name": "Lathliss, Dragon Queen", "set": "jmp",
                "set_name": "Jumpstart", "cn": "340", "type_line": "Legendary Creature — Dragon",
                "rarity": "rare", "colors": ["R"], "prices": {"usd": 0.69, "ck": 0.59}}}}},
            "mainboard": {"count": 2, "cards": {
              "a": {"quantity": 1, "finish": "foil", "card": {"scryfall_id": "c2",
                "name": "Valakut Awakening // Valakut Stoneforge", "set": "znr", "cn": "174",
                "type_line": "Instant // Land", "rarity": "rare", "colors": [],
                "card_faces": [{"colors": ["R"]}, {"colors": []}],
                "prices": {"usd": 3.0, "usd_foil": 5.0, "ck": 4.0}}},
              "b": {"quantity": 10, "finish": "nonFoil", "card": {"scryfall_id": "c3",
                "name": "Mountain", "set": "iko", "cn": "271", "prices": {}}}}},
            "maybeboard": {"count": 0, "cards": {}},
            "tokens": {"count": 1, "cards": {"t": {"quantity": 1, "card": {"scryfall_id": "t1",
              "name": "Dragon"}}}}
          }
        }"#;
        let raw: RawDeck = serde_json::from_str(json).unwrap();
        let summary = RawSummary {
            public_id: "abc".into(),
            name: "Dragons".into(),
            format: "commander".into(),
            public_url: String::new(),
            color_identity: vec!["R".into()],
            mainboard_count: 99,
            sideboard_count: 0,
            last_updated_at_utc: "2026-10-13T18:55:33.017Z".into(),
            created_by_user: None,
        }
        .into_summary();
        assert_eq!(summary.url, "https://moxfield.com/decks/abc");
        let deck = raw.into_view(summary);
        let names: Vec<_> = deck.cards.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Lathliss, Dragon Queen",
                "Mountain",
                "Valakut Awakening // Valakut Stoneforge"
            ]
        );
        let valakut = &deck.cards[2];
        assert_eq!(valakut.board, Board::Main);
        assert_eq!(valakut.finish, Finish::Foil);
        assert_eq!(valakut.colors, ["R"]);
        assert_eq!(
            (valakut.tcgplayer, valakut.cardkingdom),
            (Some(5.0), Some(4.0))
        );
        assert_eq!(valakut.set_code, "ZNR");
        assert_eq!(deck.cards[0].board, Board::Commander);
        assert_eq!(deck.cards[1].tcgplayer, None);
    }
}
