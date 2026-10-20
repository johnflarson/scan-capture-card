//! Wiring shared by all commands: HTTP clients, Scryfall, pricing and the
//! collection store.

use std::rc::Rc;
use std::time::Duration;

use anyhow::Result;
use reqwest::blocking::Client;

use crate::config::{Config, StorageBackend};
use crate::model::{Card, CollectionEntry, Finish, Prices, format_usd};
use crate::pricing::PriceService;
use crate::scryfall::Scryfall;
use crate::store::CollectionStore;
use crate::store::markdown::MarkdownStore;
use crate::store::postgres::PostgresStore;

const USER_AGENT: &str = concat!("mtg-scan/", env!("CARGO_PKG_VERSION"));

pub struct App {
    /// Shared with the postgres store (it looks card details up) so both
    /// respect the same rate limits.
    pub scryfall: Rc<Scryfall>,
    pub prices: PriceService,
    pub store: Box<dyn CollectionStore>,
}

pub fn http_client() -> Result<Client> {
    Ok(Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(30))
        .build()?)
}

impl App {
    pub fn new(cfg: &Config) -> Result<Self> {
        let http = http_client()?;
        let scryfall = Rc::new(Scryfall::new(http.clone()));
        let store: Box<dyn CollectionStore> = match cfg.storage.backend {
            StorageBackend::Markdown => Box::new(MarkdownStore::open(&cfg.collection)?),
            StorageBackend::Postgres => Box::new(connect_postgres(cfg, scryfall.clone())?),
        };
        Ok(Self {
            scryfall,
            prices: PriceService::new(http),
            store,
        })
    }

    /// Price `card` and add `quantity` copies to the collection.
    pub fn add_card(
        &mut self,
        card: &Card,
        wanted: Finish,
        quantity: u32,
    ) -> Result<CollectionEntry> {
        let finish = card.best_finish(wanted);
        if finish != wanted {
            eprintln!(
                "  ! {} has no {wanted} printing; recording it as {finish}",
                card.name
            );
        }
        let prices = self.prices.price(card, finish);
        self.store
            .add(CollectionEntry::from_card(card, finish, quantity, prices))
    }
}

pub fn connect_postgres(cfg: &Config, scryfall: Rc<Scryfall>) -> Result<PostgresStore> {
    let connection = cfg.storage.resolved_connection()?;
    PostgresStore::connect(connection, &cfg.storage.schema, scryfall)
}

pub fn describe_card(card: &Card, finish: Finish) -> String {
    format!(
        "{} — {} ({} #{}) [{}, {finish}]",
        card.name,
        card.set_name,
        card.set_code.to_uppercase(),
        card.collector_number,
        card.rarity
    )
}

pub fn describe_prices(p: &Prices) -> String {
    format!(
        "TCGplayer {} · Card Kingdom {} (buylist {})",
        format_usd(p.tcgplayer),
        format_usd(p.cardkingdom),
        format_usd(p.cardkingdom_buylist)
    )
}
