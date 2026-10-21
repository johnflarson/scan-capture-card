//! Decks page: the configured Moxfield user's public decks, how much of each
//! the collection already holds, and what is left to buy.

use std::collections::HashMap;

use leptos::prelude::*;
use leptos::task::spawn_local;
use mtg_scan_shared::{
    Board, CardOwnership, ColorGroup, DeckOwnership, DeckPrice, DeckStats, DeckSummary, DeckView,
    DecksView, Have, Slice, usd,
};

use crate::api;
use crate::app::{CollectionCache, Metric};
use crate::filters::count;
use crate::home::DecksIcon;
use crate::mana::ManaSymbols;
use crate::preview::CardLink;

/// The deck list and each deck's cards, shared by Home and Decks. After the
/// list loads, every deck's cards are fetched one after another in the
/// background (the server caches them until the deck changes on Moxfield).
#[derive(Clone, Copy)]
pub struct DeckCache {
    pub list: RwSignal<Option<DecksView>>,
    pub loading: RwSignal<bool>,
    /// Decks with their cards, by deck ID.
    pub details: RwSignal<HashMap<String, DeckView>>,
    /// Decks whose cards couldn't be loaded, with the reason.
    pub failed: RwSignal<HashMap<String, String>>,
    fetching: RwSignal<bool>,
    /// The deck open on the Decks page; kept while switching tabs.
    pub open: RwSignal<Option<String>>,
}

impl DeckCache {
    pub fn new() -> Self {
        Self {
            list: RwSignal::new(None),
            loading: RwSignal::new(false),
            details: RwSignal::new(HashMap::new()),
            failed: RwSignal::new(HashMap::new()),
            fetching: RwSignal::new(false),
            open: RwSignal::new(None),
        }
    }

    /// Load the deck list (`refresh`: ask Moxfield now), then any deck whose
    /// cards are missing or older than the list says.
    pub fn load(self, refresh: bool, notify: Callback<(String, bool)>) {
        if self.loading.get_untracked() {
            return;
        }
        self.loading.set(true);
        spawn_local(async move {
            match api::decks(refresh).await {
                Ok(view) => {
                    // Forget decks no longer listed (deleted or made private).
                    let listed: std::collections::HashSet<String> =
                        view.decks.iter().map(|d| d.id.clone()).collect();
                    self.details
                        .update(|d| d.retain(|id, _| listed.contains(id)));
                    self.failed.set(HashMap::new());
                    if self
                        .open
                        .get_untracked()
                        .is_some_and(|id| !listed.contains(&id))
                    {
                        self.open.set(None);
                    }
                    self.list.set(Some(view));
                    self.fetch_cards();
                }
                Err(e) => notify.run((format!("Could not load the decks: {e}"), true)),
            }
            self.loading.set(false);
        });
    }

    /// Load the list unless it already is.
    pub fn ensure(self, notify: Callback<(String, bool)>) {
        if self.list.with_untracked(Option::is_none) {
            self.load(false, notify);
        }
    }

    /// Try a deck that failed again.
    pub fn retry(self, id: &str) {
        self.failed.update(|f| {
            f.remove(id);
        });
        self.fetch_cards();
    }

    /// The next listed deck whose cards are missing or out of date.
    fn next_stale(self) -> Option<String> {
        self.list.with_untracked(|list| {
            let list = list.as_ref()?;
            self.details.with_untracked(|details| {
                self.failed.with_untracked(|failed| {
                    list.decks
                        .iter()
                        .find(|s| {
                            !failed.contains_key(&s.id)
                                && details
                                    .get(&s.id)
                                    .is_none_or(|d| d.summary.updated != s.updated)
                        })
                        .map(|s| s.id.clone())
                })
            })
        })
    }

    fn fetch_cards(self) {
        if self.fetching.get_untracked() {
            return;
        }
        self.fetching.set(true);
        spawn_local(async move {
            while let Some(id) = self.next_stale() {
                match api::deck(&id).await {
                    Ok(deck) => self.details.update(|m| {
                        m.insert(id, deck);
                    }),
                    Err(e) => self.failed.update(|m| {
                        m.insert(id, e);
                    }),
                }
            }
            self.fetching.set(false);
        });
    }

    /// (decks with cards loaded, decks listed).
    pub fn progress(self) -> (usize, usize) {
        let listed = self.list.with(|l| l.as_ref().map_or(0, |l| l.decks.len()));
        let done = self.details.with(|d| d.len()) + self.failed.with(|f| f.len());
        (done.min(listed), listed)
    }

    pub fn checking(self) -> bool {
        self.fetching.get() || self.loading.get()
    }
}

/// Each loaded deck checked against the collection. Empty until the
/// collection has loaded, so nothing shows as missing by mistake.
pub fn ownership(decks: DeckCache, cache: CollectionCache) -> Memo<HashMap<String, DeckOwnership>> {
    Memo::new(move |_| {
        cache.data.with(|c| {
            let Some(c) = c else {
                return HashMap::new();
            };
            decks.details.with(|details| {
                details
                    .iter()
                    .map(|(id, deck)| (id.clone(), DeckOwnership::of(deck, &c.rows)))
                    .collect()
            })
        })
    })
}

#[component]
pub fn DecksPage(
    decks: DeckCache,
    cache: CollectionCache,
    notify: Callback<(String, bool)>,
) -> impl IntoView {
    decks.ensure(notify);
    let owned = ownership(decks, cache);
    move || match decks.list.get() {
        None => view! {
            <section class="card">
                <div class="empty">
                    <div class="spinner inline"></div>
                    "Loading decks from Moxfield…"
                </div>
            </section>
        }
        .into_any(),
        Some(view) if view.user.is_none() => view! { <SetupHelp /> }.into_any(),
        Some(view) => match decks.open.get() {
            Some(id) => view! { <DeckDetail id decks owned /> }.into_any(),
            None => view! { <DeckList view decks owned notify /> }.into_any(),
        },
    }
}

#[component]
fn SetupHelp() -> impl IntoView {
    view! {
        <section class="card placeholder">
            <div class="launch-icon big"><DecksIcon /></div>
            <h1>"Decks"</h1>
            <p>"Connect a Moxfield account to see its public decks, which cards you own and what each deck costs to finish."</p>
            <p>
                "Add this line to the " <code>".env"</code>
                " file next to mtg-scan.toml, then restart mtg-scan:"
            </p>
            <pre class="code">"MTG_SCAN_MOXFIELD_USER=https://moxfield.com/users/YourName"</pre>
        </section>
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Sort {
    Updated,
    Name,
    Owned,
    ToFinish,
}

impl Sort {
    const ALL: [Sort; 4] = [Sort::Updated, Sort::Name, Sort::Owned, Sort::ToFinish];

    fn label(self) -> &'static str {
        match self {
            Sort::Updated => "Updated",
            Sort::Name => "Name",
            Sort::Owned => "Most owned",
            Sort::ToFinish => "Cheapest to finish",
        }
    }
}

#[component]
fn DeckList(
    view: DecksView,
    decks: DeckCache,
    owned: Memo<HashMap<String, DeckOwnership>>,
    notify: Callback<(String, bool)>,
) -> impl IntoView {
    let search = RwSignal::new(String::new());
    let sort = RwSignal::new(Sort::Updated);
    let all = StoredValue::new(view.decks.clone());

    let shown = move || {
        let needle = search.get().to_lowercase();
        let mut list: Vec<DeckSummary> = all.with_value(|d| {
            d.iter()
                .filter(|d| {
                    needle.is_empty()
                        || d.name.to_lowercase().contains(&needle)
                        || d.format_label().to_lowercase().contains(&needle)
                })
                .cloned()
                .collect()
        });
        let by = sort.get();
        owned.with(|o| {
            let pct = |d: &DeckSummary| o.get(&d.id).map_or(-1, |x| x.percent() as i64);
            let cost = |d: &DeckSummary| o.get(&d.id).map_or(f64::MAX, |x| x.missing.tcgplayer);
            match by {
                // The server lists most recently changed first.
                Sort::Updated => {}
                Sort::Name => list.sort_by_key(|d| d.name.to_lowercase()),
                Sort::Owned => list.sort_by_key(|d| std::cmp::Reverse(pct(d))),
                Sort::ToFinish => list.sort_by(|a, b| cost(a).total_cmp(&cost(b))),
            }
        });
        list
    };

    let progress = move || {
        let (done, listed) = decks.progress();
        decks
            .checking()
            .then(|| format!("Checking deck cards… {done} of {listed}"))
    };

    view! {
        <div class="collection-head">
            <h1>"Decks"</h1>
            {view
                .profile_url
                .clone()
                .map(|url| {
                    view! {
                        <a class="location" href=url target="_blank" rel="noopener">
                            {format!("{} on Moxfield", view.user.clone().unwrap_or_default())}
                        </a>
                    }
                })}
            <span class="location">{progress}</span>
            <input
                class="search"
                type="search"
                placeholder="Search decks or formats"
                prop:value=move || search.get()
                on:input=move |ev| search.set(event_target_value(&ev))
            />
            <button
                class="btn tonal"
                disabled=move || decks.loading.get()
                on:click=move |_| decks.load(true, notify)
            >
                {move || if decks.loading.get() { "Loading…" } else { "Refresh" }}
            </button>
        </div>
        {view.warning.clone().map(|w| view! { <div class="banner">{w}</div> })}
        <div class="filter-row deck-sort">
            <span class="filter-label">"Sort"</span>
            <div class="segmented" role="group" aria-label="Sort decks">
                {Sort::ALL
                    .into_iter()
                    .map(|s| {
                        view! {
                            <button class:selected=move || sort.get() == s on:click=move |_| sort.set(s)>
                                {s.label()}
                            </button>
                        }
                    })
                    .collect_view()}
            </div>
        </div>
        <div class="deck-grid">
            <For
                each=shown
                key=|d| d.id.clone()
                children=move |d| view! { <DeckTile summary=d decks owned /> }
            />
        </div>
        <Show when=move || all.with_value(Vec::is_empty)>
            <section class="card">
                <div class="empty">"No public decks on this Moxfield account."</div>
            </section>
        </Show>
    }
}

/// Mana symbols for a color identity (the colorless symbol for none).
fn pips(colors: &[String]) -> impl IntoView + use<> {
    view! { <ManaSymbols colors=colors.to_vec() /> }
}

/// A price sum, with "+" when some copies have no price and aren't in it.
fn sum(amount: f64, unpriced: u32) -> String {
    let total = usd(Some(amount));
    if unpriced > 0 {
        format!("{total}+")
    } else {
        total
    }
}

/// Explains the "+" on totals that leave unpriced copies out.
fn unpriced_note(value: &DeckPrice) -> Option<String> {
    let copies = |n: u32| {
        if n == 1 {
            "1 copy".to_string()
        } else {
            format!("{n} copies")
        }
    };
    let parts: Vec<String> = [
        (value.tcgplayer_unpriced, "TCGplayer"),
        (value.cardkingdom_unpriced, "Card Kingdom"),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, store)| format!("{} without a {store} price", copies(n)))
    .collect();
    (!parts.is_empty()).then(|| {
        format!(
            "+ Not included: {}. Moxfield has no price for them.",
            parts.join(", ")
        )
    })
}

fn date(updated: &str) -> &str {
    updated.get(..10).unwrap_or(updated)
}

#[component]
fn DeckTile(
    summary: DeckSummary,
    decks: DeckCache,
    owned: Memo<HashMap<String, DeckOwnership>>,
) -> impl IntoView {
    let id = summary.id.clone();
    let open_id = id.clone();
    let size = summary.mainboard + summary.sideboard;
    let status = move || {
        if let Some(o) = owned.with(|m| m.get(&id).cloned()) {
            let pct = o.percent();
            return view! {
                <div class="own-bar" title=format!("{pct}% owned")>
                    <span class:done=pct == 100 style=format!("width: {pct}%")></span>
                </div>
                <div class="deck-own">
                    <span>{format!("{} of {} owned · {pct}%", o.owned, o.total)}</span>
                    {if o.owned == o.total {
                        view! { <span class="chip primary">"Complete"</span> }.into_any()
                    } else {
                        view! {
                            <span class="tcg" title="TCGplayer price of the missing copies">
                                {format!(
                                    "To finish {}",
                                    sum(o.missing.tcgplayer, o.missing.tcgplayer_unpriced),
                                )}
                            </span>
                        }
                            .into_any()
                    }}
                </div>
            }
            .into_any();
        }
        match decks.failed.with(|f| f.get(&id).cloned()) {
            Some(e) => {
                view! { <div class="deck-own error-text">{format!("Couldn't load: {e}")}</div> }
                    .into_any()
            }
            None => view! { <div class="deck-own muted">"Checking cards…"</div> }.into_any(),
        }
    };
    view! {
        <button class="deck-tile" on:click=move |_| decks.open.set(Some(open_id.clone()))>
            <div class="deck-head">
                <span class="deck-name">{summary.name.clone()}</span>
                {pips(&summary.colors)}
            </div>
            <div class="muted deck-meta">
                {format!(
                    "{} · {size} cards · updated {}",
                    summary.format_label(),
                    date(&summary.updated),
                )}
            </div>
            {status}
        </button>
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Showing {
    All,
    Missing,
    Owned,
}

#[component]
fn DeckDetail(
    id: String,
    decks: DeckCache,
    owned: Memo<HashMap<String, DeckOwnership>>,
) -> impl IntoView {
    let showing = RwSignal::new(Showing::All);
    let id = StoredValue::new(id);
    let deck = move || {
        decks
            .details
            .with(|m| id.with_value(|id| m.get(id).cloned()))
    };
    let summary = move || {
        decks.list.with(|l| {
            l.as_ref()
                .and_then(|l| id.with_value(|id| l.decks.iter().find(|d| &d.id == id).cloned()))
        })
    };
    let ownership = move || owned.with(|m| id.with_value(|id| m.get(id).cloned()));
    let back = move |_| decks.open.set(None);

    let head = move || {
        summary().map(|s| {
            view! {
                <h1>{s.name.clone()}</h1>
                {pips(&s.colors)}
                <span class="location">
                    {format!("{} · updated {}", s.format_label(), date(&s.updated))}
                </span>
                <a class="btn text" href=s.url.clone() target="_blank" rel="noopener">
                    "Open on Moxfield"
                </a>
            }
        })
    };

    let body = move || {
        let Some(o) = ownership() else {
            let failed = decks
                .failed
                .with(|f| id.with_value(|id| f.get(id).cloned()));
            return match failed {
                Some(e) => view! {
                    <section class="card">
                        <div class="empty">
                            <p class="error-text">{format!("Couldn't load this deck: {e}")}</p>
                            <button
                                class="btn tonal"
                                on:click=move |_| id.with_value(|id| decks.retry(id))
                            >
                                "Try again"
                            </button>
                        </div>
                    </section>
                }
                .into_any(),
                None => view! {
                    <section class="card">
                        <div class="empty">
                            <div class="spinner inline"></div>
                            "Loading the deck and your collection…"
                        </div>
                    </section>
                }
                .into_any(),
            };
        };
        let warning = deck().and_then(|d| d.warning);
        view! {
            {warning.map(|w| view! { <div class="banner">{w}</div> })}
            <div class="metrics-row deck-metrics">
                <Metric label="Cards" value=o.total.to_string() kind="" />
                <Metric
                    label="Owned"
                    value=format!("{} ({}%)", o.owned, o.percent())
                    kind=""
                />
                <Metric
                    label="Deck · TCGplayer"
                    value=sum(o.value.tcgplayer, o.value.tcgplayer_unpriced)
                    kind="tcg"
                />
                <Metric
                    label="Deck · Card Kingdom"
                    value=sum(o.value.cardkingdom, o.value.cardkingdom_unpriced)
                    kind="ck"
                />
                <Metric
                    label="To finish · TCGplayer"
                    value=sum(o.missing.tcgplayer, o.missing.tcgplayer_unpriced)
                    kind="tcg"
                />
                <Metric
                    label="To finish · Card Kingdom"
                    value=sum(o.missing.cardkingdom, o.missing.cardkingdom_unpriced)
                    kind="ck"
                />
            </div>
            {unpriced_note(&o.value).map(|note| view! { <p class="muted note">{note}</p> })}
            <div class="filter-row deck-sort">
                <span class="filter-label">"Show"</span>
                <div class="segmented" role="group" aria-label="Show cards">
                    {[(Showing::All, "All"), (Showing::Missing, "Missing"), (Showing::Owned, "Owned")]
                        .into_iter()
                        .map(|(s, label)| {
                            view! {
                                <button
                                    class:selected=move || showing.get() == s
                                    on:click=move |_| showing.set(s)
                                >
                                    {label}
                                </button>
                            }
                        })
                        .collect_view()}
                </div>
            </div>
            <DeckTable ownership=o showing />
        }
        .into_any()
    };

    view! {
        <div class="collection-head">
            <button class="btn text back" on:click=back>"← All decks"</button>
            {head}
        </div>
        {body}
    }
}

#[component]
fn DeckTable(ownership: DeckOwnership, showing: RwSignal<Showing>) -> impl IntoView {
    let lines = StoredValue::new(ownership.cards);
    let rows = move || {
        let show = showing.get();
        lines.with_value(|cards| {
            let mut out: Vec<(Board, Vec<CardOwnership>)> = Vec::new();
            for c in cards {
                let keep = match show {
                    Showing::All => true,
                    Showing::Missing => matches!(c.have(), Have::Partial | Have::Missing),
                    Showing::Owned => matches!(c.have(), Have::Exact | Have::OtherPrinting),
                };
                if !keep {
                    continue;
                }
                match out.last_mut() {
                    Some((b, list)) if *b == c.card.board => list.push(c.clone()),
                    _ => out.push((c.card.board, vec![c.clone()])),
                }
            }
            out
        })
    };
    view! {
        <section class="card">
            <div class="table-wrap">
                <table>
                    <thead>
                        <tr>
                            <th class="num">"Qty"</th>
                            <th>"Name"</th>
                            <th>"Type"</th>
                            <th>"Set"</th>
                            <th>"#"</th>
                            <th>"Finish"</th>
                            <th>"In collection"</th>
                            <th class="num">"TCGplayer"</th>
                            <th class="num">"Card Kingdom"</th>
                            <th class="num">"To buy"</th>
                        </tr>
                    </thead>
                    {move || {
                        rows()
                            .into_iter()
                            .map(|(board, cards)| view! { <BoardSection board cards /> })
                            .collect_view()
                    }}
                </table>
            </div>
            <Show when=move || rows().is_empty()>
                <div class="empty">"Nothing to show."</div>
            </Show>
        </section>
    }
}

#[component]
fn BoardSection(board: Board, cards: Vec<CardOwnership>) -> impl IntoView {
    let copies: u32 = cards.iter().map(|c| c.card.quantity).sum();
    let have: u32 = cards.iter().map(|c| c.card.quantity - c.missing()).sum();
    let note = if board.counts() {
        format!("{} · {have} owned", count(copies as usize, "card"))
    } else {
        format!(
            "{} · not counted in the deck",
            count(copies as usize, "card")
        )
    };
    view! {
        <tbody>
            <tr class="group-head">
                <td colspan="10">
                    <span class="group-label">{board.label()}</span>
                    <span class="muted">{note}</span>
                </td>
            </tr>
            {cards.into_iter().map(|c| view! { <DeckRow line=c /> }).collect_view()}
        </tbody>
    }
}

#[component]
fn DeckRow(line: CardOwnership) -> impl IntoView {
    let c = &line.card;
    let (chip_class, chip_text, chip_title) = match line.have() {
        Have::Exact => (
            "chip primary",
            "Owned".to_string(),
            "This printing is in the collection",
        ),
        Have::OtherPrinting => (
            "chip warning",
            "Other printing".to_string(),
            "Owned, but not (only) this printing",
        ),
        Have::Partial => (
            "chip partial",
            format!("{} of {}", line.any, c.quantity),
            "Some copies are in the collection",
        ),
        Have::Missing => (
            "chip missing",
            "Missing".to_string(),
            "Not in the collection",
        ),
    };
    let to_buy = line.missing();
    view! {
        <tr class:counted=c.board.counts()>
            <td class="num">{c.quantity}</td>
            <td>
                <CardLink id=c.scryfall_id.clone() name=c.name.clone() href=c.scryfall_url() />
            </td>
            <td title=c.type_line.clone()>{c.main_type()}</td>
            <td title=c.set_name.clone()>{c.set_code.clone()}</td>
            <td>{c.number.clone()}</td>
            <td>{c.finish.label()}</td>
            <td>
                <span class=chip_class title=chip_title>{chip_text}</span>
            </td>
            <td class="num tcg">{usd(c.tcgplayer)}</td>
            <td class="num ck">{usd(c.cardkingdom)}</td>
            <td class="num">
                {if to_buy == 0 {
                    String::new()
                } else {
                    usd(c.tcgplayer.map(|p| p * to_buy as f64))
                }}
            </td>
        </tr>
    }
}

/// The Home page's Decks block: count, color identities, formats and the
/// decks cheapest to finish.
#[component]
pub fn DeckStatsBlock(
    decks: DeckCache,
    owned: Memo<HashMap<String, DeckOwnership>>,
    go_decks: Callback<()>,
) -> impl IntoView {
    move || {
        let Some(view) = decks.list.get() else {
            return view! { <p class="muted">"Loading decks…"</p> }.into_any();
        };
        if view.user.is_none() {
            return view! {
                <p class="muted">
                    "Set " <code>"MTG_SCAN_MOXFIELD_USER"</code>
                    " in .env to list your Moxfield decks here."
                </p>
            }
            .into_any();
        }
        let stats = DeckStats::of(&view.decks);
        let closest: Vec<(DeckSummary, DeckOwnership)> = owned.with(|o| {
            let mut v: Vec<_> = view
                .decks
                .iter()
                .filter_map(|d| o.get(&d.id).map(|x| (d.clone(), x.clone())))
                .filter(|(_, x)| x.owned < x.total)
                .collect();
            v.sort_by(|a, b| a.1.missing.tcgplayer.total_cmp(&b.1.missing.tcgplayer));
            v.truncate(5);
            v
        });
        let complete = owned.with(|o| o.values().filter(|x| x.owned == x.total).count());
        let checked = owned.with(|o| o.len());
        let color_slices: Vec<(ColorGroup, Slice)> = stats
            .by_color
            .iter()
            .map(|(g, n)| {
                (
                    *g,
                    Slice {
                        label: g.label().to_string(),
                        cards: *n as u32,
                        value: 0.0,
                    },
                )
            })
            .collect();
        let formats: Vec<Slice> = stats
            .by_format
            .iter()
            .map(|(f, n)| Slice {
                label: f.clone(),
                cards: *n as u32,
                value: 0.0,
            })
            .collect();
        view! {
            <div class="metrics-row deck-home-metrics">
                <Metric label="Decks" value=stats.decks.to_string() kind="" />
                <div class="metric" title="Decks whose every card is in the collection (any printing)">
                    <div class="label">"Complete"</div>
                    <div class="value">
                        {if checked < stats.decks {
                            format!("{complete} ({checked} of {} checked)", stats.decks)
                        } else {
                            complete.to_string()
                        }}
                    </div>
                </div>
            </div>
            <div class="deck-stats">
                <div>
                    <h4>"Color identity"</h4>
                    <crate::home::ColorBreakdown
                        slices=color_slices
                        total=stats.decks as u32
                        unit="decks"
                    />
                </div>
                <div>
                    <h4>"Formats"</h4>
                    <crate::home::Bars slices=formats class_of=|_| String::new() unit="decks" />
                </div>
                <div>
                    <h4>"Cheapest to finish"</h4>
                    {if closest.is_empty() {
                        view! { <p class="muted">"Checking deck cards…"</p> }.into_any()
                    } else {
                        closest
                            .into_iter()
                            .map(|(d, o)| {
                                let id = d.id.clone();
                                view! {
                                    <button
                                        class="mini deck-mini"
                                        on:click=move |_| {
                                            decks.open.set(Some(id.clone()));
                                            go_decks.run(());
                                        }
                                    >
                                        {pips(&d.colors)}
                                        <span class="mini-text">
                                            <span class="deck-mini-name">{d.name.clone()}</span>
                                            <span class="muted">
                                                {format!("{}% owned · {} missing", o.percent(), o.total - o.owned)}
                                            </span>
                                        </span>
                                        <span class="mini-right tcg">
                                            {sum(o.missing.tcgplayer, o.missing.tcgplayer_unpriced)}
                                        </span>
                                    </button>
                                }
                            })
                            .collect_view()
                            .into_any()
                    }}
                </div>
            </div>
        }
        .into_any()
    }
}
