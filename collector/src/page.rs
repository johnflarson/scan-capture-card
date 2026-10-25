//! The metrics page: one self-contained HTML document (no scripts, no
//! outside files), light or dark with the reader's system. Charts are SVG
//! bars with a tooltip on each day.

use std::fmt::Write;

use crate::metrics::{Breakdown, Metrics};

/// `n` with thousands separators.
fn num(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// A number tile; `None` shows a dash.
fn tile(label: &str, value: Option<i64>, note: Option<String>) -> String {
    let value = value.map_or("–".to_string(), num);
    let note = note.map_or(String::new(), |n| {
        format!("<span class=\"note\">{}</span>", esc(&n))
    });
    format!(
        "<div class=\"tile\"><span class=\"label\">{}</span><span class=\"value\">{value}</span>{note}</div>",
        esc(label)
    )
}

/// Bars over days (or weeks): one series, a tooltip per bar, the maximum
/// and zero on the left, the first and last date below.
fn bars(series: &[(String, i64)], unit: &str) -> String {
    if series.iter().all(|(_, v)| *v == 0) {
        return "<p class=\"empty\">Nothing yet.</p>".to_string();
    }
    let (w, h, left, bottom, top) = (720.0, 170.0, 60.0, 22.0, 8.0);
    let plot_w = w - left;
    let plot_h = h - bottom - top;
    let max = series.iter().map(|(_, v)| *v).max().unwrap_or(1).max(1) as f64;
    let step = plot_w / series.len() as f64;
    let bar_w = (step - 2.0).max(1.0);
    let base = top + plot_h;
    let mut svg = format!(
        "<svg class=\"chart\" viewBox=\"0 0 {w} {h}\" role=\"img\" aria-label=\"{} per day\">",
        esc(unit)
    );
    // Grid: the maximum and half of it.
    for (frac, label) in [(1.0, max), (0.5, max / 2.0)] {
        let y = base - plot_h * frac;
        let _ = write!(
            svg,
            "<line class=\"grid\" x1=\"{left}\" x2=\"{w}\" y1=\"{y:.1}\" y2=\"{y:.1}\"/>\
             <text class=\"axis\" x=\"{}\" y=\"{:.1}\" text-anchor=\"end\">{}</text>",
            left - 6.0,
            y + 4.0,
            num(label.round() as i64)
        );
    }
    let _ = write!(
        svg,
        "<line class=\"base\" x1=\"{left}\" x2=\"{w}\" y1=\"{base}\" y2=\"{base}\"/>\
         <text class=\"axis\" x=\"{}\" y=\"{}\" text-anchor=\"end\">0</text>",
        left - 6.0,
        base + 4.0
    );
    for (i, (day, v)) in series.iter().enumerate() {
        let x = left + i as f64 * step + 1.0;
        let bh = plot_h * (*v as f64) / max;
        let tip = format!("{day}: {} {unit}", num(*v));
        let _ = write!(svg, "<g class=\"bar\"><title>{}</title>", esc(&tip));
        // The hover target is the whole column, wider than the bar.
        let _ = write!(
            svg,
            "<rect class=\"hit\" x=\"{:.1}\" y=\"{top}\" width=\"{step:.1}\" height=\"{plot_h}\"/>",
            x - 1.0
        );
        if *v > 0 {
            // Rounded at the data end, square on the baseline.
            let r = 4.0_f64.min(bar_w / 2.0).min(bh);
            let y = base - bh;
            let _ = write!(
                svg,
                "<path class=\"mark\" d=\"M{x:.1},{base} V{:.1} Q{x:.1},{y:.1} {:.1},{y:.1} H{:.1} Q{:.1},{y:.1} {:.1},{:.1} V{base} Z\"/>",
                y + r,
                x + r,
                x + bar_w - r,
                x + bar_w,
                x + bar_w,
                y + r
            );
        }
        svg.push_str("</g>");
    }
    if let (Some(first), Some(last)) = (series.first(), series.last()) {
        let _ = write!(
            svg,
            "<text class=\"axis\" x=\"{left}\" y=\"{}\">{}</text>\
             <text class=\"axis\" x=\"{w}\" y=\"{}\" text-anchor=\"end\">{}</text>",
            h - 4.0,
            esc(&first.0),
            h - 4.0,
            esc(&last.0)
        );
    }
    svg.push_str("</svg>");
    svg
}

/// Servers by one setting: a row per value with its share.
fn breakdown(b: &Breakdown) -> String {
    let total: u64 = b.rows.iter().map(|(_, n)| n).sum();
    let mut html = format!("<div class=\"breakdown\"><h3>{}</h3>", esc(&b.title));
    if b.rows.is_empty() {
        html.push_str("<p class=\"empty\">No servers yet.</p>");
    }
    for (label, n) in &b.rows {
        let share = if total > 0 {
            *n as f64 * 100.0 / total as f64
        } else {
            0.0
        };
        let _ = write!(
            html,
            "<div class=\"share\"><span class=\"name\">{}</span><span class=\"count\">{} <span class=\"pct\">({share:.0}%)</span></span>\
             <span class=\"track\"><span class=\"fill\" style=\"width:{share:.1}%\"></span></span></div>",
            esc(label),
            num(*n as i64)
        );
    }
    html.push_str("</div>");
    html
}

fn as_i64(series: &[(String, u64)]) -> Vec<(String, i64)> {
    series.iter().map(|(d, v)| (d.clone(), *v as i64)).collect()
}

pub fn render(m: &Metrics) -> String {
    let mut body = String::new();
    let s = &m.servers;
    let _ = write!(
        body,
        "<header><h1>The Shuffling Tome</h1>\
         <p class=\"sub\">Servers that allowed usage statistics, and the project on GitHub. {}, UTC.</p></header>",
        esc(&m.today)
    );

    body.push_str("<section><h2>Servers</h2><div class=\"tiles\">");
    body.push_str(&tile("Today", Some(s.today as i64), None));
    body.push_str(&tile("Last 7 days", Some(s.last_7_days as i64), None));
    body.push_str(&tile("Last 30 days", Some(s.last_30_days as i64), None));
    body.push_str(&tile("Ever", Some(s.ever as i64), None));
    body.push_str("</div><h3>Servers that sent a ping, per day</h3>");
    body.push_str(&bars(&as_i64(&m.daily_servers), "servers"));
    body.push_str("<h3>New servers, per week</h3>");
    body.push_str(&bars(&as_i64(&m.new_servers), "new servers"));
    body.push_str("</section>");

    body.push_str(
        "<section><h2>How they are set up</h2><p class=\"sub\">Each server counts once, by its latest ping of the last 30 days.</p><div class=\"breakdowns\">",
    );
    for b in &m.breakdowns {
        body.push_str(&breakdown(b));
    }
    body.push_str("</div></section>");

    let sc = &m.scans;
    body.push_str("<section><h2>Scanning, last 30 days</h2><div class=\"tiles\">");
    body.push_str(&tile(
        "Cards recognised",
        Some(sc.recognised as i64),
        Some("at least".into()),
    ));
    body.push_str(&tile(
        "Not recognised",
        Some(sc.unrecognised as i64),
        Some("at least".into()),
    ));
    body.push_str("</div>");
    body.push_str(&breakdown(&Breakdown {
        title: "Server-days by cards recognised".into(),
        rows: sc.ranges.clone(),
    }));
    body.push_str("</section>");

    body.push_str("<section><h2>Errors, last 30 days</h2>");
    if m.errors.is_empty() {
        body.push_str("<p class=\"empty\">None reported.</p>");
    } else {
        body.push_str(
            "<table><thead><tr><th>Kind</th><th class=\"n\">Servers</th><th class=\"n\">Errors (at least)</th></tr></thead><tbody>",
        );
        for e in &m.errors {
            let _ = write!(
                body,
                "<tr><td>{}</td><td class=\"n\">{}</td><td class=\"n\">{}</td></tr>",
                esc(&e.kind.replace('_', " ")),
                num(e.servers as i64),
                num(e.count as i64)
            );
        }
        body.push_str("</tbody></table>");
    }
    body.push_str("</section>");

    let g = &m.github;
    body.push_str("<section><h2>GitHub</h2><div class=\"tiles\">");
    let change = g.stars_change.map(|c| {
        let sign = if c >= 0 { "+" } else { "" };
        format!("{sign}{} in 30 days", num(c))
    });
    body.push_str(&tile("Stars", g.stars, change));
    body.push_str(&tile("Forks", g.forks, None));
    body.push_str(&tile("Watchers", g.watchers, None));
    body.push_str(&tile("Open issues", g.open_issues, None));
    body.push_str(&tile(
        "Sponsors",
        g.sponsors,
        g.sponsor_income_cents
            .map(|c| format!("${} a month", num(c / 100))),
    ));
    body.push_str(&tile("Release downloads", Some(g.release_downloads), None));
    body.push_str(&tile(
        "Image pulls",
        g.image_downloads,
        g.image_downloads.is_none().then(|| "no image yet".into()),
    ));
    body.push_str("</div>");
    body.push_str("<h3>Page views, per day</h3>");
    body.push_str(&bars(&g.views, "views"));
    body.push_str("<h3>Unique visitors, per day</h3>");
    body.push_str(&bars(&g.visitors, "visitors"));
    body.push_str("<h3>Clones, per day</h3>");
    body.push_str(&bars(&g.clones, "clones"));
    if !g.releases.is_empty() {
        body.push_str(
            "<h3>Releases</h3><table><thead><tr><th>Release</th><th>Published</th><th class=\"n\">Downloads</th></tr></thead><tbody>",
        );
        for r in &g.releases {
            let _ = write!(
                body,
                "<tr><td>{}</td><td>{}</td><td class=\"n\">{}</td></tr>",
                esc(&r.tag),
                esc(r.published.get(..10).unwrap_or(&r.published)),
                num(r.downloads)
            );
        }
        body.push_str("</tbody></table>");
    }
    let _ = write!(
        body,
        "<p class=\"sub\">{}</p>",
        match &g.updated {
            Some(t) => format!("Fetched from GitHub at {} UTC.", esc(t)),
            None => "Not fetched from GitHub yet.".into(),
        }
    );
    for p in &g.problems {
        let _ = write!(body, "<p class=\"problem\">{}</p>", esc(p));
    }
    body.push_str("</section><footer><a href=\"api/metrics\">These numbers as JSON</a></footer>");

    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <meta name=\"robots\" content=\"noindex\">\
         <title>Tome metrics</title><style>{STYLE}</style></head><body><main>{body}</main></body></html>"
    )
}

const STYLE: &str = r#"
:root {
  --bg: #f7f8f8; --surface: #ffffff; --line: #dde3e2; --grid: #e8eceb;
  --ink: #1b2120; --ink-2: #4b5755; --ink-3: #6c7876;
  --mark: #1f7a70; --mark-hover: #145a52; --track: #e3ebe9; --problem: #9a3b16;
}
@media (prefers-color-scheme: dark) {
  :root {
    --bg: #121615; --surface: #1a201f; --line: #2c3533; --grid: #252d2b;
    --ink: #e6ecea; --ink-2: #b3bfbc; --ink-3: #8d9a97;
    --mark: #5cc2b4; --mark-hover: #8fdcd1; --track: #263331; --problem: #f0a07d;
  }
}
* { box-sizing: border-box; }
body { margin: 0; background: var(--bg); color: var(--ink);
  font: 15px/1.5 system-ui, -apple-system, "Segoe UI", sans-serif; }
main { max-width: 1040px; margin: 0 auto; padding: 24px 16px 48px; }
h1 { font-size: 28px; margin: 0; letter-spacing: -0.01em; }
h2 { font-size: 20px; margin: 0 0 12px; }
h3 { font-size: 14px; font-weight: 600; color: var(--ink-2); margin: 20px 0 6px; }
.sub { color: var(--ink-3); margin: 4px 0 0; font-size: 13px; }
header { margin-bottom: 20px; }
section { background: var(--surface); border: 1px solid var(--line); border-radius: 12px;
  padding: 18px 20px; margin: 0 0 16px; }
.tiles { display: grid; grid-template-columns: repeat(auto-fill, minmax(150px, 1fr)); gap: 10px; }
.tile { display: flex; flex-direction: column; padding: 10px 12px; border-radius: 8px; background: var(--bg); }
.tile .label { font-size: 12px; color: var(--ink-3); }
.tile .value { font-size: 26px; font-weight: 600; font-variant-numeric: tabular-nums; }
.tile .note { font-size: 12px; color: var(--ink-2); }
svg.chart { width: 100%; height: auto; display: block; }
svg.chart .grid { stroke: var(--grid); stroke-width: 1; }
svg.chart .base { stroke: var(--line); stroke-width: 1; }
svg.chart .axis { fill: var(--ink-3); font-size: 11px; }
svg.chart .mark { fill: var(--mark); }
svg.chart .hit { fill: transparent; }
svg.chart .bar:hover .mark { fill: var(--mark-hover); }
.breakdowns { display: grid; grid-template-columns: repeat(auto-fill, minmax(280px, 1fr)); gap: 4px 24px; }
.breakdown h3 { margin-top: 12px; }
.share { display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 2px 8px; margin: 6px 0; font-size: 13px; }
.share .name { overflow-wrap: anywhere; }
.share .count { font-variant-numeric: tabular-nums; color: var(--ink-2); }
.share .pct { color: var(--ink-3); }
.share .track { grid-column: 1 / -1; height: 6px; border-radius: 3px; background: var(--track); overflow: hidden; }
.share .fill { display: block; height: 100%; border-radius: 0 3px 3px 0; background: var(--mark); }
table { width: 100%; border-collapse: collapse; font-size: 14px; }
th, td { text-align: left; padding: 6px 8px; border-bottom: 1px solid var(--line); }
th { color: var(--ink-3); font-weight: 500; font-size: 12px; }
td.n, th.n { text-align: right; font-variant-numeric: tabular-nums; }
.empty { color: var(--ink-3); font-size: 13px; }
.problem { color: var(--problem); font-size: 13px; margin: 4px 0 0; }
footer { font-size: 13px; }
a { color: var(--mark); }
@media (max-width: 520px) {
  section { padding: 14px; }
  .tile .value { font-size: 22px; }
  /* The chart shrinks to about half; keep its labels readable. */
  svg.chart .axis { font-size: 22px; }
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_get_separators() {
        assert_eq!(num(0), "0");
        assert_eq!(num(999), "999");
        assert_eq!(num(1000), "1,000");
        assert_eq!(num(119_166_230), "119,166,230");
        assert_eq!(num(-1234), "-1,234");
    }

    #[test]
    fn a_chart_has_a_tooltip_per_day() {
        let series = vec![
            ("2026-10-24".to_string(), 3),
            ("2026-10-25".to_string(), 0),
            ("2026-10-26".to_string(), 5),
        ];
        let svg = bars(&series, "servers");
        assert_eq!(svg.matches("<title>").count(), 3);
        assert!(svg.contains("2026-10-26: 5 servers"));
        assert_eq!(svg.matches("class=\"mark\"").count(), 2, "no bar for zero");
        assert!(bars(&[("d".into(), 0)], "x").contains("Nothing yet"));
    }

    #[test]
    fn text_is_escaped() {
        let html = breakdown(&Breakdown {
            title: "<b>".into(),
            rows: vec![("a&b".into(), 1)],
        });
        assert!(html.contains("&lt;b&gt;") && html.contains("a&amp;b"));
    }
}
