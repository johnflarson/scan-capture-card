//! The collector's SQLite file: one row per server and day (the day's last
//! ping), the GitHub numbers per day, and each release's downloads. No IP
//! addresses or anything else about who sent a ping.

use std::path::Path;

use anyhow::{Context, Result};
use mtg_scan_shared::Ping;
use rusqlite::{Connection, OptionalExtension, params};

pub struct Store {
    conn: Connection,
}

/// A ping as stored: the day (UTC) it arrived and what it said.
#[derive(Debug, Clone)]
pub struct Row {
    pub day: String,
    pub ping: Ping,
}

/// A release and the downloads of its files.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Release {
    pub tag: String,
    pub published: String,
    pub downloads: i64,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS pings (
    day TEXT NOT NULL,
    install_id TEXT NOT NULL,
    received_at TEXT NOT NULL,
    ping TEXT NOT NULL,
    PRIMARY KEY (day, install_id)
);
CREATE TABLE IF NOT EXISTS github_daily (
    day TEXT NOT NULL,
    metric TEXT NOT NULL,
    value INTEGER NOT NULL,
    PRIMARY KEY (day, metric)
);
CREATE TABLE IF NOT EXISTS github_releases (
    tag TEXT PRIMARY KEY,
    published TEXT NOT NULL,
    downloads INTEGER NOT NULL
);
";

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("failed to create {}", dir.display()))?;
        }
        let conn =
            Connection::open(path).with_context(|| format!("failed to open {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Self::with(conn)
    }

    #[cfg(test)]
    pub fn in_memory() -> Result<Self> {
        Self::with(Connection::open_in_memory()?)
    }

    fn with(conn: Connection) -> Result<Self> {
        conn.execute_batch(SCHEMA)
            .context("failed to set up the database")?;
        Ok(Self { conn })
    }

    /// Keep `ping` as the server's ping of `day` (a later one that day
    /// replaces it).
    pub fn record(&self, day: &str, ping: &Ping, received_at: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO pings (day, install_id, received_at, ping) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (day, install_id) DO UPDATE
             SET received_at = excluded.received_at, ping = excluded.ping",
            params![
                day,
                ping.install_id,
                received_at,
                serde_json::to_string(ping)?
            ],
        )?;
        Ok(())
    }

    /// Pings from `day` on, oldest first.
    pub fn pings_since(&self, day: &str) -> Result<Vec<Row>> {
        let mut stmt = self
            .conn
            .prepare("SELECT day, ping FROM pings WHERE day >= ?1 ORDER BY day, install_id")?;
        let rows = stmt.query_map([day], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (day, json) = row?;
            // A row this version can't read is left out, not fatal.
            if let Ok(ping) = serde_json::from_str(&json) {
                out.push(Row { day, ping });
            }
        }
        Ok(out)
    }

    /// The first day each server was seen, for servers first seen from `day` on.
    pub fn first_seen_since(&self, day: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT MIN(day) AS first FROM pings GROUP BY install_id HAVING first >= ?1 ORDER BY first",
        )?;
        let rows = stmt.query_map([day], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Servers ever seen.
    pub fn servers_ever(&self) -> Result<u64> {
        let n: i64 =
            self.conn
                .query_row("SELECT COUNT(DISTINCT install_id) FROM pings", [], |r| {
                    r.get(0)
                })?;
        Ok(n as u64)
    }

    /// Drop pings from before `day`.
    pub fn prune(&self, day: &str) -> Result<usize> {
        Ok(self
            .conn
            .execute("DELETE FROM pings WHERE day < ?1", [day])?)
    }

    pub fn set_github(&self, day: &str, metric: &str, value: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO github_daily (day, metric, value) VALUES (?1, ?2, ?3)
             ON CONFLICT (day, metric) DO UPDATE SET value = excluded.value",
            params![day, metric, value],
        )?;
        Ok(())
    }

    /// `metric` per day from `day` on, oldest first.
    pub fn github_series(&self, metric: &str, day: &str) -> Result<Vec<(String, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT day, value FROM github_daily WHERE metric = ?1 AND day >= ?2 ORDER BY day",
        )?;
        let rows = stmt.query_map(params![metric, day], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The latest value of `metric`, with its day.
    pub fn github_latest(&self, metric: &str) -> Result<Option<(String, i64)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT day, value FROM github_daily WHERE metric = ?1 ORDER BY day DESC LIMIT 1",
                [metric],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    /// `metric` on `day` or the closest day before it.
    pub fn github_on(&self, metric: &str, day: &str) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row(
                "SELECT value FROM github_daily WHERE metric = ?1 AND day <= ?2
                 ORDER BY day DESC LIMIT 1",
                params![metric, day],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn set_releases(&self, releases: &[Release]) -> Result<()> {
        for r in releases {
            self.conn.execute(
                "INSERT INTO github_releases (tag, published, downloads) VALUES (?1, ?2, ?3)
                 ON CONFLICT (tag) DO UPDATE
                 SET published = excluded.published, downloads = excluded.downloads",
                params![r.tag, r.published, r.downloads],
            )?;
        }
        Ok(())
    }

    /// Releases, newest first.
    pub fn releases(&self) -> Result<Vec<Release>> {
        let mut stmt = self.conn.prepare(
            "SELECT tag, published, downloads FROM github_releases ORDER BY published DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Release {
                tag: r.get(0)?,
                published: r.get(1)?,
                downloads: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
pub mod tests {
    use std::collections::BTreeMap;

    use super::*;

    pub fn ping(id: u8) -> Ping {
        Ping {
            schema: mtg_scan_shared::SCHEMA,
            install_id: format!("{id:032x}"),
            version: "1.0.0-alpha.1".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
            install: "docker".into(),
            storage: "sqlite".into(),
            recognition: "auto".into(),
            llm_provider: Some("ollama".into()),
            ocr: "paddle".into(),
            card_kingdom: false,
            setup_done: true,
            accounts: "1".into(),
            scans: "10-99".into(),
            unrecognised: "0".into(),
            errors: BTreeMap::new(),
        }
    }

    #[test]
    fn one_row_per_server_and_day() {
        let store = Store::in_memory().unwrap();
        let mut p = ping(1);
        store.record("2026-10-24", &p, "t1").unwrap();
        p.scans = "100-999".into();
        store.record("2026-10-24", &p, "t2").unwrap();
        store.record("2026-10-25", &ping(1), "t3").unwrap();
        store.record("2026-10-25", &ping(2), "t4").unwrap();
        let rows = store.pings_since("2026-10-24").unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows[0].ping.scans, "100-999",
            "the later ping of the day wins"
        );
        assert_eq!(store.servers_ever().unwrap(), 2);
        assert_eq!(
            store.first_seen_since("2026-10-25").unwrap(),
            ["2026-10-25"],
            "server 1 was first seen the day before"
        );
        assert_eq!(store.prune("2026-10-25").unwrap(), 1);
        assert_eq!(store.pings_since("2000-01-01").unwrap().len(), 2);
    }

    #[test]
    fn github_numbers_by_day() {
        let store = Store::in_memory().unwrap();
        store.set_github("2026-10-01", "stars", 10).unwrap();
        store.set_github("2026-10-20", "stars", 14).unwrap();
        store.set_github("2026-10-20", "stars", 15).unwrap();
        assert_eq!(
            store.github_latest("stars").unwrap(),
            Some(("2026-10-20".into(), 15))
        );
        assert_eq!(store.github_on("stars", "2026-10-10").unwrap(), Some(10));
        assert_eq!(store.github_on("stars", "2026-09-30").unwrap(), None);
        assert_eq!(store.github_series("stars", "2026-10-02").unwrap().len(), 1);

        let releases = [
            Release {
                tag: "v1.0.0-alpha.1".into(),
                published: "2026-11-01T00:00:00Z".into(),
                downloads: 3,
            },
            Release {
                tag: "v0.3.0".into(),
                published: "2026-09-01T00:00:00Z".into(),
                downloads: 40,
            },
        ];
        store.set_releases(&releases).unwrap();
        store.set_releases(&releases[..1]).unwrap();
        assert_eq!(store.releases().unwrap(), releases);
    }
}
