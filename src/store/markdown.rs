//! Markdown-file collection store. The file is both human-readable (it
//! renders nicely on GitHub / Obsidian) and the source of truth: it is parsed
//! before every read or change and fully rewritten after every change.
//!
//! Several stores can have the same file open (the scanner, each web
//! request, a CLI command in another process). Every change therefore runs
//! as one locked read-modify-write: it takes an exclusive lock on
//! `<collection>.md.lock`, re-reads the file, changes it and writes it back,
//! so no change overwrites another.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use chrono::NaiveDate;

use super::{CollectionStore, totals};
use crate::model::{CollectionEntry, Condition, Finish, Prices, RowKey, format_usd};

const HEADER: [&str; 13] = [
    "Qty",
    "Name",
    "Set",
    "Set Name",
    "#",
    "Rarity",
    "Finish",
    "TCGplayer",
    "Card Kingdom",
    "CK Buylist",
    "Added",
    "Priced",
    "Scryfall ID",
];

pub struct MarkdownStore {
    path: PathBuf,
    entries: Vec<CollectionEntry>,
}

impl MarkdownStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let entries = load(&path)?;
        Ok(Self { path, entries })
    }

    /// Start a change: lock the collection against other writers (threads
    /// or processes) and re-read it. The lock is released when the returned
    /// file is dropped, i.e. after the change has been saved.
    fn begin(&mut self) -> Result<File> {
        if let Some(dir) = self.path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        let lock_path = self.path.with_extension("md.lock");
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("failed to open {}", lock_path.display()))?;
        lock.lock()
            .with_context(|| format!("failed to lock {}", lock_path.display()))?;
        self.entries = load(&self.path)?;
        Ok(lock)
    }

    /// Set a row's quantity in memory (0 deletes it); `None` if absent.
    fn apply_quantity(
        &mut self,
        scryfall_id: &str,
        finish: Finish,
        quantity: impl FnOnce(u32) -> u32,
    ) -> Option<CollectionEntry> {
        let idx = self.position(scryfall_id, finish)?;
        let entry = &mut self.entries[idx];
        entry.quantity = quantity(entry.quantity);
        let result = entry.clone();
        if result.quantity == 0 {
            self.entries.remove(idx);
        }
        Some(result)
    }

    fn position(&self, scryfall_id: &str, finish: Finish) -> Option<usize> {
        self.entries
            .iter()
            .position(|e| e.same_printing(scryfall_id, finish))
    }

    fn save(&mut self) -> Result<()> {
        self.entries.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| a.set_code.cmp(&b.set_code))
                .then_with(|| a.collector_number.cmp(&b.collector_number))
                .then_with(|| a.finish.as_str().cmp(b.finish.as_str()))
        });
        let text = render(&self.entries);
        // Write-then-rename so a crash never leaves a half-written collection.
        let tmp = self.path.with_extension("md.tmp");
        std::fs::write(&tmp, text).with_context(|| format!("failed to write {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("failed to write {}", self.path.display()))
    }
}

impl CollectionStore for MarkdownStore {
    fn entries(&self) -> Result<Vec<CollectionEntry>> {
        load(&self.path)
    }

    fn add(&mut self, entry: CollectionEntry) -> Result<CollectionEntry> {
        let _lock = self.begin()?;
        let result = match self
            .entries
            .iter_mut()
            .find(|e| e.same_printing(&entry.scryfall_id, entry.finish))
        {
            Some(existing) => {
                existing.quantity += entry.quantity;
                existing.prices = entry.prices;
                existing.price_updated = entry.price_updated;
                existing.clone()
            }
            None => {
                self.entries.push(entry.clone());
                entry
            }
        };
        self.save()?;
        Ok(result)
    }

    // The file has no condition or language: rows are matched by printing
    // and finish alone.
    fn remove(&mut self, key: &RowKey, quantity: u32) -> Result<Option<CollectionEntry>> {
        let _lock = self.begin()?;
        let result = self.apply_quantity(&key.scryfall_id, key.finish, |have| {
            have.saturating_sub(quantity)
        });
        if result.is_some() {
            self.save()?;
        }
        Ok(result)
    }

    fn set_quantity(&mut self, key: &RowKey, quantity: u32) -> Result<Option<CollectionEntry>> {
        let _lock = self.begin()?;
        let result = self.apply_quantity(&key.scryfall_id, key.finish, |_| quantity);
        if result.is_some() {
            self.save()?;
        }
        Ok(result)
    }

    fn relocate(&mut self, key: &RowKey, to: CollectionEntry) -> Result<Option<CollectionEntry>> {
        let _lock = self.begin()?;
        let Some(idx) = self.position(&key.scryfall_id, key.finish) else {
            return Ok(None);
        };
        let old = self.entries.remove(idx);
        let result = match self.position(&to.scryfall_id, to.finish) {
            Some(target) => {
                let existing = &mut self.entries[target];
                existing.quantity += to.quantity;
                existing.added = existing.added.min(old.added);
                existing.prices = to.prices;
                existing.price_updated = to.price_updated;
                existing.clone()
            }
            None => {
                let entry = CollectionEntry {
                    added: old.added,
                    ..to
                };
                self.entries.push(entry.clone());
                entry
            }
        };
        self.save()?;
        Ok(Some(result))
    }

    fn update_prices(&mut self, scryfall_id: &str, finish: Finish, prices: Prices) -> Result<()> {
        let _lock = self.begin()?;
        if let Some(e) = self
            .entries
            .iter_mut()
            .find(|e| e.same_printing(scryfall_id, finish))
        {
            e.prices = prices;
            e.price_updated = Some(chrono::Local::now().date_naive());
            self.save()?;
        }
        Ok(())
    }

    fn location(&self) -> String {
        self.path.display().to_string()
    }
}

fn load(path: &Path) -> Result<Vec<CollectionEntry>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    parse(&text).with_context(|| format!("failed to parse {}", path.display()))
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace('[', "\\[")
        .replace(']', "\\]")
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(n) = chars.next() {
                out.push(n);
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn link(text: &str, url: Option<&str>) -> String {
    match url {
        Some(u) if !u.is_empty() => format!("[{text}](<{u}>)"),
        _ => text.to_string(),
    }
}

pub fn render(entries: &[CollectionEntry]) -> String {
    let t = totals(entries);
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M");
    let mut out = String::new();
    out.push_str("# MTG Collection\n\n");
    out.push_str(
        "> Managed by `mtg-scan`. You may edit quantities or delete rows by hand, but keep the \
         table layout and the Scryfall ID column intact.\n\n",
    );
    out.push_str("## Summary\n\n| | |\n|---|---:|\n");
    out.push_str(&format!("| Unique printings | {} |\n", t.printings));
    out.push_str(&format!("| Total cards | {} |\n", t.cards));
    out.push_str(&format!(
        "| TCGplayer market value | ${:.2} |\n",
        t.tcgplayer
    ));
    out.push_str(&format!(
        "| Card Kingdom retail value | ${:.2} |\n",
        t.cardkingdom
    ));
    out.push_str(&format!(
        "| Card Kingdom buylist value | ${:.2} |\n",
        t.cardkingdom_buylist
    ));
    out.push_str(&format!("| Last updated | {now} |\n\n"));
    out.push_str("## Cards\n\n");
    out.push_str(&format!("| {} |\n", HEADER.join(" | ")));
    out.push_str("|---:|---|---|---|---|---|---|---:|---:|---:|---|---|---|\n");
    for e in entries {
        let cells = [
            e.quantity.to_string(),
            link(&escape(&e.name), Some(&e.scryfall_url())),
            escape(&e.set_code),
            escape(&e.set_name),
            escape(&e.collector_number),
            escape(&e.rarity),
            e.finish.to_string(),
            link(
                &format_usd(e.prices.tcgplayer),
                e.prices.tcgplayer_url.as_deref(),
            ),
            link(
                &format_usd(e.prices.cardkingdom),
                e.prices.cardkingdom_url.as_deref(),
            ),
            format_usd(e.prices.cardkingdom_buylist),
            e.added.to_string(),
            e.price_updated.map(|d| d.to_string()).unwrap_or_default(),
            e.scryfall_id.clone(),
        ];
        out.push_str(&format!("| {} |\n", cells.join(" | ")));
    }
    out
}

/// Split a table row on unescaped pipes.
fn split_row(line: &str) -> Vec<String> {
    let inner = line.trim().trim_start_matches('|');
    let inner = inner.strip_suffix('|').unwrap_or(inner);
    let mut cells = Vec::new();
    let mut cur = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                cur.push(c);
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            '|' => cells.push(std::mem::take(&mut cur).trim().to_string()),
            _ => cur.push(c),
        }
    }
    cells.push(cur.trim().to_string());
    cells
}

/// Parse `[text](<url>)` / `[text](url)` / `text` into (text, url).
fn parse_link(cell: &str) -> (String, Option<String>) {
    if let Some(rest) = cell.strip_prefix('[') {
        // Find the closing `](` that isn't escaped.
        let bytes = rest.as_bytes();
        let mut i = 0;
        while i + 1 < bytes.len() {
            if bytes[i] == b'\\' {
                i += 2;
                continue;
            }
            if bytes[i] == b']' && bytes[i + 1] == b'(' {
                let text = unescape(&rest[..i]);
                let url = rest[i + 2..]
                    .trim_end_matches(')')
                    .trim_start_matches('<')
                    .trim_end_matches('>');
                return (text, Some(url.to_string()).filter(|u| !u.is_empty()));
            }
            i += 1;
        }
    }
    (unescape(cell), None)
}

fn parse_usd(text: &str) -> Option<f64> {
    text.trim()
        .trim_start_matches('$')
        .replace(',', "")
        .parse()
        .ok()
}

pub fn parse(text: &str) -> Result<Vec<CollectionEntry>> {
    let mut lines = text.lines().enumerate();
    // Locate the cards table header.
    loop {
        match lines.next() {
            Some((_, l)) if l.trim_start().starts_with("| Qty |") => break,
            Some(_) => continue,
            None => return Ok(Vec::new()),
        }
    }
    lines.next(); // alignment row

    let mut entries = Vec::new();
    for (lineno, line) in lines {
        let line = line.trim();
        if !line.starts_with('|') {
            break;
        }
        let cells = split_row(line);
        let row = || format!("row on line {}", lineno + 1);
        if cells.len() != HEADER.len() {
            return Err(anyhow!(
                "{}: expected {} columns, found {}",
                row(),
                HEADER.len(),
                cells.len()
            ));
        }
        let (name, _) = parse_link(&cells[1]);
        let (tcg, tcg_url) = parse_link(&cells[7]);
        let (ck, ck_url) = parse_link(&cells[8]);
        let date = |s: &str| NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok();
        entries.push(CollectionEntry {
            quantity: cells[0]
                .parse()
                .with_context(|| format!("{}: bad quantity", row()))?,
            name,
            set_code: unescape(&cells[2]),
            set_name: unescape(&cells[3]),
            collector_number: unescape(&cells[4]),
            rarity: unescape(&cells[5]),
            colors: Vec::new(),
            type_line: String::new(),
            finish: cells[6].parse().with_context(row)?,
            condition: Condition::Nm,
            language: "en".into(),
            prices: Prices {
                tcgplayer: parse_usd(&tcg),
                tcgplayer_url: tcg_url,
                cardkingdom: parse_usd(&ck),
                cardkingdom_buylist: parse_usd(&cells[9]),
                cardkingdom_url: ck_url,
                ..Prices::default()
            },
            added: date(&cells[10]).unwrap_or_else(|| chrono::Local::now().date_naive()),
            price_updated: date(&cells[11]),
            scryfall_id: cells[12].clone(),
        });
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, id: &str, finish: Finish, qty: u32) -> CollectionEntry {
        CollectionEntry {
            scryfall_id: id.into(),
            name: name.into(),
            set_code: "DMU".into(),
            set_name: "Dominaria United".into(),
            collector_number: "107".into(),
            rarity: "mythic".into(),
            colors: Vec::new(),
            type_line: String::new(),
            finish,
            condition: Condition::Nm,
            language: "en".into(),
            quantity: qty,
            prices: Prices {
                tcgplayer: Some(102.11),
                tcgplayer_url: Some("https://www.tcgplayer.com/product/282800".into()),
                cardkingdom: Some(119.99),
                cardkingdom_buylist: Some(75.0),
                cardkingdom_url: Some(
                    "https://www.cardkingdom.com/mtg/dominaria-united/sheoldred".into(),
                ),
                ..Prices::default()
            },
            added: NaiveDate::from_ymd_opt(2026, 10, 20).unwrap(),
            price_updated: NaiveDate::from_ymd_opt(2026, 10, 20),
        }
    }

    #[test]
    fn render_parse_round_trip() {
        let mut odd = entry("Weird | Name [x]", "id-2", Finish::Foil, 2);
        odd.prices = Prices::default();
        odd.price_updated = None;
        let entries = vec![
            entry("Sheoldred, the Apocalypse", "id-1", Finish::Nonfoil, 1),
            odd,
        ];
        let parsed = parse(&render(&entries)).unwrap();
        assert_eq!(parsed, entries);
    }

    #[test]
    fn add_merges_and_remove_deletes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.md");
        let mut store = MarkdownStore::open(&path).unwrap();
        store
            .add(entry(
                "Sheoldred, the Apocalypse",
                "id-1",
                Finish::Nonfoil,
                1,
            ))
            .unwrap();
        let merged = store
            .add(entry(
                "Sheoldred, the Apocalypse",
                "id-1",
                Finish::Nonfoil,
                2,
            ))
            .unwrap();
        assert_eq!(merged.quantity, 3);
        store
            .add(entry("Sheoldred, the Apocalypse", "id-1", Finish::Foil, 1))
            .unwrap();

        let reopened = MarkdownStore::open(&path).unwrap();
        assert_eq!(reopened.entries().unwrap().len(), 2);

        let mut store = reopened;
        let left = store
            .remove(&RowKey::new("id-1", Finish::Nonfoil, "en"), 3)
            .unwrap()
            .unwrap();
        assert_eq!(left.quantity, 0);
        assert_eq!(store.entries().unwrap().len(), 1);
        assert!(
            store
                .remove(&RowKey::new("missing", Finish::Nonfoil, "en"), 1)
                .unwrap()
                .is_none()
        );

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("| Total cards | 1 |"));
    }

    #[test]
    fn set_quantity_and_relocate() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = MarkdownStore::open(dir.path().join("c.md")).unwrap();
        store.add(entry("A", "id-1", Finish::Nonfoil, 2)).unwrap();
        store.add(entry("B", "id-2", Finish::Foil, 1)).unwrap();

        assert_eq!(
            store
                .set_quantity(&RowKey::new("id-1", Finish::Nonfoil, "en"), 5)
                .unwrap()
                .unwrap()
                .quantity,
            5
        );
        assert!(
            store
                .set_quantity(&RowKey::new("nope", Finish::Nonfoil, "en"), 1)
                .unwrap()
                .is_none()
        );

        // Move to a printing + finish not owned yet: the row is replaced.
        let mut to = entry("A (other printing)", "id-3", Finish::Foil, 5);
        to.added = NaiveDate::from_ymd_opt(2030, 1, 1).unwrap();
        let moved = store
            .relocate(&RowKey::new("id-1", Finish::Nonfoil, "en"), to)
            .unwrap()
            .unwrap();
        assert_eq!((moved.scryfall_id.as_str(), moved.quantity), ("id-3", 5));
        // The row keeps the date the card was first added.
        assert_eq!(moved.added, NaiveDate::from_ymd_opt(2026, 10, 20).unwrap());
        assert!(
            !store
                .entries()
                .unwrap()
                .iter()
                .any(|e| e.scryfall_id == "id-1")
        );

        // Move onto a row that exists: quantities merge.
        let merged = store
            .relocate(
                &RowKey::new("id-3", Finish::Foil, "en"),
                entry("B", "id-2", Finish::Foil, 5),
            )
            .unwrap()
            .unwrap();
        assert_eq!(merged.quantity, 6);
        assert_eq!(store.entries().unwrap().len(), 1);

        assert!(
            store
                .relocate(
                    &RowKey::new("gone", Finish::Foil, "en"),
                    entry("B", "id-2", Finish::Foil, 1)
                )
                .unwrap()
                .is_none()
        );
        assert_eq!(
            store
                .set_quantity(&RowKey::new("id-2", Finish::Foil, "en"), 0)
                .unwrap()
                .unwrap()
                .quantity,
            0
        );
        assert!(store.entries().unwrap().is_empty());
    }

    #[test]
    fn stores_on_one_file_see_each_others_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.md");
        let mut scanner = MarkdownStore::open(&path).unwrap();
        let mut web = MarkdownStore::open(&path).unwrap();
        scanner.add(entry("A", "id-1", Finish::Nonfoil, 1)).unwrap();
        web.set_quantity(&RowKey::new("id-1", Finish::Nonfoil, "en"), 4)
            .unwrap()
            .unwrap();
        // The scanner's next save must not undo the web edit.
        scanner.add(entry("B", "id-2", Finish::Nonfoil, 1)).unwrap();
        let entries = web.entries().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries
                .iter()
                .find(|e| e.scryfall_id == "id-1")
                .unwrap()
                .quantity,
            4
        );
    }

    #[test]
    fn concurrent_writers_lose_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.md");
        let writers: Vec<_> = (0..4)
            .map(|w| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let mut store = MarkdownStore::open(&path).unwrap();
                    for i in 0..15 {
                        store
                            .add(entry("Card", &format!("id-{w}-{i}"), Finish::Nonfoil, 1))
                            .unwrap();
                        store
                            .add(entry("Shared", "shared", Finish::Foil, 1))
                            .unwrap();
                    }
                })
            })
            .collect();
        for w in writers {
            w.join().unwrap();
        }
        let entries = MarkdownStore::open(&path).unwrap().entries().unwrap();
        assert_eq!(entries.len(), 4 * 15 + 1, "every distinct row was kept");
        let shared = entries.iter().find(|e| e.scryfall_id == "shared").unwrap();
        assert_eq!(shared.quantity, 4 * 15, "every increment was kept");
    }
}
