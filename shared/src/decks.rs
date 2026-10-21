//! Moxfield decks (`GET /api/decks`, `GET /api/decks/{id}`) and how much of
//! each deck the collection already holds. Ownership is computed in the
//! browser from the collection it has loaded.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::{CollectionRow, ColorGroup, Finish};

/// The configured Moxfield user's public decks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecksView {
    /// `None` when no Moxfield user is configured.
    pub user: Option<String>,
    pub profile_url: Option<String>,
    pub decks: Vec<DeckSummary>,
    /// Set when Moxfield couldn't be reached and the list is from the cache
    /// (or empty).
    pub warning: Option<String>,
}

/// One deck as listed (no cards).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeckSummary {
    /// Moxfield's public ID, as in `moxfield.com/decks/<id>`.
    pub id: String,
    pub name: String,
    /// e.g. "commander", "pauperEdh", "modern".
    pub format: String,
    pub url: String,
    /// Color identity letters, WUBRG order.
    pub colors: Vec<String>,
    /// Main deck cards (commanders not included).
    pub mainboard: u32,
    pub sideboard: u32,
    /// `YYYY-MM-DD` of the last change on Moxfield.
    pub updated: String,
}

impl DeckSummary {
    pub fn color_group(&self) -> ColorGroup {
        ColorGroup::of(&self.colors)
    }

    /// "Pauper EDH", "Commander", "Modern".
    pub fn format_label(&self) -> String {
        match self.format.as_str() {
            "pauperEdh" => "Pauper EDH".into(),
            "edh" | "commander" => "Commander".into(),
            "standardBrawl" | "standardbrawl" => "Standard Brawl".into(),
            other => {
                // camelCase -> "Camel Case"
                let mut label = String::new();
                for (i, c) in other.chars().enumerate() {
                    if i == 0 {
                        label.extend(c.to_uppercase());
                    } else if c.is_uppercase() {
                        label.push(' ');
                        label.push(c);
                    } else {
                        label.push(c);
                    }
                }
                label
            }
        }
    }
}

/// Where a card sits in the deck.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Board {
    Commander,
    Companion,
    Main,
    Side,
    /// Considered, not in the deck: listed but not counted or priced.
    Maybe,
    /// Attractions, stickers, signature spells and the like.
    Other,
}

impl Board {
    pub fn label(self) -> &'static str {
        match self {
            Board::Commander => "Commander",
            Board::Companion => "Companion",
            Board::Main => "Main deck",
            Board::Side => "Sideboard",
            Board::Maybe => "Maybeboard",
            Board::Other => "Other",
        }
    }

    /// Counted in the deck's size, ownership and price.
    pub fn counts(self) -> bool {
        self != Board::Maybe
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeckCard {
    pub board: Board,
    pub quantity: u32,
    pub scryfall_id: String,
    pub name: String,
    pub set_code: String,
    pub set_name: String,
    pub number: String,
    pub finish: Finish,
    pub type_line: String,
    pub rarity: String,
    pub colors: Vec<String>,
    /// Per copy, from Moxfield (TCGplayer market and Card Kingdom retail).
    pub tcgplayer: Option<f64>,
    pub cardkingdom: Option<f64>,
}

impl DeckCard {
    pub fn scryfall_url(&self) -> String {
        format!(
            "https://scryfall.com/card/{}/{}",
            self.set_code.to_lowercase(),
            self.number
        )
    }

    /// The card's main type (see [`crate::main_type`]).
    pub fn main_type(&self) -> &'static str {
        crate::main_type(&self.type_line)
    }
}

/// A deck with its cards.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeckView {
    pub summary: DeckSummary,
    pub cards: Vec<DeckCard>,
    /// Set when Moxfield couldn't be reached and this is a cached copy.
    pub warning: Option<String>,
}

/// How many copies of a deck card the collection holds.
#[derive(Debug, Clone, PartialEq)]
pub struct CardOwnership {
    pub card: DeckCard,
    /// Copies of this exact printing (any finish).
    pub exact: u32,
    /// Copies under this name, any printing or finish.
    pub any: u32,
}

/// What an ownership line shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Have {
    /// Enough copies of this printing.
    Exact,
    /// Enough copies, at least some of another printing.
    OtherPrinting,
    /// Some copies, not enough.
    Partial,
    Missing,
}

impl CardOwnership {
    /// Copies still needed (counting any printing as owned).
    pub fn missing(&self) -> u32 {
        self.card.quantity.saturating_sub(self.any)
    }

    pub fn have(&self) -> Have {
        if self.exact >= self.card.quantity {
            Have::Exact
        } else if self.any >= self.card.quantity {
            Have::OtherPrinting
        } else if self.any > 0 {
            Have::Partial
        } else {
            Have::Missing
        }
    }
}

/// A deck checked against the collection. A card counts as owned in any
/// printing; the same collection copy can count for several decks.
#[derive(Debug, Clone, PartialEq)]
pub struct DeckOwnership {
    pub cards: Vec<CardOwnership>,
    /// Copies in the deck (maybeboard excluded).
    pub total: u32,
    /// Of those, copies the collection covers.
    pub owned: u32,
    /// The whole deck at Moxfield's prices.
    pub value: DeckPrice,
    /// The copies still missing.
    pub missing: DeckPrice,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DeckPrice {
    pub tcgplayer: f64,
    pub cardkingdom: f64,
    /// Copies without a TCGplayer price (not in the sum).
    pub unpriced: u32,
}

impl DeckPrice {
    fn add(&mut self, card: &DeckCard, copies: u32) {
        let n = copies as f64;
        match card.tcgplayer {
            Some(p) => self.tcgplayer += p * n,
            None => self.unpriced += copies,
        }
        self.cardkingdom += card.cardkingdom.unwrap_or(0.0) * n;
    }
}

/// Lowercase name, so "Delver of Secrets // Insectile Aberration" from either
/// side matches.
fn name_key(name: &str) -> String {
    name.trim().to_lowercase()
}

impl DeckOwnership {
    pub fn of(deck: &DeckView, collection: &[CollectionRow]) -> DeckOwnership {
        let mut by_id: HashMap<&str, u32> = HashMap::new();
        let mut by_name: HashMap<String, u32> = HashMap::new();
        for r in collection {
            *by_id.entry(r.scryfall_id.as_str()).or_default() += r.quantity;
            *by_name.entry(name_key(&r.name)).or_default() += r.quantity;
        }

        let mut out = DeckOwnership {
            cards: Vec::with_capacity(deck.cards.len()),
            total: 0,
            owned: 0,
            value: DeckPrice::default(),
            missing: DeckPrice::default(),
        };
        for card in &deck.cards {
            let exact = by_id.get(card.scryfall_id.as_str()).copied().unwrap_or(0);
            let any = by_name
                .get(&name_key(&card.name))
                .copied()
                .unwrap_or(0)
                .max(exact);
            let line = CardOwnership {
                card: card.clone(),
                exact,
                any,
            };
            if card.board.counts() {
                out.total += card.quantity;
                out.owned += card.quantity - line.missing();
                out.value.add(card, card.quantity);
                out.missing.add(card, line.missing());
            }
            out.cards.push(line);
        }
        out
    }

    /// Share of the deck owned, 0-100.
    pub fn percent(&self) -> u32 {
        if self.total == 0 {
            100
        } else {
            (self.owned as f64 * 100.0 / self.total as f64).floor() as u32
        }
    }
}

/// Deck statistics for the landing page.
#[derive(Debug, Clone, PartialEq)]
pub struct DeckStats {
    pub decks: usize,
    /// Decks per color identity group, WUBRG + multicolor + colorless.
    pub by_color: Vec<(ColorGroup, usize)>,
    /// Decks per format, most first.
    pub by_format: Vec<(String, usize)>,
}

impl DeckStats {
    pub fn of(decks: &[DeckSummary]) -> DeckStats {
        let by_color = ColorGroup::ALL
            .into_iter()
            .map(|g| (g, decks.iter().filter(|d| d.color_group() == g).count()))
            .collect();
        let mut formats: Vec<(String, usize)> = Vec::new();
        for d in decks {
            let label = d.format_label();
            match formats.iter_mut().find(|(f, _)| *f == label) {
                Some((_, n)) => *n += 1,
                None => formats.push((label, 1)),
            }
        }
        formats.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        DeckStats {
            decks: decks.len(),
            by_color,
            by_format: formats,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary() -> DeckSummary {
        DeckSummary {
            id: "abc".into(),
            name: "Dragons".into(),
            format: "commander".into(),
            url: "https://moxfield.com/decks/abc".into(),
            colors: vec!["R".into()],
            mainboard: 99,
            sideboard: 0,
            updated: "2026-10-13".into(),
        }
    }

    fn card(board: Board, quantity: u32, id: &str, name: &str, tcg: Option<f64>) -> DeckCard {
        DeckCard {
            board,
            quantity,
            scryfall_id: id.into(),
            name: name.into(),
            set_code: "SET".into(),
            set_name: String::new(),
            number: "1".into(),
            finish: Finish::Nonfoil,
            type_line: "Instant".into(),
            rarity: "common".into(),
            colors: vec![],
            tcgplayer: tcg,
            cardkingdom: tcg.map(|p| p * 2.0),
        }
    }

    fn owned(id: &str, name: &str, quantity: u32) -> CollectionRow {
        CollectionRow {
            scryfall_id: id.into(),
            name: name.into(),
            set_code: String::new(),
            set_name: String::new(),
            number: String::new(),
            rarity: String::new(),
            colors: vec![],
            type_line: String::new(),
            finish: Finish::Nonfoil,
            quantity,
            tcgplayer: None,
            cardkingdom: None,
            cardkingdom_buylist: None,
            added: String::new(),
            priced: None,
            scryfall_url: String::new(),
            tcgplayer_url: None,
            cardkingdom_url: None,
        }
    }

    #[test]
    fn ownership_counts_any_printing_and_prices_what_is_missing() {
        let deck = DeckView {
            summary: summary(),
            cards: vec![
                card(
                    Board::Commander,
                    1,
                    "lathliss",
                    "Lathliss, Dragon Queen",
                    Some(0.5),
                ),
                card(Board::Main, 1, "bolt-m11", "Lightning Bolt", Some(1.0)),
                card(Board::Main, 4, "shock", "Shock", Some(0.25)),
                card(Board::Main, 10, "mountain", "Mountain", None),
                card(Board::Maybe, 1, "dragon", "Shivan Dragon", Some(3.0)),
            ],
            warning: None,
        };
        let collection = [
            owned("lathliss", "Lathliss, Dragon Queen", 1),
            // Another printing of Bolt, in two finishes.
            owned("bolt-a25", "Lightning Bolt", 1),
            owned("bolt-a25", "lightning bolt", 1),
            owned("shock", "Shock", 1),
        ];
        let o = DeckOwnership::of(&deck, &collection);
        let have: Vec<Have> = o.cards.iter().map(CardOwnership::have).collect();
        assert_eq!(
            have,
            [
                Have::Exact,
                Have::OtherPrinting,
                Have::Partial,
                Have::Missing,
                Have::Missing
            ]
        );
        assert_eq!(o.cards[1].any, 2);
        assert_eq!(o.cards[2].missing(), 3);
        // Maybeboard is listed but not counted.
        assert_eq!((o.total, o.owned), (16, 3));
        assert_eq!(o.percent(), 18);
        assert_eq!(o.value.tcgplayer, 0.5 + 1.0 + 1.0);
        assert_eq!(o.value.unpriced, 10);
        assert_eq!(o.missing.tcgplayer, 0.75);
        assert_eq!(o.missing.cardkingdom, 1.5);
        assert_eq!(o.missing.unpriced, 10);
    }

    #[test]
    fn deck_stats_and_format_labels() {
        let mut a = summary();
        let mut b = summary();
        b.format = "pauperEdh".into();
        b.colors = vec!["U".into(), "G".into()];
        let mut c = summary();
        c.format = "modern".into();
        c.colors = vec![];
        a.format = "commander".into();
        let s = DeckStats::of(&[a, b, c.clone()]);
        assert_eq!(s.decks, 3);
        let count = |g| s.by_color.iter().find(|(c, _)| *c == g).unwrap().1;
        assert_eq!(
            (
                count(ColorGroup::Red),
                count(ColorGroup::Multicolor),
                count(ColorGroup::Colorless)
            ),
            (1, 1, 1)
        );
        assert_eq!(s.by_format.len(), 3);
        assert_eq!(c.format_label(), "Modern");
        let mut d = summary();
        d.format = "pauperEdh".into();
        assert_eq!(d.format_label(), "Pauper EDH");
        d.format = "oathbreaker".into();
        assert_eq!(d.format_label(), "Oathbreaker");
        d.format = "historicBrawl".into();
        assert_eq!(d.format_label(), "Historic Brawl");
    }
}
