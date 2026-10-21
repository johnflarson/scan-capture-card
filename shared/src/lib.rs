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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
    pub finish: Finish,
    pub quantity: u32,
    pub tcgplayer: Option<f64>,
    pub cardkingdom: Option<f64>,
    pub cardkingdom_buylist: Option<f64>,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CollectionView {
    pub location: String,
    pub rows: Vec<CollectionRow>,
    pub totals: Totals,
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

    #[test]
    fn totals_weigh_prices_by_quantity() {
        let row = |quantity, tcg: Option<f64>| CollectionRow {
            scryfall_id: String::new(),
            name: String::new(),
            set_code: String::new(),
            set_name: String::new(),
            number: String::new(),
            rarity: String::new(),
            finish: Finish::Nonfoil,
            quantity,
            tcgplayer: tcg,
            cardkingdom: Some(1.0),
            cardkingdom_buylist: None,
            priced: None,
            scryfall_url: String::new(),
            tcgplayer_url: None,
            cardkingdom_url: None,
        };
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
