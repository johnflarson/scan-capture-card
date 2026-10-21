//! Types exchanged between the mtg-scan server and its web UI as JSON.
//!
//! Both sides compile this crate, so the API can't drift: a field renamed
//! here is a compile error on whichever side wasn't updated.

use serde::{Deserialize, Serialize};

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
    pub dry_run: bool,
    /// One-line human-readable status.
    pub status: String,
    pub camera: String,
    pub recognizer: String,
    /// Where cards are saved, e.g. the database location.
    pub collection: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CardOutcome {
    /// Saved to the collection.
    Added,
    /// Identified and priced only (dry run).
    Priced,
}

/// A card identified during this session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CardView {
    /// Increases with every card; stable key for lists.
    pub seq: u64,
    /// Local time, `HH:MM:SS`.
    pub time: String,
    pub outcome: CardOutcome,
    pub name: String,
    pub set_code: String,
    pub set_name: String,
    pub number: String,
    pub rarity: String,
    pub finish: Finish,
    /// Copies in the collection after this scan (None for dry runs).
    pub quantity: Option<u32>,
    pub tcgplayer: Option<f64>,
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
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Totals {
    pub printings: usize,
    pub cards: u32,
    pub tcgplayer: f64,
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
            t.cardkingdom += r.cardkingdom.unwrap_or(0.0) * q;
            t.cardkingdom_buylist += r.cardkingdom_buylist.unwrap_or(0.0) * q;
            t
        })
    }
}

impl CollectionRow {
    pub fn color_group(&self) -> ColorGroup {
        ColorGroup::of(&self.colors)
    }

    pub fn main_type(&self) -> &'static str {
        main_type(&self.type_line)
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
    /// TCGplayer value change over the last week (PostgreSQL price history).
    pub value_change: Option<ValueChange>,
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
    /// Every color group, in WUBRG + multicolor + colorless order.
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
                let t = colors.get(&g).copied().unwrap_or_default();
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
    v.map(|p| format!("${p:.2}")).unwrap_or_else(|| "—".into())
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
            cardkingdom: Some(1.0),
            cardkingdom_buylist: None,
            added: String::new(),
            priced: None,
            scryfall_url: String::new(),
            tcgplayer_url: None,
            cardkingdom_url: None,
        }
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
    }
}
