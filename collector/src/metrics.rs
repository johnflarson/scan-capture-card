//! The numbers the metrics page shows, worked out from the stored pings and
//! GitHub numbers. Breakdowns count servers, each by its latest ping of the
//! last 30 days; scan and error counts arrive as ranges, so totals are the
//! sum of each range's lower bound ("at least").

use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::Result;
use chrono::{Datelike, Duration, NaiveDate};
use mtg_scan_shared::{COUNT_RANGES, ERROR_KINDS, Ping};
use serde::Serialize;

use crate::store::{Release, Store};

/// Days of history the charts show.
pub const CHART_DAYS: i64 = 90;
/// The window breakdowns and totals cover.
pub const WINDOW_DAYS: i64 = 30;
/// Weeks of new servers shown.
const WEEKS: i64 = 12;

#[derive(Debug, Clone, Serialize)]
pub struct Metrics {
    pub today: String,
    pub servers: Servers,
    /// Servers that pinged, per day, oldest first (`CHART_DAYS`).
    pub daily_servers: Vec<(String, u64)>,
    /// Servers first seen, per week (Monday), oldest first.
    pub new_servers: Vec<(String, u64)>,
    pub breakdowns: Vec<Breakdown>,
    pub scans: Scans,
    pub errors: Vec<ErrorRow>,
    pub github: GitHub,
}

#[derive(Debug, Clone, Serialize)]
pub struct Servers {
    pub today: u64,
    pub last_7_days: u64,
    pub last_30_days: u64,
    pub ever: u64,
}

/// Servers by one setting, most common first.
#[derive(Debug, Clone, Serialize)]
pub struct Breakdown {
    pub title: String,
    pub rows: Vec<(String, u64)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Scans {
    /// Cards recognised in the last 30 days, at least.
    pub recognised: u64,
    /// Cards not recognised in the last 30 days, at least.
    pub unrecognised: u64,
    /// Server-days in each range of cards recognised.
    pub ranges: Vec<(String, u64)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorRow {
    pub kind: String,
    /// Servers that reported it in the last 30 days.
    pub servers: u64,
    /// Errors of this kind, at least.
    pub count: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct GitHub {
    pub stars: Option<i64>,
    /// Change over the last 30 days, when known then.
    pub stars_change: Option<i64>,
    pub forks: Option<i64>,
    pub watchers: Option<i64>,
    pub open_issues: Option<i64>,
    pub sponsors: Option<i64>,
    pub sponsor_income_cents: Option<i64>,
    /// Pulls of the container image (GitHub's package page).
    pub image_downloads: Option<i64>,
    pub release_downloads: i64,
    pub releases: Vec<Release>,
    /// Page views and unique visitors per day, oldest first.
    pub views: Vec<(String, i64)>,
    pub visitors: Vec<(String, i64)>,
    pub clones: Vec<(String, i64)>,
    /// When the numbers were last fetched, and what couldn't be.
    pub updated: Option<String>,
    pub problems: Vec<String>,
}

/// The lower bound of a range of `COUNT_RANGES`.
fn lower_bound(range: &str) -> u64 {
    range
        .split(['-', '+'])
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

fn day(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

/// Counts of `label` sorted by count, then label.
fn ranked(counts: HashMap<String, u64>) -> Vec<(String, u64)> {
    let mut rows: Vec<_> = counts.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    rows
}

pub fn compute(store: &Store, today: NaiveDate, github: GitHub) -> Result<Metrics> {
    let chart_start = today - Duration::days(CHART_DAYS - 1);
    let window_start = day(today - Duration::days(WINDOW_DAYS - 1));
    let week_start = day(today - Duration::days(6));
    let today_s = day(today);
    let rows = store.pings_since(&day(chart_start))?;

    // Servers per day.
    let mut per_day: BTreeMap<String, BTreeSet<&str>> = BTreeMap::new();
    for r in &rows {
        per_day
            .entry(r.day.clone())
            .or_default()
            .insert(&r.ping.install_id);
    }
    let daily_servers = (0..CHART_DAYS)
        .map(|i| {
            let d = day(chart_start + Duration::days(i));
            let n = per_day.get(&d).map_or(0, |s| s.len() as u64);
            (d, n)
        })
        .collect();
    let distinct_since = |from: &str| {
        rows.iter()
            .filter(|r| r.day.as_str() >= from)
            .map(|r| r.ping.install_id.as_str())
            .collect::<BTreeSet<_>>()
            .len() as u64
    };
    let servers = Servers {
        today: distinct_since(&today_s),
        last_7_days: distinct_since(&week_start),
        last_30_days: distinct_since(&window_start),
        ever: store.servers_ever()?,
    };

    // New servers per week (weeks start on Monday).
    let monday = |d: NaiveDate| d - Duration::days(d.weekday().num_days_from_monday() as i64);
    let first_week = monday(today) - Duration::weeks(WEEKS - 1);
    let mut new_per_week: BTreeMap<String, u64> = (0..WEEKS)
        .map(|i| (day(first_week + Duration::weeks(i)), 0))
        .collect();
    for first in store.first_seen_since(&day(first_week))? {
        if let Ok(d) = NaiveDate::parse_from_str(&first, "%Y-%m-%d") {
            *new_per_week.entry(day(monday(d))).or_default() += 1;
        }
    }
    let new_servers = new_per_week.into_iter().collect();

    // Each server's latest ping of the window.
    let window: Vec<_> = rows
        .iter()
        .filter(|r| r.day.as_str() >= window_start.as_str())
        .collect();
    let mut latest: HashMap<&str, &Ping> = HashMap::new();
    for r in &window {
        // Rows come oldest first, so the last one wins.
        latest.insert(&r.ping.install_id, &r.ping);
    }
    type Pick = fn(&Ping) -> String;
    let settings: [(&str, Pick); 10] = [
        ("Version", |p| p.version.clone()),
        ("Installed as", |p| p.install.clone()),
        ("Operating system", |p| format!("{} {}", p.os, p.arch)),
        ("Storage", |p| p.storage.clone()),
        ("Card reading", |p| p.recognition.clone()),
        ("Vision model", |p| {
            p.llm_provider.clone().unwrap_or_else(|| "none".into())
        }),
        ("OCR engine", |p| p.ocr.clone()),
        ("Accounts", |p| p.accounts.clone()),
        ("Card Kingdom prices", |p| {
            if p.card_kingdom { "on" } else { "off" }.into()
        }),
        ("Setup finished", |p| {
            if p.setup_done { "yes" } else { "no" }.into()
        }),
    ];
    let breakdowns = settings
        .iter()
        .map(|(title, pick)| {
            let mut counts = HashMap::new();
            for p in latest.values() {
                *counts.entry(pick(p)).or_default() += 1;
            }
            Breakdown {
                title: title.to_string(),
                rows: ranked(counts),
            }
        })
        .collect();

    // Scans and errors over the window, every server-day.
    let mut scan_ranges: BTreeMap<&str, u64> = COUNT_RANGES.iter().map(|r| (*r, 0)).collect();
    let (mut recognised, mut unrecognised) = (0, 0);
    let mut errors: BTreeMap<&str, (BTreeSet<&str>, u64)> = BTreeMap::new();
    for r in &window {
        let p = &r.ping;
        if let Some(n) = scan_ranges.get_mut(p.scans.as_str()) {
            *n += 1;
        }
        recognised += lower_bound(&p.scans);
        unrecognised += lower_bound(&p.unrecognised);
        for (kind, range) in &p.errors {
            let e = errors.entry(kind.as_str()).or_default();
            e.0.insert(&p.install_id);
            e.1 += lower_bound(range).max(1);
        }
    }
    let scans = Scans {
        recognised,
        unrecognised,
        ranges: COUNT_RANGES
            .iter()
            .map(|r| (r.to_string(), scan_ranges[r]))
            .collect(),
    };
    let errors = ERROR_KINDS
        .iter()
        .filter_map(|kind| {
            errors.get(kind).map(|(servers, count)| ErrorRow {
                kind: kind.to_string(),
                servers: servers.len() as u64,
                count: *count,
            })
        })
        .collect();

    Ok(Metrics {
        today: today_s,
        servers,
        daily_servers,
        new_servers,
        breakdowns,
        scans,
        errors,
        github,
    })
}

/// `series` on every day of the charts, 0 where it has none.
fn every_day(series: Vec<(String, i64)>, today: NaiveDate) -> Vec<(String, i64)> {
    let known: HashMap<String, i64> = series.into_iter().collect();
    (0..CHART_DAYS)
        .map(|i| {
            let d = day(today - Duration::days(CHART_DAYS - 1 - i));
            let v = known.get(&d).copied().unwrap_or(0);
            (d, v)
        })
        .collect()
}

/// The stored GitHub numbers, as of `today`.
pub fn github(store: &Store, today: NaiveDate) -> Result<GitHub> {
    let latest =
        |metric: &str| -> Result<Option<i64>> { Ok(store.github_latest(metric)?.map(|(_, v)| v)) };
    let stars = latest("stars")?;
    let month_ago = day(today - Duration::days(WINDOW_DAYS));
    let stars_change = match (stars, store.github_on("stars", &month_ago)?) {
        (Some(now), Some(then)) => Some(now - then),
        _ => None,
    };
    let since = day(today - Duration::days(CHART_DAYS - 1));
    let releases = store.releases()?;
    Ok(GitHub {
        stars,
        stars_change,
        forks: latest("forks")?,
        watchers: latest("watchers")?,
        open_issues: latest("open_issues")?,
        sponsors: latest("sponsors")?,
        sponsor_income_cents: latest("sponsor_income_cents")?,
        image_downloads: latest("image_downloads")?,
        release_downloads: releases.iter().map(|r| r.downloads).sum(),
        releases,
        views: every_day(store.github_series("views", &since)?, today),
        visitors: every_day(store.github_series("view_uniques", &since)?, today),
        clones: every_day(store.github_series("clones", &since)?, today),
        updated: None,
        problems: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::ping;

    fn date(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn ranges_count_from_their_lower_bound() {
        let bounds: Vec<_> = COUNT_RANGES.iter().map(|r| lower_bound(r)).collect();
        assert_eq!(bounds, [0, 1, 10, 100, 1000]);
    }

    #[test]
    fn servers_settings_scans_and_errors() {
        let store = Store::in_memory().unwrap();
        // Server 1: every day for a week, upgraded on the last day.
        for d in 19..=25 {
            let mut p = ping(1);
            if d == 25 {
                p.version = "1.0.0-alpha.2".into();
                p.errors.insert("camera".into(), "1-9".into());
            }
            store.record(&format!("2026-10-{d}"), &p, "t").unwrap();
        }
        // Server 2: Windows, OCR only, 40 days ago (outside the window) and today.
        let mut p2 = ping(2);
        p2.os = "windows".into();
        p2.install = "windows-release".into();
        p2.recognition = "ocr".into();
        p2.llm_provider = None;
        p2.scans = "100-999".into();
        p2.errors.insert("camera".into(), "10-99".into());
        store.record("2026-09-15", &p2, "t").unwrap();
        store.record("2026-10-25", &p2, "t").unwrap();
        // Server 3: once, 10 days ago.
        store.record("2026-10-15", &ping(3), "t").unwrap();

        let m = compute(&store, date("2026-10-25"), GitHub::default()).unwrap();
        assert_eq!(
            (
                m.servers.today,
                m.servers.last_7_days,
                m.servers.last_30_days,
                m.servers.ever
            ),
            (2, 2, 3, 3)
        );
        assert_eq!(m.daily_servers.len(), CHART_DAYS as usize);
        assert_eq!(
            m.daily_servers.last().unwrap(),
            &("2026-10-25".to_string(), 2)
        );

        let breakdown = |title: &str| {
            m.breakdowns
                .iter()
                .find(|b| b.title == title)
                .unwrap()
                .rows
                .clone()
        };
        assert_eq!(
            breakdown("Version"),
            [
                ("1.0.0-alpha.1".to_string(), 2),
                ("1.0.0-alpha.2".to_string(), 1)
            ],
            "each server counts once, by its latest ping"
        );
        assert_eq!(
            breakdown("Vision model"),
            [("ollama".to_string(), 2), ("none".to_string(), 1)]
        );

        // 7 + 1 server-days of 10-99 and one of 100-999 in the window.
        assert_eq!(m.scans.recognised, 8 * 10 + 100);
        assert_eq!(
            m.scans.ranges.iter().map(|(_, n)| *n).collect::<Vec<_>>(),
            [0, 0, 8, 1, 0]
        );
        assert_eq!(m.errors.len(), 1);
        assert_eq!(
            (
                m.errors[0].kind.as_str(),
                m.errors[0].servers,
                m.errors[0].count
            ),
            ("camera", 2, 1 + 10),
            "the ping from 40 days ago is outside the window"
        );

        let weeks: u64 = m.new_servers.iter().map(|(_, n)| n).sum();
        assert_eq!(weeks, 3);
        assert_eq!(
            m.new_servers.last().unwrap().0,
            "2026-10-19",
            "weeks start on Monday"
        );
    }

    #[test]
    fn github_numbers_with_the_month_change() {
        let store = Store::in_memory().unwrap();
        store.set_github("2026-09-20", "stars", 10).unwrap();
        store.set_github("2026-10-25", "stars", 16).unwrap();
        store.set_github("2026-10-24", "views", 30).unwrap();
        store
            .set_releases(&[Release {
                tag: "v0.3.0".into(),
                published: "2026-09-01T00:00:00Z".into(),
                downloads: 40,
            }])
            .unwrap();
        let g = github(&store, date("2026-10-25")).unwrap();
        assert_eq!((g.stars, g.stars_change), (Some(16), Some(6)));
        assert_eq!(g.release_downloads, 40);
        assert_eq!(g.views.len(), CHART_DAYS as usize);
        assert_eq!(
            g.views[CHART_DAYS as usize - 2],
            ("2026-10-24".to_string(), 30)
        );
        assert_eq!(g.views.last().unwrap(), &("2026-10-25".to_string(), 0));
        assert_eq!(g.sponsors, None);
    }
}
