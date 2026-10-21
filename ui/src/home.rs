//! Landing page: big buttons for the three jobs (scan, collection, decks)
//! next to statistics about the collection.

use leptos::prelude::*;
use mtg_scan_shared::{
    CollectionRow, DeckOwnership, Finish, Mode, Slice, StateView, Stats, ValueChange, usd,
};

use crate::app::{CollectionCache, Metric, Tab};
use crate::decks::{DeckCache, DeckStatsBlock, ownership};

#[component]
pub fn HomePage(
    state: RwSignal<Option<StateView>>,
    cache: CollectionCache,
    decks: DeckCache,
    notify: Callback<(String, bool)>,
    go: Callback<Tab>,
) -> impl IntoView {
    decks.ensure(notify);
    let owned = ownership(decks, cache);
    let stats = Memo::new(move |_| cache.data.with(|d| d.as_ref().map(|d| Stats::of(&d.rows))));
    let value_change = move || {
        cache
            .data
            .with(|d| d.as_ref().and_then(|d| d.value_change.clone()))
    };

    let scan_detail = Signal::derive(move || match state.get() {
        Some(s) if s.ready => format!(
            "{} · auto-scan {}",
            if s.mode == Mode::Table {
                "Table mode"
            } else {
                "Single card"
            },
            if s.auto { "on" } else { "off" }
        ),
        Some(_) => "Starting the camera and recognizer…".into(),
        None => "Connecting…".into(),
    });
    let collection_detail = Signal::derive(move || match stats.get() {
        Some(s) => format!(
            "{} cards · {} printings · {}",
            s.totals.cards,
            s.totals.printings,
            usd(Some(s.totals.tcgplayer))
        ),
        None => "Loading…".into(),
    });
    let decks_detail = Signal::derive(move || match decks.list.get() {
        None => "Loading…".into(),
        Some(v) if v.user.is_none() => "Connect Moxfield in .env".into(),
        Some(v) => {
            let complete = owned.with(|o| o.values().filter(|x| x.owned == x.total).count());
            let checked = owned.with(|o| o.len());
            if checked == 0 {
                format!("{} decks on Moxfield", v.decks.len())
            } else {
                format!("{} decks · {complete} complete", v.decks.len())
            }
        }
    });
    let go_decks = Callback::new(move |()| go.run(Tab::Decks));

    view! {
        <div class="home">
            <nav class="launch" aria-label="Start">
                <LaunchButton
                    kind="scan"
                    title="Scan"
                    detail=scan_detail
                    on_click=Callback::new(move |()| go.run(Tab::Scan))
                >
                    <CameraIcon />
                </LaunchButton>
                <LaunchButton
                    kind="collection"
                    title="Collection"
                    detail=collection_detail
                    on_click=Callback::new(move |()| go.run(Tab::Collection))
                >
                    <LibraryIcon />
                </LaunchButton>
                <LaunchButton
                    kind="decks"
                    title="Decks"
                    detail=decks_detail
                    on_click=go_decks
                >
                    <DecksIcon />
                </LaunchButton>
            </nav>

            <section class="card stats">
                <div class="card-header">
                    <h2>"Collection at a glance"</h2>
                    <span class="spacer"></span>
                    <button
                        class="btn text"
                        disabled=move || cache.loading.get()
                        on:click=move |_| cache.load(notify)
                    >
                        {move || if cache.loading.get() { "Loading…" } else { "Refresh" }}
                    </button>
                </div>
                {move || match stats.get() {
                    None => view! {
                        <div class="empty">
                            <div class="spinner inline"></div>
                            "Loading the collection…"
                        </div>
                    }
                    .into_any(),
                    Some(s) if s.totals.cards == 0 => view! {
                        <div class="empty">"No cards yet. Start with Scan."</div>
                    }
                    .into_any(),
                    Some(s) => {
                        view! { <StatsBody stats=s value_change=value_change() decks owned go_decks /> }
                            .into_any()
                    }
                }}
            </section>
        </div>
    }
}

#[component]
fn LaunchButton(
    kind: &'static str,
    title: &'static str,
    detail: Signal<String>,
    on_click: Callback<()>,
    children: Children,
) -> impl IntoView {
    view! {
        <button class=format!("launch-btn {kind}") on:click=move |_| on_click.run(())>
            <span class="launch-icon">{children()}</span>
            <span class="launch-text">
                <span class="launch-title">{title}</span>
                <span class="launch-detail">{move || detail.get()}</span>
            </span>
        </button>
    }
}

#[component]
fn StatsBody(
    stats: Stats,
    value_change: Option<ValueChange>,
    decks: DeckCache,
    owned: Memo<std::collections::HashMap<String, DeckOwnership>>,
    go_decks: Callback<()>,
) -> impl IntoView {
    let t = stats.totals.clone();
    let (change_value, change_kind, change_title) = match &value_change {
        Some(c) => {
            let diff = c.now - c.before;
            let pct = if c.before > 0.0 {
                diff / c.before * 100.0
            } else {
                0.0
            };
            let sign = if diff >= 0.0 { "+" } else { "−" };
            (
                format!("{sign}{} ({sign}{:.1}%)", usd(Some(diff.abs())), pct.abs()),
                if diff >= 0.0 { "up" } else { "down" },
                format!(
                    "TCGplayer value of {} row(s) priced on {} and today",
                    c.rows, c.since
                ),
            )
        }
        None => (
            "—".to_string(),
            "",
            "Appears once a week of daily prices has been recorded (PostgreSQL storage)".into(),
        ),
    };
    let rarity_class = |label: &str| format!("rarity-{}", label.to_lowercase());

    view! {
        <div class="metrics-row home-metrics">
            <Metric label="Cards" value=t.cards.to_string() kind="" />
            <Metric label="Printings" value=t.printings.to_string() kind="" />
            <Metric label="TCGplayer" value=usd(Some(t.tcgplayer)) kind="tcg" />
            <Metric label="Card Kingdom" value=usd(Some(t.cardkingdom)) kind="ck" />
            <div class="metric" title=change_title>
                <div class="label">"Last 7 days"</div>
                <div class=format!("value {change_kind}")>{change_value}</div>
            </div>
        </div>
        <div class="stats-grid">
            <StatBlock title="By color" wide=true>
                <ColorBreakdown slices=stats.by_color.clone() total=t.cards />
            </StatBlock>
            <StatBlock title="By type">
                <Bars slices=stats.by_type.clone() class_of=|_| String::new() />
            </StatBlock>
            <StatBlock title="By rarity">
                <Bars slices=stats.by_rarity.clone() class_of=rarity_class />
            </StatBlock>
            <StatBlock title="Most valuable">
                <CardList rows=stats.most_valuable.clone() show_price=true />
            </StatBlock>
            <StatBlock title="Recently added">
                <CardList rows=stats.recently_added.clone() show_price=false />
            </StatBlock>
            <StatBlock title="Top sets">
                <Bars slices=stats.top_sets.clone() class_of=|_| String::new() />
            </StatBlock>
            <StatBlock title="Finish">
                <Bars
                    slices=stats.by_finish.clone()
                    class_of=|label: &str| {
                        if label == Finish::Nonfoil.label() { String::new() } else { "foil".into() }
                    }
                />
            </StatBlock>
            <StatBlock title="Decks" wide=true>
                <DeckStatsBlock decks owned go_decks />
            </StatBlock>
        </div>
    }
}

#[component]
fn StatBlock(
    title: &'static str,
    #[prop(optional)] wide: bool,
    children: Children,
) -> impl IntoView {
    view! {
        <div class="stat-block" class:wide=wide>
            <h3>{title}</h3>
            {children()}
        </div>
    }
}

/// A stacked bar of the color groups with a legend; `unit` names what is
/// counted ("cards", "decks").
#[component]
pub fn ColorBreakdown(
    slices: Vec<(mtg_scan_shared::ColorGroup, Slice)>,
    total: u32,
    #[prop(default = "cards")] unit: &'static str,
) -> impl IntoView {
    let pct = move |cards: u32| {
        if total == 0 {
            0.0
        } else {
            cards as f64 * 100.0 / total as f64
        }
    };
    let segments = slices
        .iter()
        .filter(|(_, s)| s.cards > 0)
        .map(|(g, s)| {
            view! {
                <span
                    class=format!("seg {}", g.class())
                    style=format!("width: {:.2}%", pct(s.cards))
                    title=format!("{}: {} {unit}", s.label, s.cards)
                ></span>
            }
        })
        .collect_view();
    let legend = slices
        .iter()
        .map(|(g, s)| {
            view! {
                <li class:none=s.cards == 0>
                    <span class=format!("swatch {}", g.class())></span>
                    <span class="legend-label">{s.label.clone()}</span>
                    <span class="legend-count">{s.cards}</span>
                    <span class="legend-pct">{format!("{:.0}%", pct(s.cards))}</span>
                </li>
            }
        })
        .collect_view();
    view! {
        <div class="color-bar">{segments}</div>
        <ul class="legend">{legend}</ul>
    }
}

/// Horizontal bars, scaled to the largest slice; the value is shown on
/// hover when there is one.
#[component]
pub fn Bars(
    slices: Vec<Slice>,
    class_of: fn(&str) -> String,
    #[prop(default = "cards")] unit: &'static str,
) -> impl IntoView {
    let max = slices.iter().map(|s| s.cards).max().unwrap_or(0).max(1);
    let rows = slices
        .into_iter()
        .map(|s| {
            let width = s.cards as f64 * 100.0 / max as f64;
            let title = if s.value > 0.0 {
                format!("{} {unit} · {}", s.cards, usd(Some(s.value)))
            } else {
                format!("{} {unit}", s.cards)
            };
            view! {
                <li title=title>
                    <span class="bar-label">{s.label.clone()}</span>
                    <span class="bar-track">
                        <span
                            class=format!("bar-fill {}", class_of(&s.label))
                            style=format!("width: {width:.2}%")
                        ></span>
                    </span>
                    <span class="bar-count">{s.cards}</span>
                </li>
            }
        })
        .collect_view();
    view! { <ul class="bars">{rows}</ul> }
}

/// A short list of cards with thumbnails.
#[component]
fn CardList(rows: Vec<CollectionRow>, show_price: bool) -> impl IntoView {
    if rows.is_empty() {
        return view! { <p class="muted">"No prices yet."</p> }.into_any();
    }
    rows.into_iter()
        .map(|r| {
            // Scryfall redirects this to the card image.
            let thumb = format!(
                "https://api.scryfall.com/cards/{}?format=image&version=small",
                r.scryfall_id
            );
            let right = if show_price {
                usd(r.tcgplayer)
            } else {
                r.added.clone()
            };
            view! {
                <div class="mini">
                    <img src=thumb alt="" loading="lazy" />
                    <div class="mini-text">
                        <a href=r.scryfall_url.clone() target="_blank" rel="noopener">
                            {r.name.clone()}
                        </a>
                        <span class="muted">
                            {format!("{} #{}", r.set_code, r.number)}
                            {(r.finish != Finish::Nonfoil)
                                .then(|| format!(" · {}", r.finish.label()))}
                            {(r.quantity > 1).then(|| format!(" · ×{}", r.quantity))}
                        </span>
                    </div>
                    <span class=if show_price { "mini-right tcg" } else { "mini-right" }>
                        {right}
                    </span>
                </div>
            }
        })
        .collect_view()
        .into_any()
}

#[component]
fn CameraIcon() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" aria-hidden="true">
            <path d="M12 15.2a3.2 3.2 0 1 0 0-6.4 3.2 3.2 0 0 0 0 6.4z" />
            <path d="M9 2 7.17 4H4a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2V6a2 2 0 0 0-2-2h-3.17L15 2H9zm3 15a5 5 0 1 1 0-10 5 5 0 0 1 0 10z" />
        </svg>
    }
}

#[component]
fn LibraryIcon() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" aria-hidden="true">
            <path d="M4 6H2v14a2 2 0 0 0 2 2h14v-2H4V6zm16-4H8a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V4a2 2 0 0 0-2-2zm-1 9h-4v4h-2v-4H9V9h4V5h2v4h4v2z" />
        </svg>
    }
}

#[component]
pub fn DecksIcon() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" aria-hidden="true">
            <path d="m2.53 19.65 1.34.56v-9.03l-2.43 5.86c-.41 1.02.08 2.19 1.09 2.61zm19.5-3.7L17.07 3.98a2.01 2.01 0 0 0-2.6-1.08L7.1 5.95a2 2 0 0 0-1.08 2.6l4.96 11.97a2 2 0 0 0 2.6 1.08l7.36-3.05a2 2 0 0 0 1.09-2.6zM7.88 8.75a1 1 0 1 1 0-2 1 1 0 0 1 0 2zm-2 11a2 2 0 0 0 2 2h1.45l-3.45-8.34v6.34z" />
        </svg>
    }
}
