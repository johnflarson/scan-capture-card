//! Scryfall's daily bulk data: every paper printing with its card details
//! and prices, copied into the database's card cache. The collection is then
//! listed without calling the Scryfall API, which Scryfall asks apps to do
//! when they need many cards (<https://scryfall.com/docs/api/bulk-data>).
//!
//! It runs inside the app, not from an OS scheduler: [`spawn_daily`] checks
//! once an hour whether Scryfall has published a new file (it does about once
//! a day) and copies it in; `mtg-scan db sync-cards` does it now.

use std::io::{BufRead, BufReader, Read};
use std::time::Duration;

use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use reqwest::blocking::Client;

use crate::app::{App, USER_AGENT};
use crate::config::Config;
use crate::model::Card;
use crate::scryfall::{ApiCard, Scryfall};
use crate::store::CardCache;

/// The `app_job` row recording the last file copied in (its date).
pub const JOB: &str = "scryfall_default_cards";
/// Every printing in English, or in its only printed language.
const KIND: &str = "default-cards";
/// Printings written per transaction.
const BATCH: usize = 1000;
const CHECK_EVERY: Duration = Duration::from_secs(3600);

#[derive(Debug, PartialEq)]
pub struct Report {
    /// The bulk file's date, as Scryfall gives it.
    pub file_date: String,
    /// Printings written (0 when skipped).
    pub cards: usize,
    /// Lines that weren't a card mtg-scan could read.
    pub unreadable: usize,
    /// The file was already copied in.
    pub skipped: bool,
}

/// Copy Scryfall's current bulk file into `cache`, unless it already holds
/// that file (`force` copies it anyway).
pub fn sync(scryfall: &Scryfall, cache: &dyn CardCache, force: bool) -> Result<Report> {
    let info = scryfall.bulk_info(KIND)?;
    let done = cache.job(JOB)?;
    if !force && done.is_some_and(|j| j.detail == info.updated_at) {
        return Ok(Report {
            file_date: info.updated_at,
            cards: 0,
            unreadable: 0,
            skipped: true,
        });
    }
    let uri = info
        .jsonl_download_uri
        .context("Scryfall's bulk data has no JSON Lines file")?;
    // The file is ~80 MB, so no short overall timeout here.
    let download = Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(30 * 60))
        .build()?
        .get(&uri)
        .send()
        .and_then(|r| r.error_for_status())
        .with_context(|| format!("failed to download {uri}"))?;
    let (cards, unreadable) = read_cards(download, |batch| cache.store_cards(batch).map(|_| ()))?;
    cache.finish_job(JOB, &info.updated_at)?;
    Ok(Report {
        file_date: info.updated_at,
        cards,
        unreadable,
        skipped: false,
    })
}

/// Read gzip-compressed JSON Lines card objects, handing paper printings to
/// `each` in batches. Returns (printings read, unreadable lines); a line
/// that isn't a card mtg-scan understands is skipped rather than failing
/// the whole file.
pub fn read_cards(
    gz: impl Read,
    mut each: impl FnMut(&[Card]) -> Result<()>,
) -> Result<(usize, usize)> {
    let reader = BufReader::new(GzDecoder::new(gz));
    let (mut total, mut unreadable) = (0, 0);
    let mut batch = Vec::with_capacity(BATCH);
    for line in reader.lines() {
        let line = line.context("reading Scryfall's bulk file failed")?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<ApiCard>(&line) {
            Ok(card) if card.digital => {}
            Ok(card) => batch.push(Card::from(card)),
            Err(_) => unreadable += 1,
        }
        if batch.len() == BATCH {
            each(&batch)?;
            total += batch.len();
            batch.clear();
        }
    }
    if !batch.is_empty() {
        each(&batch)?;
        total += batch.len();
    }
    Ok((total, unreadable))
}

/// Keep the card cache current in the background while the app runs: check
/// for a new bulk file now and then every hour. Uses its own database
/// connection, so scanning never waits for it.
pub fn spawn_daily(cfg: Config) {
    let spawned = std::thread::Builder::new()
        .name("card-sync".into())
        .spawn(move || {
            loop {
                match sync_now(&cfg) {
                    Ok(Some(r)) if !r.skipped => eprintln!(
                        "Card data: copied {} printings from Scryfall's file of {}",
                        r.cards,
                        r.file_date.get(..10).unwrap_or(&r.file_date)
                    ),
                    Ok(_) => {}
                    Err(e) => eprintln!("  ! card data sync failed ({e:#}); trying again later"),
                }
                std::thread::sleep(CHECK_EVERY);
            }
        });
    if let Err(e) = spawned {
        eprintln!("  ! could not start the card data sync ({e})");
    }
}

/// One sync against the configured store; `None` when it has no card cache
/// (Markdown).
fn sync_now(cfg: &Config) -> Result<Option<Report>> {
    let app = App::new(cfg)?;
    let Some(cache) = app.store.card_cache() else {
        return Ok(None);
    };
    sync(&app.scryfall, cache, false).map(Some)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use flate2::Compression;
    use flate2::write::GzEncoder;

    use super::*;

    fn gz(text: &str) -> Vec<u8> {
        let mut enc = GzEncoder::new(Vec::new(), Compression::fast());
        enc.write_all(text.as_bytes()).unwrap();
        enc.finish().unwrap()
    }

    const BOLT: &str = r#"{"object":"card","id":"e768c957-3a1f-42f5-853a-96942f645df5","oracle_id":"4457ed35-7c10-48c8-9776-456485fdf070","name":"Lightning Bolt","lang":"en","set":"m11","set_name":"Magic 2011","collector_number":"149","rarity":"common","colors":["R"],"type_line":"Instant","finishes":["nonfoil","foil"],"tcgplayer_id":37301,"cardmarket_id":22337,"digital":false,"image_uris":{"small":"https://cards.scryfall.io/small/front/e/7/e768c957.jpg","normal":"https://cards.scryfall.io/normal/front/e/7/e768c957.jpg"},"prices":{"usd":"1.13","usd_foil":"9.10","usd_etched":null,"eur":"0.95","eur_foil":"6.50","tix":"0.02"},"legalities":{"modern":"legal"}}"#;
    const ARENA_ONLY: &str = r#"{"id":"00000000-0000-0000-0000-000000000001","name":"Arena Card","set":"ymid","set_name":"Alchemy","collector_number":"1","rarity":"rare","digital":true,"prices":{}}"#;
    const DFC: &str = r#"{"id":"11bf83bb-c95b-4b4f-9a56-ce7a1816307a","name":"Delver of Secrets // Insectile Aberration","set":"isd","set_name":"Innistrad","collector_number":"51","rarity":"common","type_line":"Creature — Human Wizard // Creature — Human Insect","finishes":["nonfoil","foil"],"card_faces":[{"colors":["U"],"image_uris":{"small":"front-small","normal":"front-normal"}},{"colors":["U"],"image_uris":{"small":"back-small"}}],"prices":{"usd":"0.32"}}"#;

    #[test]
    fn reads_paper_printings_with_prices_in_batches() {
        let text = format!("{BOLT}\n{ARENA_ONLY}\n\nnot json\n{DFC}\n");
        let mut seen = Vec::new();
        let (total, unreadable) = read_cards(gz(&text).as_slice(), |batch| {
            seen.extend_from_slice(batch);
            Ok(())
        })
        .unwrap();
        assert_eq!((total, unreadable), (2, 1), "Arena-only cards are skipped");
        let bolt = &seen[0];
        assert_eq!(
            (
                bolt.name.as_str(),
                bolt.lang.as_str(),
                bolt.set_code.as_str()
            ),
            ("Lightning Bolt", "en", "m11")
        );
        assert_eq!(
            bolt.oracle_id.as_deref(),
            Some("4457ed35-7c10-48c8-9776-456485fdf070")
        );
        assert_eq!(
            (bolt.scryfall_usd, bolt.scryfall_eur, bolt.scryfall_tix),
            (Some(1.13), Some(0.95), Some(0.02))
        );
        assert_eq!(bolt.scryfall_usd_etched, None);
        assert_eq!(
            (bolt.tcgplayer_id, bolt.cardmarket_id),
            (Some(37301), Some(22337))
        );
        assert!(bolt.image_normal.as_deref().unwrap().contains("normal"));
        let delver = &seen[1];
        assert_eq!(delver.lang, "en", "language defaults to English");
        assert_eq!(delver.colors, vec!["U"], "faces' colors, once");
        assert_eq!(
            (
                delver.image_small.as_deref(),
                delver.image_normal.as_deref()
            ),
            (Some("front-small"), Some("front-normal")),
            "the front face's images"
        );
    }

    #[test]
    fn cached_cards_survive_a_round_trip_and_mark_the_job() {
        let store = crate::store::sqlite::SqliteStore::open_in_memory().unwrap();
        let cache: &dyn CardCache = &store;
        let mut cards = Vec::new();
        read_cards(gz(&format!("{BOLT}\n{DFC}")).as_slice(), |b| {
            cards.extend_from_slice(b);
            Ok(())
        })
        .unwrap();
        assert_eq!(cache.store_cards(&cards).unwrap(), 2);
        // Storing again replaces rather than duplicates.
        assert_eq!(cache.store_cards(&cards[..1]).unwrap(), 1);
        assert_eq!(cache.cached_count().unwrap(), 2);
        let ids: Vec<String> = cards.iter().map(|c| c.scryfall_id.clone()).collect();
        let back = cache.cached_cards(&ids).unwrap();
        assert_eq!(back[&cards[0].scryfall_id], cards[0]);
        assert_eq!(back[&cards[1].scryfall_id], cards[1]);
        assert!(
            cache
                .cached_cards(&["00000000-0000-0000-0000-000000000009".into()])
                .unwrap()
                .is_empty()
        );

        assert!(cache.job(JOB).unwrap().is_none());
        cache
            .finish_job(JOB, "2026-10-22T09:05:40.234+00:00")
            .unwrap();
        let job = cache.job(JOB).unwrap().unwrap();
        assert_eq!(job.detail, "2026-10-22T09:05:40.234+00:00");
        assert!((chrono::Utc::now() - job.done_at).num_seconds() < 60);
    }
}
