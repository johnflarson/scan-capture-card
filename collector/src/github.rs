//! The project's numbers from GitHub, fetched every six hours and kept per
//! day: stars, forks, watchers and open issues; page views, unique visitors
//! and clones (GitHub keeps only 14 days of these, so the daily rows here
//! are the history; they need a token); each release's downloads; pulls of
//! the container image (from the package page, which has no API); and
//! sponsors (GraphQL, needs a token).

use std::time::Duration;

use anyhow::{Context, Result, bail};
use regex::Regex;
use reqwest::Client;
use serde_json::{Value, json};

use crate::store::Release;

/// What GitHub said, ready to store.
#[derive(Debug, Default)]
pub struct Snapshot {
    /// `(day, metric, value)` rows.
    pub daily: Vec<(String, &'static str, i64)>,
    pub releases: Vec<Release>,
    /// Parts that couldn't be fetched, in words.
    pub problems: Vec<String>,
}

pub struct Source {
    pub repo: String,
    pub sponsor: String,
    pub token: Option<String>,
    client: Client,
}

const API: &str = "https://api.github.com";

impl Source {
    pub fn new(repo: String, sponsor: String, token: Option<String>) -> Result<Self> {
        let client = Client::builder()
            .user_agent(concat!("tome-collector/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self {
            repo,
            sponsor,
            token,
            client,
        })
    }

    async fn get(&self, url: &str) -> Result<Value> {
        let mut req = self
            .client
            .get(url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(token) = &self.token {
            req = req.bearer_auth(token);
        }
        let resp = req.send().await.with_context(|| format!("{url} failed"))?;
        let status = resp.status();
        if !status.is_success() {
            bail!("{url}: HTTP {status}");
        }
        Ok(resp.json().await?)
    }

    /// Everything that can be fetched; what can't is listed in `problems`.
    pub async fn fetch(&self, today: &str) -> Snapshot {
        let mut snap = Snapshot::default();
        match self.repository(today).await {
            Ok(rows) => snap.daily.extend(rows),
            Err(e) => snap.problems.push(format!("repository: {e:#}")),
        }
        if self.token.is_some() {
            for (kind, count, uniques) in [
                ("views", "views", "view_uniques"),
                ("clones", "clones", "clone_uniques"),
            ] {
                match self.traffic(kind, count, uniques).await {
                    Ok(rows) => snap.daily.extend(rows),
                    Err(e) => snap.problems.push(format!("{kind}: {e:#}")),
                }
            }
            match self.sponsors(today).await {
                Ok(rows) => snap.daily.extend(rows),
                Err(e) => snap.problems.push(format!("sponsors: {e:#}")),
            }
        } else {
            snap.problems
                .push("traffic and sponsors need GITHUB_TOKEN".to_string());
        }
        match self.releases().await {
            Ok(releases) => snap.releases = releases,
            Err(e) => snap.problems.push(format!("releases: {e:#}")),
        }
        match self.image_downloads().await {
            Ok(Some(n)) => snap.daily.push((today.to_string(), "image_downloads", n)),
            Ok(None) => {}
            Err(e) => snap.problems.push(format!("image: {e:#}")),
        }
        snap
    }

    async fn repository(&self, today: &str) -> Result<Vec<(String, &'static str, i64)>> {
        let repo = self.get(&format!("{API}/repos/{}", self.repo)).await?;
        Ok(repository_rows(&repo, today))
    }

    async fn traffic(
        &self,
        kind: &str,
        count: &'static str,
        uniques: &'static str,
    ) -> Result<Vec<(String, &'static str, i64)>> {
        let body = self
            .get(&format!("{API}/repos/{}/traffic/{kind}", self.repo))
            .await?;
        Ok(traffic_rows(&body, kind, count, uniques))
    }

    async fn releases(&self) -> Result<Vec<Release>> {
        let body = self
            .get(&format!("{API}/repos/{}/releases?per_page=100", self.repo))
            .await?;
        Ok(release_rows(&body))
    }

    /// Pulls of `ghcr.io/<repo>`, read from its package page; `None` while
    /// no image is published.
    async fn image_downloads(&self) -> Result<Option<i64>> {
        let name = self.repo.rsplit('/').next().unwrap_or(&self.repo);
        let url = format!("https://github.com/{}/pkgs/container/{name}", self.repo);
        let resp = self.client.get(&url).send().await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let html = resp.error_for_status()?.text().await?;
        page_downloads(&html)
            .map(Some)
            .context("the package page has no download count (did its layout change?)")
    }

    async fn sponsors(&self, today: &str) -> Result<Vec<(String, &'static str, i64)>> {
        let query = json!({
            "query": "query($login: String!) { user(login: $login) { sponsors { totalCount } monthlyEstimatedSponsorsIncomeInCents } }",
            "variables": { "login": self.sponsor },
        });
        let resp = self
            .client
            .post(format!("{API}/graphql"))
            .bearer_auth(self.token.as_deref().unwrap_or_default())
            .json(&query)
            .send()
            .await?
            .error_for_status()?;
        let body: Value = resp.json().await?;
        let user = &body["data"]["user"];
        let Some(count) = user["sponsors"]["totalCount"].as_i64() else {
            bail!(
                "{}",
                body["errors"][0]["message"]
                    .as_str()
                    .unwrap_or("no sponsor count in the answer")
            );
        };
        let mut rows = vec![(today.to_string(), "sponsors", count)];
        // Only the account's own token sees the income.
        if let Some(cents) = user["monthlyEstimatedSponsorsIncomeInCents"].as_i64() {
            rows.push((today.to_string(), "sponsor_income_cents", cents));
        }
        Ok(rows)
    }
}

fn repository_rows(repo: &Value, today: &str) -> Vec<(String, &'static str, i64)> {
    [
        ("stars", "stargazers_count"),
        ("forks", "forks_count"),
        ("watchers", "subscribers_count"),
        ("open_issues", "open_issues_count"),
    ]
    .into_iter()
    .filter_map(|(metric, field)| repo[field].as_i64().map(|v| (today.to_string(), metric, v)))
    .collect()
}

/// The per-day counts of a traffic answer (`views` or `clones`).
fn traffic_rows(
    body: &Value,
    kind: &str,
    count: &'static str,
    uniques: &'static str,
) -> Vec<(String, &'static str, i64)> {
    let mut rows = Vec::new();
    for day in body[kind].as_array().into_iter().flatten() {
        let Some(date) = day["timestamp"].as_str().and_then(|t| t.get(..10)) else {
            continue;
        };
        if let Some(n) = day["count"].as_i64() {
            rows.push((date.to_string(), count, n));
        }
        if let Some(n) = day["uniques"].as_i64() {
            rows.push((date.to_string(), uniques, n));
        }
    }
    rows
}

fn release_rows(body: &Value) -> Vec<Release> {
    body.as_array()
        .into_iter()
        .flatten()
        .filter(|r| !r["draft"].as_bool().unwrap_or(false))
        .filter_map(|r| {
            Some(Release {
                tag: r["tag_name"].as_str()?.to_string(),
                published: r["published_at"].as_str().unwrap_or_default().to_string(),
                downloads: r["assets"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|a| a["download_count"].as_i64())
                    .sum(),
            })
        })
        .collect()
}

/// "Total downloads" on a package page.
fn page_downloads(html: &str) -> Option<i64> {
    let re = Regex::new(r#"Total downloads</span>\s*<h3 title="([0-9,]+)""#).ok()?;
    re.captures(html)?[1].replace(',', "").parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_repository_traffic_and_releases() {
        let repo = json!({"stargazers_count": 12, "forks_count": 2, "subscribers_count": 3, "open_issues_count": 9});
        let rows = repository_rows(&repo, "2026-10-25");
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0], ("2026-10-25".to_string(), "stars", 12));

        let views = json!({"count": 50, "uniques": 7, "views": [
            {"timestamp": "2026-10-24T00:00:00Z", "count": 30, "uniques": 4},
            {"timestamp": "2026-10-25T00:00:00Z", "count": 20, "uniques": 3}
        ]});
        let rows = traffic_rows(&views, "views", "views", "view_uniques");
        assert_eq!(
            rows,
            [
                ("2026-10-24".to_string(), "views", 30),
                ("2026-10-24".to_string(), "view_uniques", 4),
                ("2026-10-25".to_string(), "views", 20),
                ("2026-10-25".to_string(), "view_uniques", 3),
            ]
        );

        let releases = json!([
            {"tag_name": "v0.3.0", "published_at": "2026-09-01T00:00:00Z", "draft": false,
             "assets": [{"download_count": 30}, {"download_count": 2}]},
            {"tag_name": "v0.4.0", "published_at": null, "draft": true, "assets": []}
        ]);
        assert_eq!(
            release_rows(&releases),
            [Release {
                tag: "v0.3.0".into(),
                published: "2026-09-01T00:00:00Z".into(),
                downloads: 32
            }]
        );
    }

    #[test]
    fn reads_the_package_page_count() {
        let html = r#"<div><span class="d-block color-fg-muted text-small tmp-mb-1">Total downloads</span>
            <h3 title="119166230">119M</h3></div>"#;
        assert_eq!(page_downloads(html), Some(119_166_230));
        assert_eq!(page_downloads("<h3 title=\"5\">5</h3>"), None);
    }
}
