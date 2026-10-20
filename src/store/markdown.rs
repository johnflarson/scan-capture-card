//! Markdown-file collection store. The file is both human-readable (it
//! renders nicely on GitHub / Obsidian) and the source of truth: it is parsed
//! on open and fully rewritten after every change.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use chrono::NaiveDate;

use super::{CollectionStore, totals};
use crate::model::{CollectionEntry, Finish, Prices, format_usd};

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
        let entries = if path.exists() {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            parse(&text).with_context(|| format!("failed to parse {}", path.display()))?
        } else {
            Vec::new()
        };
        Ok(Self { path, entries })
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
        if let Some(dir) = self.path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        // Write-then-rename so a crash never leaves a half-written collection.
        let tmp = self.path.with_extension("md.tmp");
        std::fs::write(&tmp, text).with_context(|| format!("failed to write {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("failed to write {}", self.path.display()))
    }
}

impl CollectionStore for MarkdownStore {
    fn entries(&self) -> Result<Vec<CollectionEntry>> {
        Ok(self.entries.clone())
    }

    fn add(&mut self, entry: CollectionEntry) -> Result<CollectionEntry> {
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

    fn remove(
        &mut self,
        scryfall_id: &str,
        finish: Finish,
        quantity: u32,
    ) -> Result<Option<CollectionEntry>> {
        let Some(idx) = self
            .entries
            .iter()
            .position(|e| e.same_printing(scryfall_id, finish))
        else {
            return Ok(None);
        };
        let entry = &mut self.entries[idx];
        entry.quantity = entry.quantity.saturating_sub(quantity);
        let result = entry.clone();
        if result.quantity == 0 {
            self.entries.remove(idx);
        }
        self.save()?;
        Ok(Some(result))
    }

    fn update_prices(&mut self, scryfall_id: &str, finish: Finish, prices: Prices) -> Result<()> {
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
            finish: cells[6].parse().with_context(row)?,
            prices: Prices {
                tcgplayer: parse_usd(&tcg),
                tcgplayer_url: tcg_url,
                cardkingdom: parse_usd(&ck),
                cardkingdom_buylist: parse_usd(&cells[9]),
                cardkingdom_url: ck_url,
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
            finish,
            quantity: qty,
            prices: Prices {
                tcgplayer: Some(102.11),
                tcgplayer_url: Some("https://www.tcgplayer.com/product/282800".into()),
                cardkingdom: Some(119.99),
                cardkingdom_buylist: Some(75.0),
                cardkingdom_url: Some(
                    "https://www.cardkingdom.com/mtg/dominaria-united/sheoldred".into(),
                ),
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
        let left = store.remove("id-1", Finish::Nonfoil, 3).unwrap().unwrap();
        assert_eq!(left.quantity, 0);
        assert_eq!(store.entries().unwrap().len(), 1);
        assert!(
            store
                .remove("missing", Finish::Nonfoil, 1)
                .unwrap()
                .is_none()
        );

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("| Total cards | 1 |"));
    }
}
