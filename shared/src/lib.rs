//! Types exchanged between the mtg-scan server and its web UI as JSON.
//!
//! Both sides compile this crate, so the API can't drift: a field renamed
//! here is a compile error on whichever side wasn't updated.

use serde::{Deserialize, Serialize};

mod decks;
mod settings;
pub use decks::*;
pub use settings::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// One card inside the on-screen guide.
    Single,
    /// Every card laid out in view.
    Table,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Finish {
    Nonfoil,
    Foil,
    Etched,
}

impl Finish {
    pub const ALL: [Finish; 3] = [Finish::Nonfoil, Finish::Foil, Finish::Etched];

    pub fn label(self) -> &'static str {
        match self {
            Finish::Nonfoil => "Nonfoil",
            Finish::Foil => "Foil",
            Finish::Etched => "Etched",
        }
    }
}

/// Scanner status, sent on connect and whenever it changes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateView {
    /// Recognizer and database are loaded; scans can start.
    pub ready: bool,
    /// A scan (or undo) is in progress.
    pub busy: bool,
    pub auto: bool,
    pub mode: Mode,
    pub finish: Finish,
    /// Started with `--dry-run`: nothing is ever saved.
    pub dry_run: bool,
    /// Scans are added to the collection (the "Save to collection" switch).
    /// Off: cards are identified and priced only, and nothing is written.
    pub save: bool,
    /// One-line human-readable status.
    pub status: String,
    pub camera: String,
    pub recognizer: String,
    /// Where cards are saved, e.g. the database location.
    pub collection: String,
    /// Card Kingdom prices are recorded (`[prices] card_kingdom`).
    #[serde(default)]
    pub card_kingdom: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CardOutcome {
    /// Saved to the collection.
    Added,
    /// Identified and priced only (dry run).
    Priced,
    /// The printing is uncertain: waiting for the user to accept (possibly
    /// another printing) or discard it. Not saved yet.
    Held,
    /// Held, then discarded by the user. Never saved.
    Discarded,
}

/// A card identified during this session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CardView {
    /// Increases with every card; stable key for lists.
    pub seq: u64,
    /// Local time, `HH:MM:SS`.
    pub time: String,
    pub outcome: CardOutcome,
    /// The identified printing.
    pub scryfall_id: String,
    pub name: String,
    pub set_code: String,
    pub set_name: String,
    pub number: String,
    pub rarity: String,
    pub finish: Finish,
    /// Copies in the collection after this scan (None for dry runs).
    pub quantity: Option<u32>,
    pub tcgplayer: Option<f64>,
    /// Cardmarket price in euros.
    #[serde(default)]
    pub cardmarket: Option<f64>,
    pub cardkingdom: Option<f64>,
    pub cardkingdom_buylist: Option<f64>,
    pub tcgplayer_url: Option<String>,
    pub cardkingdom_url: Option<String>,
    pub scryfall_url: String,
    pub image_url: Option<String>,
    /// "OCR" or "LLM".
    pub method: String,
    /// How sure the match is, e.g. "exact printing".
    pub quality: String,
    /// The printing may be wrong; worth a manual check.
    pub needs_review: bool,
    pub note: Option<String>,
    /// Removed again with Undo.
    pub undone: bool,
}

impl CardView {
    /// The printing and finish, e.g. to hide every copy of one card.
    pub fn key(&self) -> String {
        format!("{}/{}", self.scryfall_id, self.finish.label())
    }

    /// Kept this session: saved, or priced while not saving.
    pub fn kept(&self) -> bool {
        !self.undone && matches!(self.outcome, CardOutcome::Added | CardOutcome::Priced)
    }
}

/// The `n` most expensive kept cards by `price`, most expensive first: each
/// printing and finish once, without cards whose key is in `hidden` or that
/// have no price.
pub fn top_cards(
    cards: &[CardView],
    price: fn(&CardView) -> Option<f64>,
    hidden: &[String],
    n: usize,
) -> Vec<CardView> {
    let mut ranked: Vec<&CardView> = cards
        .iter()
        .filter(|c| c.kept() && price(c).is_some() && !hidden.contains(&c.key()))
        .collect();
    ranked.sort_by(|a, b| {
        price(b)
            .partial_cmp(&price(a))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut top: Vec<CardView> = Vec::new();
    for c in ranked {
        if top.len() == n {
            break;
        }
        if !top.iter().any(|t| t.key() == c.key()) {
            top.push(c.clone());
        }
    }
    top
}

/// Everything the UI needs when it (re)connects.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionView {
    pub state: StateView,
    /// Oldest first.
    pub cards: Vec<CardView>,
}

/// Pushed to the UI over Server-Sent Events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
// Short-lived wire messages: boxing the large variant would buy nothing.
#[allow(clippy::large_enum_variant)]
pub enum ServerEvent {
    State(StateView),
    Card(CardView),
    /// Cards removed again by Undo.
    Undone {
        seqs: Vec<u64>,
    },
    /// The session list was cleared: start fresh.
    Cleared,
    /// Background progress or a problem worth showing briefly.
    Info {
        message: String,
        error: bool,
    },
}

/// Sent by the UI to `POST /api/command`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientCommand {
    ScanNow,
    /// Single-card mode: re-identify the current card with the vision LLM.
    RetryLlm,
    Undo,
    /// Table mode: forget the last pass so every card is added again.
    Reset,
    SetFinish {
        finish: Finish,
    },
    SetAuto {
        auto: bool,
    },
    SetMode {
        mode: Mode,
    },
    /// Add scans to the collection, or only identify and price them.
    SetSave {
        save: bool,
    },
    /// Empty the session list and forget the last scan (table memory, undo).
    /// Cards already saved stay in the collection.
    Clear,
    /// Save a held card (seq from its [`CardView`]) as this printing and finish:
    /// the identified one, or another the user picked.
    Accept {
        seq: u64,
        scryfall_id: String,
        finish: Finish,
    },
    /// Drop a held card without saving it.
    Discard {
        seq: u64,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Totals {
    pub printings: usize,
    pub cards: u32,
    pub tcgplayer: f64,
    /// Euros.
    #[serde(default)]
    pub cardmarket: f64,
    pub cardkingdom: f64,
    pub cardkingdom_buylist: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CollectionRow {
    pub scryfall_id: String,
    pub name: String,
    pub set_code: String,
    pub set_name: String,
    pub number: String,
    pub rarity: String,
    /// Color letters (`W`, `U`, `B`, `R`, `G`); empty for colorless.
    pub colors: Vec<String>,
    /// e.g. "Legendary Creature — Phyrexian Praetor"; empty if unknown.
    pub type_line: String,
    pub finish: Finish,
    pub quantity: u32,
    pub tcgplayer: Option<f64>,
    /// Cardmarket price in euros.
    #[serde(default)]
    pub cardmarket: Option<f64>,
    pub cardkingdom: Option<f64>,
    pub cardkingdom_buylist: Option<f64>,
    /// `YYYY-MM-DD` the row was first added.
    pub added: String,
    /// `YYYY-MM-DD` of the latest price.
    pub priced: Option<String>,
    pub scryfall_url: String,
    pub tcgplayer_url: Option<String>,
    pub cardkingdom_url: Option<String>,
}

impl Totals {
    pub fn of(rows: &[CollectionRow]) -> Totals {
        rows.iter().fold(Totals::default(), |mut t, r| {
            let q = r.quantity as f64;
            t.printings += 1;
            t.cards += r.quantity;
            t.tcgplayer += r.tcgplayer.unwrap_or(0.0) * q;
            t.cardmarket += r.cardmarket.unwrap_or(0.0) * q;
            t.cardkingdom += r.cardkingdom.unwrap_or(0.0) * q;
            t.cardkingdom_buylist += r.cardkingdom_buylist.unwrap_or(0.0) * q;
            t
        })
    }
}

impl CollectionRow {
    /// Whether Scryfall's card details (type line, colors) were loaded. Every
    /// card has a type line, so an empty one means they are missing (e.g.
    /// Scryfall was unreachable), and empty colors then mean "unknown", not
    /// colorless.
    pub fn details_known(&self) -> bool {
        !self.type_line.trim().is_empty()
    }

    /// `None` when the card details are missing.
    pub fn color_group(&self) -> Option<ColorGroup> {
        self.details_known().then(|| ColorGroup::of(&self.colors))
    }

    /// The main type, or "Unknown" when the card details are missing.
    pub fn main_type(&self) -> &'static str {
        if self.details_known() {
            main_type(&self.type_line)
        } else {
            UNKNOWN
        }
    }

    /// TCGplayer value of all copies.
    pub fn value(&self) -> f64 {
        self.tcgplayer.unwrap_or(0.0) * self.quantity as f64
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CollectionView {
    pub location: String,
    pub rows: Vec<CollectionRow>,
    pub totals: Totals,
    /// TCGplayer value change over the last week (from the price history).
    pub value_change: Option<ValueChange>,
    /// Card Kingdom prices are recorded (`[prices] card_kingdom`).
    #[serde(default)]
    pub card_kingdom: bool,
}

/// How the value of cards owned now changed since `since`, counting only
/// cards that had a TCGplayer price on both dates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ValueChange {
    /// `YYYY-MM-DD`.
    pub since: String,
    pub before: f64,
    pub now: f64,
    /// Rows (printing + finish) compared.
    pub rows: u32,
}

/// The color grouping used for statistics and filters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ColorGroup {
    White,
    Blue,
    Black,
    Red,
    Green,
    Multicolor,
    Colorless,
}

impl ColorGroup {
    pub const ALL: [ColorGroup; 7] = [
        ColorGroup::White,
        ColorGroup::Blue,
        ColorGroup::Black,
        ColorGroup::Red,
        ColorGroup::Green,
        ColorGroup::Multicolor,
        ColorGroup::Colorless,
    ];

    pub fn of(colors: &[String]) -> ColorGroup {
        match colors {
            [] => ColorGroup::Colorless,
            [one] => match one.as_str() {
                "W" => ColorGroup::White,
                "U" => ColorGroup::Blue,
                "B" => ColorGroup::Black,
                "R" => ColorGroup::Red,
                "G" => ColorGroup::Green,
                _ => ColorGroup::Colorless,
            },
            _ => ColorGroup::Multicolor,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ColorGroup::White => "White",
            ColorGroup::Blue => "Blue",
            ColorGroup::Black => "Black",
            ColorGroup::Red => "Red",
            ColorGroup::Green => "Green",
            ColorGroup::Multicolor => "Multicolor",
            ColorGroup::Colorless => "Colorless",
        }
    }

    /// Short CSS class name, e.g. `w`.
    pub fn class(self) -> &'static str {
        match self {
            ColorGroup::White => "w",
            ColorGroup::Blue => "u",
            ColorGroup::Black => "b",
            ColorGroup::Red => "r",
            ColorGroup::Green => "g",
            ColorGroup::Multicolor => "m",
            ColorGroup::Colorless => "c",
        }
    }
}

/// Main card types, most specific first: an "Artifact Creature" is a
/// Creature, an "Artifact Land" a Land.
pub const CARD_TYPES: [&str; 9] = [
    "Creature",
    "Planeswalker",
    "Battle",
    "Instant",
    "Sorcery",
    "Land",
    "Artifact",
    "Enchantment",
    "Other",
];

/// The main type of a type line (front face for double-faced cards).
pub fn main_type(type_line: &str) -> &'static str {
    let front = type_line.split(" // ").next().unwrap_or_default();
    let types = front.split('—').next().unwrap_or_default();
    CARD_TYPES[..CARD_TYPES.len() - 1]
        .iter()
        .find(|t| types.split_whitespace().any(|w| w == **t))
        .copied()
        .unwrap_or("Other")
}

/// Rarities in display order.
pub const RARITIES: [&str; 6] = ["common", "uncommon", "rare", "mythic", "special", "bonus"];

/// "mythic" -> "Mythic".
pub fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// A Collection page column that rows can be filtered and grouped by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Facet {
    Type,
    Rarity,
    Color,
}

impl Facet {
    pub const ALL: [Facet; 3] = [Facet::Type, Facet::Rarity, Facet::Color];

    pub fn label(self) -> &'static str {
        match self {
            Facet::Type => "Type",
            Facet::Rarity => "Rarity",
            Facet::Color => "Color",
        }
    }

    /// Every value, in display order.
    pub fn values(self) -> Vec<String> {
        let known: Vec<String> = match self {
            Facet::Type => CARD_TYPES.iter().map(|t| t.to_string()).collect(),
            Facet::Rarity => RARITIES.iter().map(|r| capitalize(r)).collect(),
            Facet::Color => ColorGroup::ALL
                .iter()
                .map(|g| g.label().to_string())
                .collect(),
        };
        known.into_iter().chain([UNKNOWN.to_string()]).collect()
    }

    /// Where `value` sorts; values this list doesn't know go last.
    fn rank(self, value: &str) -> usize {
        let values = self.values();
        values
            .iter()
            .position(|v| v == value)
            .unwrap_or(values.len())
    }

    /// `rows` split by this column: groups in display order, rows within a
    /// group in their original order. Only values present appear.
    pub fn group(self, rows: &[CollectionRow]) -> Vec<(String, Vec<CollectionRow>)> {
        let mut groups = std::collections::BTreeMap::<(usize, String), Vec<CollectionRow>>::new();
        for r in rows {
            let value = r.facet(self);
            groups
                .entry((self.rank(&value), value))
                .or_default()
                .push(r.clone());
        }
        groups.into_iter().map(|((_, v), rows)| (v, rows)).collect()
    }

    /// The values present in `rows`, in display order, with their copies.
    pub fn present(self, rows: &[CollectionRow]) -> Vec<(String, u32)> {
        self.group(rows)
            .into_iter()
            .map(|(v, rows)| (v, rows.iter().map(|r| r.quantity).sum()))
            .collect()
    }
}

/// Type, rarity or color of a row whose card details couldn't be loaded.
pub const UNKNOWN: &str = "Unknown";

impl CollectionRow {
    /// "Mythic", or "Unknown" when the row has none.
    pub fn rarity_label(&self) -> String {
        match self.rarity.trim() {
            "" => UNKNOWN.to_string(),
            r => capitalize(&r.to_lowercase()),
        }
    }

    /// This row's value in a filterable column, as shown in the table.
    pub fn facet(&self, facet: Facet) -> String {
        match facet {
            Facet::Type => self.main_type().to_string(),
            Facet::Rarity => self.rarity_label(),
            Facet::Color => self
                .color_group()
                .map_or(UNKNOWN, ColorGroup::label)
                .to_string(),
        }
    }

    /// Whether one lowercase search word matches: anywhere in the name, set
    /// or type line, or at the start of the rarity, the color group or one of
    /// the card's colors (so "common" doesn't find uncommons).
    fn matches_word(&self, word: &str) -> bool {
        let text = [&self.name, &self.set_code, &self.set_name, &self.type_line];
        if text.iter().any(|t| t.to_lowercase().contains(word)) {
            return true;
        }
        let mut terms = vec![self.rarity_label(), self.facet(Facet::Color)];
        terms.extend(
            self.colors
                .iter()
                .map(|c| ColorGroup::of(std::slice::from_ref(c)).label().to_string()),
        );
        terms.iter().any(|t| t.to_lowercase().starts_with(word))
    }
}

/// What the Collection page shows: search words plus values picked per
/// column. Every search word must match the row (see `matches_word`); within a
/// column any picked value matches (OR); every column with picks must match
/// (AND).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RowFilter {
    pub text: String,
    pub picked: std::collections::BTreeMap<Facet, std::collections::BTreeSet<String>>,
}

impl RowFilter {
    pub fn is_picked(&self, facet: Facet, value: &str) -> bool {
        self.picked.get(&facet).is_some_and(|p| p.contains(value))
    }

    /// Pick `value`, or unpick it if it was picked.
    pub fn toggle(&mut self, facet: Facet, value: &str) {
        let picks = self.picked.entry(facet).or_default();
        if !picks.remove(value) {
            picks.insert(value.to_string());
        }
        if picks.is_empty() {
            self.picked.remove(&facet);
        }
    }

    pub fn has_picks(&self) -> bool {
        !self.picked.is_empty()
    }

    /// Whether anything is hidden: search text or picks.
    pub fn is_active(&self) -> bool {
        !self.text.trim().is_empty() || self.has_picks()
    }

    pub fn matches(&self, row: &CollectionRow) -> bool {
        let picks_match = self
            .picked
            .iter()
            .all(|(facet, values)| values.contains(&row.facet(*facet)));
        if !picks_match {
            return false;
        }
        self.text
            .to_lowercase()
            .split_whitespace()
            .all(|w| row.matches_word(w))
    }
}

/// One slice of a breakdown: copies and their TCGplayer value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Slice {
    pub label: String,
    pub cards: u32,
    pub value: f64,
}

/// Collection statistics for the landing page.
#[derive(Debug, Clone, PartialEq)]
pub struct Stats {
    pub totals: Totals,
    /// Every color group, in WUBRG + multicolor + colorless order. Rows whose
    /// colors are unknown are left out.
    pub by_color: Vec<(ColorGroup, Slice)>,
    /// Types present, most copies first.
    pub by_type: Vec<Slice>,
    /// Rarities present, common to mythic.
    pub by_rarity: Vec<Slice>,
    pub by_finish: Vec<Slice>,
    /// The five sets with the most copies.
    pub top_sets: Vec<Slice>,
    /// The five most valuable rows by TCGplayer price per copy.
    pub most_valuable: Vec<CollectionRow>,
    /// The five rows added most recently.
    pub recently_added: Vec<CollectionRow>,
}

impl Stats {
    pub fn of(rows: &[CollectionRow]) -> Stats {
        fn tally<K: Ord + Clone>(
            rows: &[CollectionRow],
            key: impl Fn(&CollectionRow) -> K,
        ) -> std::collections::BTreeMap<K, (u32, f64)> {
            let mut map = std::collections::BTreeMap::new();
            for r in rows {
                let e = map.entry(key(r)).or_insert((0, 0.0));
                e.0 += r.quantity;
                e.1 += r.value();
            }
            map
        }
        let slice = |label: String, (cards, value): (u32, f64)| Slice {
            label,
            cards,
            value,
        };

        let colors = tally(rows, CollectionRow::color_group);
        let by_color = ColorGroup::ALL
            .into_iter()
            .map(|g| {
                let t = colors.get(&Some(g)).copied().unwrap_or_default();
                (g, slice(g.label().to_string(), t))
            })
            .collect();

        let mut by_type: Vec<Slice> = tally(rows, |r| r.main_type())
            .into_iter()
            .map(|(k, t)| slice(k.to_string(), t))
            .collect();
        by_type.sort_by(|a, b| b.cards.cmp(&a.cards).then_with(|| a.label.cmp(&b.label)));

        let rarity_rank = |r: &str| {
            RARITIES
                .iter()
                .position(|x| *x == r)
                .unwrap_or(RARITIES.len())
        };
        let mut by_rarity: Vec<(String, (u32, f64))> = tally(rows, |r| r.rarity.to_lowercase())
            .into_iter()
            .collect();
        by_rarity.sort_by_key(|(k, _)| rarity_rank(k));
        let by_rarity = by_rarity
            .into_iter()
            .map(|(k, t)| slice(capitalize(&k), t))
            .collect();

        let by_finish = tally(rows, |r| r.finish)
            .into_iter()
            .map(|(k, t)| slice(k.label().to_string(), t))
            .collect();

        let mut top_sets: Vec<Slice> = tally(rows, |r| (r.set_code.clone(), r.set_name.clone()))
            .into_iter()
            .map(|((code, name), t)| {
                let label = if name.is_empty() {
                    code
                } else {
                    format!("{name} ({code})")
                };
                slice(label, t)
            })
            .collect();
        top_sets.sort_by(|a, b| b.cards.cmp(&a.cards).then_with(|| a.label.cmp(&b.label)));
        top_sets.truncate(5);

        let mut most_valuable: Vec<CollectionRow> = rows
            .iter()
            .filter(|r| r.tcgplayer.is_some())
            .cloned()
            .collect();
        most_valuable.sort_by(|a, b| {
            b.tcgplayer
                .partial_cmp(&a.tcgplayer)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        most_valuable.truncate(5);

        let mut recently_added = rows.to_vec();
        // Stable sort keeps the collection's name order within a day.
        recently_added.sort_by(|a, b| b.added.cmp(&a.added));
        recently_added.truncate(5);

        Stats {
            totals: Totals::of(rows),
            by_color,
            by_type,
            by_rarity,
            by_finish,
            top_sets,
            most_valuable,
            recently_added,
        }
    }
}

/// A printing, for the edit dialog: `GET /api/printing?q=<Scryfall link or ID>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrintingView {
    pub scryfall_id: String,
    pub name: String,
    pub set_code: String,
    pub set_name: String,
    pub number: String,
    pub rarity: String,
    /// Finishes this printing exists in.
    pub finishes: Vec<Finish>,
    pub image_url: Option<String>,
    pub scryfall_url: String,
}

/// `POST /api/collection/add`: copies of a printing to add by hand (merged
/// into the row for that printing and finish if there is one). Answered with
/// an [`EditResult`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AddCard {
    pub scryfall_id: String,
    pub finish: Finish,
    pub quantity: u32,
}

/// `POST /api/collection/edit`: what one row should become.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RowEdit {
    /// The row being edited.
    pub scryfall_id: String,
    pub finish: Finish,
    /// The printing and finish it should have (unchanged to keep them).
    pub to_scryfall_id: String,
    pub to_finish: Finish,
    /// Copies; 0 removes the row.
    pub quantity: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditResult {
    pub message: String,
    /// The row as it now stands (merged with an existing row when moved onto
    /// one); `None` when it was removed.
    pub row: Option<CollectionRow>,
}

/// `$1.23`, or an em dash when unknown.
pub fn usd(v: Option<f64>) -> String {
    // `+ 0.0` turns -0.0 (an empty f64 sum) into 0.0, so it isn't shown as "$-0.00".
    v.map(|p| format!("${:.2}", p + 0.0))
        .unwrap_or_else(|| "—".into())
}

/// `€1.23`, or an em dash when unknown.
pub fn eur(v: Option<f64>) -> String {
    v.map(|p| format!("€{:.2}", p + 0.0))
        .unwrap_or_else(|| "—".into())
}

/// A printing's front image ("normal", 488×680) straight from Scryfall's
/// image CDN. Grids use it rather than the API's image redirect, which
/// counts against the API rate limit.
pub fn card_image(scryfall_id: &str) -> String {
    let mut chars = scryfall_id.chars();
    let (a, b) = (chars.next().unwrap_or('0'), chars.next().unwrap_or('0'));
    format!("https://cards.scryfall.io/normal/front/{a}/{b}/{scryfall_id}.jpg")
}

/// "42%", or "<1%" for a share that would round to 0%.
pub fn percent(share: f64) -> String {
    if share > 0.0 && share < 0.005 {
        "<1%".into()
    } else {
        format!("{:.0}%", share * 100.0)
    }
}

/// One slice of a pie chart: a value's share of the cards.
#[derive(Debug, Clone, PartialEq)]
pub struct PieSlice {
    pub label: String,
    pub cards: u32,
    /// Share of all cards, 0-1.
    pub share: f64,
    /// SVG path in a `-50 -50 100 100` view box, clockwise from 12 o'clock.
    pub path: String,
}

const PIE_RADIUS: f64 = 48.0;

/// `(value, cards)` as pie slices, in the given order; values with no cards
/// are left out.
pub fn pie(values: &[(String, u32)]) -> Vec<PieSlice> {
    let total: u32 = values.iter().map(|(_, n)| n).sum();
    let mut done = 0;
    values
        .iter()
        .filter(|(_, n)| *n > 0)
        .map(|(label, n)| {
            let start = done as f64 / total as f64;
            done += n;
            let end = done as f64 / total as f64;
            PieSlice {
                label: label.clone(),
                cards: *n,
                share: end - start,
                path: pie_path(start, end),
            }
        })
        .collect()
}

/// The wedge from `start` to `end` (fractions of the circle).
fn pie_path(start: f64, end: f64) -> String {
    let r = PIE_RADIUS;
    if end - start >= 1.0 {
        // An arc can't end where it starts, so a whole circle is two halves.
        return format!("M0 -{r}A{r} {r} 0 1 1 0 {r}A{r} {r} 0 1 1 0 -{r}Z");
    }
    let point = |f: f64| {
        let a = f * std::f64::consts::TAU;
        // Rounded first, and `+ 0.0` so a tiny negative isn't "-0.00".
        let c = |v: f64| (v * 100.0).round() / 100.0 + 0.0;
        format!("{:.2} {:.2}", c(r * a.sin()), c(-r * a.cos()))
    };
    let large = u8::from(end - start > 0.5);
    format!("M0 0L{}A{r} {r} 0 {large} 1 {}Z", point(start), point(end))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_are_tagged_json() {
        let e = ServerEvent::Undone { seqs: vec![3, 4] };
        assert_eq!(
            serde_json::to_string(&e).unwrap(),
            r#"{"type":"undone","seqs":[3,4]}"#
        );
        let c: ClientCommand =
            serde_json::from_str(r#"{"type":"set_finish","finish":"foil"}"#).unwrap();
        assert_eq!(
            c,
            ClientCommand::SetFinish {
                finish: Finish::Foil
            }
        );
        let m: ClientCommand =
            serde_json::from_str(r#"{"type":"set_mode","mode":"table"}"#).unwrap();
        assert_eq!(m, ClientCommand::SetMode { mode: Mode::Table });
        let a: ClientCommand = serde_json::from_str(
            r#"{"type":"accept","seq":7,"scryfall_id":"abc","finish":"foil"}"#,
        )
        .unwrap();
        assert_eq!(
            a,
            ClientCommand::Accept {
                seq: 7,
                scryfall_id: "abc".into(),
                finish: Finish::Foil
            }
        );
        assert_eq!(
            serde_json::to_string(&ClientCommand::Discard { seq: 3 }).unwrap(),
            r#"{"type":"discard","seq":3}"#
        );
        assert_eq!(
            serde_json::to_string(&CardOutcome::Held).unwrap(),
            r#""held""#
        );
        let s: ClientCommand = serde_json::from_str(r#"{"type":"set_save","save":false}"#).unwrap();
        assert_eq!(s, ClientCommand::SetSave { save: false });
        assert_eq!(
            serde_json::to_string(&ClientCommand::Clear).unwrap(),
            r#"{"type":"clear"}"#
        );
        assert_eq!(
            serde_json::to_string(&ServerEvent::Cleared).unwrap(),
            r#"{"type":"cleared"}"#
        );
    }

    fn row(quantity: u32, tcg: Option<f64>) -> CollectionRow {
        CollectionRow {
            scryfall_id: String::new(),
            name: String::new(),
            set_code: String::new(),
            set_name: String::new(),
            number: String::new(),
            rarity: String::new(),
            colors: Vec::new(),
            type_line: String::new(),
            finish: Finish::Nonfoil,
            quantity,
            tcgplayer: tcg,
            cardmarket: None,
            cardkingdom: Some(1.0),
            cardkingdom_buylist: None,
            added: String::new(),
            priced: None,
            scryfall_url: String::new(),
            tcgplayer_url: None,
            cardkingdom_url: None,
        }
    }

    fn scanned(seq: u64, id: &str, tcg: Option<f64>, outcome: CardOutcome) -> CardView {
        CardView {
            seq,
            time: String::new(),
            outcome,
            scryfall_id: id.into(),
            name: id.into(),
            set_code: String::new(),
            set_name: String::new(),
            number: String::new(),
            rarity: String::new(),
            finish: Finish::Nonfoil,
            quantity: None,
            tcgplayer: tcg,
            cardmarket: None,
            cardkingdom: None,
            cardkingdom_buylist: None,
            tcgplayer_url: None,
            cardkingdom_url: None,
            scryfall_url: String::new(),
            image_url: None,
            method: String::new(),
            quality: String::new(),
            needs_review: false,
            note: None,
            undone: false,
        }
    }

    #[test]
    fn top_cards_rank_kept_cards_once_each() {
        use CardOutcome::*;
        let mut undone = scanned(6, "undone", Some(99.0), Added);
        undone.undone = true;
        let cards = vec![
            scanned(1, "a", Some(5.0), Added),
            scanned(2, "b", Some(20.0), Priced),
            scanned(3, "a", Some(5.0), Added),
            scanned(4, "held", Some(50.0), Held),
            scanned(5, "noprice", None, Added),
            undone,
            scanned(7, "c", Some(12.5), Added),
        ];
        let tcg = |c: &CardView| c.tcgplayer;
        let names = |v: Vec<CardView>| v.into_iter().map(|c| c.name).collect::<Vec<_>>();
        assert_eq!(names(top_cards(&cards, tcg, &[], 5)), ["b", "c", "a"]);
        assert_eq!(names(top_cards(&cards, tcg, &[], 2)), ["b", "c"]);
        let hidden = vec![cards[1].key()];
        assert_eq!(names(top_cards(&cards, tcg, &hidden, 5)), ["c", "a"]);
        assert!(top_cards(&cards, |c| c.cardkingdom, &[], 5).is_empty());
    }

    #[test]
    fn pie_slices_split_the_circle_in_order() {
        let v = |l: &str, n| (l.to_string(), n);
        let slices = pie(&[v("Creature", 3), v("Land", 0), v("Instant", 1)]);
        let labels: Vec<_> = slices.iter().map(|s| (s.label.as_str(), s.cards)).collect();
        assert_eq!(labels, [("Creature", 3), ("Instant", 1)]);
        assert_eq!(slices[0].share, 0.75);
        assert_eq!(slices[1].share, 0.25);
        // From 12 o'clock clockwise to 9 o'clock: the large arc.
        assert_eq!(slices[0].path, "M0 0L0.00 -48.00A48 48 0 1 1 -48.00 0.00Z");
        assert_eq!(slices[1].path, "M0 0L-48.00 0.00A48 48 0 0 1 0.00 -48.00Z");

        let whole = pie(&[v("Rare", 2)]);
        assert_eq!(whole[0].share, 1.0);
        assert_eq!(whole[0].path, "M0 -48A48 48 0 1 1 0 48A48 48 0 1 1 0 -48Z");
        assert!(pie(&[]).is_empty());
        assert!(pie(&[v("Rare", 0)]).is_empty());
    }

    #[test]
    fn card_images_come_from_the_cdn() {
        assert_eq!(
            card_image("4cbc6901-6a4a-4d0a-83ea-7eefa3b35021"),
            "https://cards.scryfall.io/normal/front/4/c/4cbc6901-6a4a-4d0a-83ea-7eefa3b35021.jpg"
        );
    }

    #[test]
    fn percents_round_but_never_show_zero_for_some() {
        assert_eq!(percent(0.4249), "42%");
        assert_eq!(percent(1.0), "100%");
        assert_eq!(percent(0.003), "<1%");
        assert_eq!(percent(0.0), "0%");
    }

    #[test]
    fn color_groups() {
        let c = |s: &[&str]| ColorGroup::of(&s.iter().map(|x| x.to_string()).collect::<Vec<_>>());
        assert_eq!(c(&[]), ColorGroup::Colorless);
        assert_eq!(c(&["U"]), ColorGroup::Blue);
        assert_eq!(c(&["R", "G"]), ColorGroup::Multicolor);
    }

    #[test]
    fn main_types() {
        assert_eq!(
            main_type("Legendary Creature — Phyrexian Praetor"),
            "Creature"
        );
        assert_eq!(main_type("Artifact Creature — Golem"), "Creature");
        assert_eq!(main_type("Artifact Land"), "Land");
        assert_eq!(main_type("Legendary Enchantment Artifact"), "Artifact");
        assert_eq!(main_type("Kindred Instant — Elf"), "Instant");
        assert_eq!(main_type("Sorcery // Sorcery"), "Sorcery");
        assert_eq!(
            main_type("Creature — Human Werewolf // Creature — Werewolf"),
            "Creature"
        );
        assert_eq!(main_type("Legendary Planeswalker — Karn"), "Planeswalker");
        assert_eq!(main_type(""), "Other");
        assert_eq!(main_type("Dungeon"), "Other");
    }

    #[test]
    fn stats_break_the_collection_down() {
        let mut bolt = row(4, Some(1.0));
        bolt.name = "Lightning Bolt".into();
        bolt.colors = vec!["R".into()];
        bolt.type_line = "Instant".into();
        bolt.rarity = "common".into();
        bolt.set_code = "M11".into();
        bolt.added = "2026-10-01".into();
        let mut sheoldred = row(1, Some(80.0));
        sheoldred.name = "Sheoldred".into();
        sheoldred.colors = vec!["B".into()];
        sheoldred.type_line = "Legendary Creature — Phyrexian Praetor".into();
        sheoldred.rarity = "mythic".into();
        sheoldred.set_code = "DMU".into();
        sheoldred.finish = Finish::Foil;
        sheoldred.added = "2026-10-20".into();
        let mut ring = row(2, None);
        ring.name = "Sol Ring".into();
        ring.type_line = "Artifact".into();
        ring.rarity = "uncommon".into();
        ring.set_code = "C21".into();
        ring.added = "2026-10-10".into();

        let s = Stats::of(&[bolt, sheoldred, ring]);
        assert_eq!(s.totals.cards, 7);
        assert_eq!(s.by_color.len(), 7, "every color group is listed");
        let color = |g| s.by_color.iter().find(|(c, _)| *c == g).unwrap().1.cards;
        assert_eq!(
            (
                color(ColorGroup::Red),
                color(ColorGroup::Black),
                color(ColorGroup::Colorless)
            ),
            (4, 1, 2)
        );
        assert_eq!(color(ColorGroup::Green), 0);
        let labels = |v: &[Slice]| v.iter().map(|x| x.label.clone()).collect::<Vec<_>>();
        assert_eq!(labels(&s.by_type), ["Instant", "Artifact", "Creature"]);
        assert_eq!(labels(&s.by_rarity), ["Common", "Uncommon", "Mythic"]);
        assert_eq!(labels(&s.by_finish), ["Nonfoil", "Foil"]);
        assert_eq!(s.top_sets[0].label, "M11");
        assert_eq!(s.by_rarity[2].value, 80.0);
        let names = |v: &[CollectionRow]| v.iter().map(|x| x.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&s.most_valuable), ["Sheoldred", "Lightning Bolt"]);
        assert_eq!(
            names(&s.recently_added),
            ["Sheoldred", "Sol Ring", "Lightning Bolt"]
        );
    }

    /// Lightning Bolt (red instant), Lightning Helix (multicolor instant),
    /// Sheoldred (black mythic creature) and Sol Ring (colorless artifact).
    fn sample() -> Vec<CollectionRow> {
        let card = |name: &str, colors: &[&str], type_line: &str, rarity: &str, qty: u32| {
            let mut r = row(qty, Some(1.0));
            r.name = name.into();
            r.colors = colors.iter().map(|c| c.to_string()).collect();
            r.type_line = type_line.into();
            r.rarity = rarity.into();
            r
        };
        vec![
            card("Lightning Bolt", &["R"], "Instant", "common", 4),
            card("Lightning Helix", &["R", "W"], "Instant", "uncommon", 1),
            card(
                "Sheoldred, the Apocalypse",
                &["B"],
                "Legendary Creature — Phyrexian Praetor",
                "mythic",
                1,
            ),
            card("Sol Ring", &[], "Artifact", "", 2),
        ]
    }

    fn names(rows: &[CollectionRow], f: &RowFilter) -> Vec<String> {
        rows.iter()
            .filter(|r| f.matches(r))
            .map(|r| r.name.clone())
            .collect()
    }

    #[test]
    fn rows_have_a_value_per_column() {
        let rows = sample();
        let col = |f| rows.iter().map(|r| r.facet(f)).collect::<Vec<_>>();
        assert_eq!(
            col(Facet::Type),
            ["Instant", "Instant", "Creature", "Artifact"]
        );
        assert_eq!(
            col(Facet::Rarity),
            ["Common", "Uncommon", "Mythic", "Unknown"]
        );
        assert_eq!(
            col(Facet::Color),
            ["Red", "Multicolor", "Black", "Colorless"]
        );
    }

    #[test]
    fn search_matches_type_rarity_and_color() {
        let rows = sample();
        let search = |text: &str| {
            names(
                &rows,
                &RowFilter {
                    text: text.into(),
                    ..Default::default()
                },
            )
        };
        assert_eq!(search("").len(), 4);
        assert_eq!(search("CREATURE"), ["Sheoldred, the Apocalypse"]);
        assert_eq!(search("praetor"), ["Sheoldred, the Apocalypse"]);
        assert_eq!(search("myth"), ["Sheoldred, the Apocalypse"]);
        // Rarity and color match from the start: "common" isn't "uncommon".
        assert_eq!(search("common"), ["Lightning Bolt"]);
        assert_eq!(search("colorless"), ["Sol Ring"]);
        // A multicolor card matches each of its colors as well.
        assert_eq!(search("white"), ["Lightning Helix"]);
        assert_eq!(search("multicolor"), ["Lightning Helix"]);
        // Every word has to match.
        assert_eq!(search("red instant common"), ["Lightning Bolt"]);
        assert_eq!(search("lightning uncommon"), ["Lightning Helix"]);
        assert!(search("blue instant").is_empty());
    }

    #[test]
    fn picks_or_within_a_column_and_across_columns() {
        let rows = sample();
        let mut f = RowFilter::default();
        f.toggle(Facet::Color, "Red");
        assert_eq!(names(&rows, &f), ["Lightning Bolt"]);
        f.toggle(Facet::Color, "Multicolor");
        assert_eq!(names(&rows, &f), ["Lightning Bolt", "Lightning Helix"]);
        f.toggle(Facet::Rarity, "Uncommon");
        assert_eq!(names(&rows, &f), ["Lightning Helix"]);
        f.text = "bolt".into();
        assert!(names(&rows, &f).is_empty(), "search and picks combine");
        f.text.clear();
        // Unpicking the last value of a column drops that column's condition.
        f.toggle(Facet::Rarity, "Uncommon");
        assert!(!f.picked.contains_key(&Facet::Rarity));
        assert!(f.is_picked(Facet::Color, "Red") && f.has_picks() && f.is_active());
        f.toggle(Facet::Color, "Red");
        f.toggle(Facet::Color, "Multicolor");
        assert!(!f.is_active());
        assert_eq!(names(&rows, &f).len(), 4);
    }

    #[test]
    fn missing_card_details_are_unknown_not_colorless() {
        let mut unresolved = row(3, Some(1.0));
        unresolved.name = "Mystery".into();
        unresolved.rarity = "rare".into();
        let mut rows = sample();
        rows.push(unresolved.clone());

        assert!(!unresolved.details_known());
        assert_eq!(unresolved.color_group(), None);
        assert_eq!(unresolved.facet(Facet::Type), "Unknown");
        assert_eq!(unresolved.facet(Facet::Color), "Unknown");
        let search = |text: &str| {
            names(
                &rows,
                &RowFilter {
                    text: text.into(),
                    ..Default::default()
                },
            )
        };
        assert_eq!(search("colorless"), ["Sol Ring"]);
        assert_eq!(search("unknown"), ["Sol Ring", "Mystery"]); // Sol Ring: no rarity
        let mut f = RowFilter::default();
        f.toggle(Facet::Color, "Colorless");
        assert_eq!(names(&rows, &f), ["Sol Ring"]);
        // Unknown sorts last and keeps its own group.
        let (last, _) = Facet::Color.present(&rows).pop().unwrap();
        assert_eq!(last, "Unknown");
        assert_eq!(Facet::Type.group(&rows).last().unwrap().0, "Unknown");
        // Statistics don't count it as colorless.
        let s = Stats::of(&rows);
        let colorless = s.by_color.iter().find(|(g, _)| *g == ColorGroup::Colorless);
        assert_eq!(colorless.unwrap().1.cards, 2);
    }

    #[test]
    fn groups_follow_display_order() {
        let rows = sample();
        let groups = |f: Facet| {
            f.group(&rows)
                .into_iter()
                .map(|(v, rows)| format!("{v}:{}", rows.len()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            groups(Facet::Type),
            ["Creature:1", "Instant:2", "Artifact:1"]
        );
        assert_eq!(
            groups(Facet::Color),
            ["Black:1", "Red:1", "Multicolor:1", "Colorless:1"]
        );
        assert_eq!(
            Facet::Rarity.present(&rows),
            [
                ("Common".to_string(), 4),
                ("Uncommon".to_string(), 1),
                ("Mythic".to_string(), 1),
                ("Unknown".to_string(), 2)
            ]
        );
        // Rows keep their order within a group.
        let instants = &Facet::Type.group(&rows)[1].1;
        assert_eq!(instants[0].name, "Lightning Bolt");
    }

    #[test]
    fn totals_weigh_prices_by_quantity() {
        let t = Totals::of(&[row(2, Some(1.25)), row(3, None)]);
        assert_eq!((t.printings, t.cards), (2, 5));
        assert_eq!(
            (t.tcgplayer, t.cardkingdom, t.cardkingdom_buylist),
            (2.5, 5.0, 0.0)
        );
    }

    #[test]
    fn usd_formats_prices() {
        assert_eq!(usd(Some(1.5)), "$1.50");
        assert_eq!(usd(None), "—");
        let empty: f64 = std::iter::empty::<f64>().sum();
        assert_eq!(usd(Some(empty)), "$0.00");
    }
}
