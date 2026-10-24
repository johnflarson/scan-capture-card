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
    /// Collection copies of this exact printing (any finish) set aside for
    /// this line.
    pub exact: u32,
    /// All copies set aside for this line, any printing (at most the
    /// line's quantity).
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
/// printing. Within a deck each collection copy covers one deck copy (a deck
/// listing Mountains from two sets needs two Mountains); across decks the
/// same copy counts for each.
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

/// Sums at Moxfield's prices. Copies without a price aren't in a sum; they
/// are counted so the UI can say the sum is incomplete.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DeckPrice {
    pub tcgplayer: f64,
    pub cardkingdom: f64,
    /// Copies without a TCGplayer price.
    pub tcgplayer_unpriced: u32,
    /// Copies without a Card Kingdom price.
    pub cardkingdom_unpriced: u32,
}

impl DeckPrice {
    fn add(&mut self, card: &DeckCard, copies: u32) {
        let n = copies as f64;
        match card.tcgplayer {
            Some(p) => self.tcgplayer += p * n,
            None => self.tcgplayer_unpriced += copies,
        }
        match card.cardkingdom {
            Some(p) => self.cardkingdom += p * n,
            None => self.cardkingdom_unpriced += copies,
        }
    }
}

/// Lowercase name, so "Delver of Secrets // Insectile Aberration" from either
/// side matches.
fn name_key(name: &str) -> String {
    name.trim().to_lowercase()
}

impl DeckOwnership {
    pub fn of(deck: &DeckView, collection: &[CollectionRow]) -> DeckOwnership {
        // Copies not yet set aside for a line of this deck: per card name, the
        // copies of each printing. Exact matches are found by ID even if the
        // two sides spell the name differently.
        let mut pool: HashMap<String, Vec<(&str, u32)>> = HashMap::new();
        let mut name_of: HashMap<&str, String> = HashMap::new();
        for r in collection {
            let name = name_key(&r.name);
            let printings = pool.entry(name.clone()).or_default();
            match printings.iter_mut().find(|(id, _)| *id == r.scryfall_id) {
                Some((_, n)) => *n += r.quantity,
                None => printings.push((r.scryfall_id.as_str(), r.quantity)),
            }
            name_of.insert(r.scryfall_id.as_str(), name);
        }
        let mut take = |name: &str, id: Option<&str>, wanted: u32| -> u32 {
            let Some(printings) = pool.get_mut(name) else {
                return 0;
            };
            let mut taken = 0;
            for (pid, left) in printings.iter_mut() {
                if id.is_some_and(|id| id != *pid) {
                    continue;
                }
                let n = (*left).min(wanted - taken);
                *left -= n;
                taken += n;
            }
            taken
        };

        let mut exact = vec![0; deck.cards.len()];
        let mut any = vec![0; deck.cards.len()];
        // The deck itself first, the maybeboard gets what is left; each in
        // the deck's order.
        for counted in [true, false] {
            let lines: Vec<usize> = (0..deck.cards.len())
                .filter(|&i| deck.cards[i].board.counts() == counted)
                .collect();
            // First the exact printings, so a copy isn't used up by a line
            // it only matches by name…
            for &i in &lines {
                let card = &deck.cards[i];
                if let Some(name) = name_of.get(card.scryfall_id.as_str()) {
                    exact[i] = take(name, Some(&card.scryfall_id), card.quantity);
                    any[i] = exact[i];
                }
            }
            // …then any printing for what is still needed.
            for &i in &lines {
                let card = &deck.cards[i];
                any[i] += take(&name_key(&card.name), None, card.quantity - any[i]);
            }
        }

        let mut out = DeckOwnership {
            cards: Vec::with_capacity(deck.cards.len()),
            total: 0,
            owned: 0,
            value: DeckPrice::default(),
            missing: DeckPrice::default(),
        };
        for (i, card) in deck.cards.iter().enumerate() {
            let line = CardOwnership {
                card: card.clone(),
                exact: exact[i],
                any: any[i],
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

/// A deck that uses a card, in any printing.
#[derive(Debug, Clone, PartialEq)]
pub struct DeckUse {
    pub id: String,
    pub name: String,
    pub url: String,
    /// Copies in the deck (maybeboard not counted).
    pub quantity: u32,
    /// The boards the card is on, in board order.
    pub boards: Vec<Board>,
    /// Scryfall IDs of the printings the deck lists.
    pub printings: Vec<String>,
}

impl DeckUse {
    /// Only considered (maybeboard), not played.
    pub fn maybe_only(&self) -> bool {
        self.boards.iter().all(|b| !b.counts())
    }

    /// "Commander", "2 · Sideboard", "Maybeboard"; empty for one copy in the
    /// main deck.
    pub fn detail(&self) -> String {
        let boards: Vec<&str> = self
            .boards
            .iter()
            .filter(|b| **b != Board::Main)
            .map(|b| b.label())
            .collect();
        let copies = (self.quantity > 1).then(|| self.quantity.to_string());
        copies
            .into_iter()
            .chain(boards.iter().map(|b| b.to_string()))
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

/// Which decks use each card: by lowercase card name (so any printing
/// counts), the decks in play first, then those with it only on the
/// maybeboard, each by deck name.
pub fn decks_by_card<'a>(
    decks: impl IntoIterator<Item = &'a DeckView>,
) -> HashMap<String, Vec<DeckUse>> {
    let mut index: HashMap<String, Vec<DeckUse>> = HashMap::new();
    for deck in decks {
        for card in &deck.cards {
            let uses = index.entry(name_key(&card.name)).or_default();
            let i = match uses.iter().position(|u| u.id == deck.summary.id) {
                Some(i) => i,
                None => {
                    uses.push(DeckUse {
                        id: deck.summary.id.clone(),
                        name: deck.summary.name.clone(),
                        url: deck.summary.url.clone(),
                        quantity: 0,
                        boards: Vec::new(),
                        printings: Vec::new(),
                    });
                    uses.len() - 1
                }
            };
            let used = &mut uses[i];
            if card.board.counts() {
                used.quantity += card.quantity;
            }
            if !used.boards.contains(&card.board) {
                used.boards.push(card.board);
                used.boards.sort();
            }
            if !used.printings.contains(&card.scryfall_id) {
                used.printings.push(card.scryfall_id.clone());
            }
        }
    }
    for uses in index.values_mut() {
        uses.sort_by(|a, b| {
            a.maybe_only()
                .cmp(&b.maybe_only())
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
    }
    index
}

/// The decks using the card called `name` (see [`decks_by_card`]).
pub fn decks_using<'a>(index: &'a HashMap<String, Vec<DeckUse>>, name: &str) -> &'a [DeckUse] {
    index.get(&name_key(name)).map_or(&[], Vec::as_slice)
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

    fn deck(id: &str, name: &str, cards: Vec<DeckCard>) -> DeckView {
        DeckView {
            summary: DeckSummary {
                id: id.into(),
                name: name.into(),
                ..summary()
            },
            cards,
            warning: None,
        }
    }

    #[test]
    fn decks_by_card_lists_each_deck_once_any_printing() {
        let decks = [
            deck(
                "z",
                "Zombies",
                vec![
                    card(Board::Main, 1, "bolt-m10", "Lightning Bolt", None),
                    card(Board::Side, 2, "bolt-2xm", "Lightning Bolt", None),
                ],
            ),
            deck(
                "m",
                "maybe pile",
                vec![card(Board::Maybe, 1, "bolt-m10", "Lightning Bolt", None)],
            ),
            deck(
                "a",
                "Arcane",
                vec![
                    card(Board::Commander, 1, "bolt-m10", "Lightning Bolt", None),
                    card(Board::Main, 1, "x", "Counterspell", None),
                ],
            ),
        ];
        let index = decks_by_card(&decks);
        let bolt = decks_using(&index, " LIGHTNING bolt");
        let names: Vec<_> = bolt.iter().map(|u| u.name.as_str()).collect();
        // In play first, by name; the maybeboard-only deck last.
        assert_eq!(names, ["Arcane", "Zombies", "maybe pile"]);
        assert_eq!(bolt[1].quantity, 3);
        assert_eq!(bolt[1].boards, [Board::Main, Board::Side]);
        assert_eq!(bolt[1].printings, ["bolt-m10", "bolt-2xm"]);
        assert_eq!(bolt[1].detail(), "3 · Sideboard");
        assert_eq!(bolt[0].detail(), "Commander");
        assert!(bolt[2].maybe_only());
        assert_eq!(bolt[2].quantity, 0);
        assert_eq!(bolt[2].detail(), "Maybeboard");
        assert_eq!(decks_using(&index, "Counterspell")[0].detail(), "");
        assert!(decks_using(&index, "Sol Ring").is_empty());
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
            condition: crate::Condition::Nm,
            language: "en".into(),
            quantity,
            tcgplayer: None,
            cardmarket: None,
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
        // Bolt needs 1 and gets 1 of the 2 owned.
        assert_eq!(o.cards[1].any, 1);
        assert_eq!(o.cards[2].missing(), 3);
        // Maybeboard is listed but not counted.
        assert_eq!((o.total, o.owned), (16, 3));
        assert_eq!(o.percent(), 18);
        assert_eq!(o.value.tcgplayer, 0.5 + 1.0 + 1.0);
        assert_eq!(o.value.tcgplayer_unpriced, 10);
        assert_eq!(o.value.cardkingdom_unpriced, 10);
        assert_eq!(o.missing.tcgplayer, 0.75);
        assert_eq!(o.missing.cardkingdom, 1.5);
        assert_eq!(o.missing.tcgplayer_unpriced, 10);
    }

    #[test]
    fn copies_cover_one_deck_line_each() {
        // Mountains from two sets, one Mountain owned (a third printing).
        let deck = DeckView {
            summary: summary(),
            cards: vec![
                card(Board::Main, 1, "mountain-iko", "Mountain", Some(0.1)),
                card(Board::Main, 1, "mountain-m21", "Mountain", Some(0.1)),
                card(Board::Maybe, 1, "mountain-dom", "Mountain", Some(0.1)),
            ],
            warning: None,
        };
        let o = DeckOwnership::of(&deck, &[owned("mountain-dom", "Mountain", 1)]);
        let have: Vec<Have> = o.cards.iter().map(CardOwnership::have).collect();
        // The counted lines get the copy before the maybeboard's exact match.
        assert_eq!(have, [Have::OtherPrinting, Have::Missing, Have::Missing]);
        assert_eq!((o.total, o.owned), (2, 1));
        assert!((o.missing.tcgplayer - 0.1).abs() < 1e-9);

        // An exact match is kept for the line that names that printing.
        let o = DeckOwnership::of(&deck, &[owned("mountain-m21", "Mountain", 1)]);
        let have: Vec<Have> = o.cards.iter().map(CardOwnership::have).collect();
        assert_eq!(have, [Have::Missing, Have::Exact, Have::Missing]);
        assert_eq!(o.owned, 1);
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
