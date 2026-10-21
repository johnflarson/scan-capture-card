//! Page layout and components.

use gloo_timers::future::TimeoutFuture;
use leptos::prelude::*;
use leptos::task::spawn_local;
use mtg_scan_shared::{
    CardOutcome, CardView, ClientCommand, CollectionRow, CollectionView, EditResult, Finish, Mode,
    PrintingView, RowEdit, ServerEvent, StateView, Totals, usd,
};

use crate::add::AddDialog;
use crate::decks::{DeckCache, DecksPage};
use crate::filters::{
    CollectionPrefs, ColorCell, FilterBar, GroupHead, Item, RarityCell, TypeCell, items,
};
use crate::home::HomePage;
use crate::preview::{CardLink, Preview, PreviewLayer};

use crate::api;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Home,
    Scan,
    Collection,
    Decks,
}

impl Tab {
    const ALL: [Tab; 4] = [Tab::Home, Tab::Scan, Tab::Collection, Tab::Decks];

    fn label(self) -> &'static str {
        match self {
            Tab::Home => "Home",
            Tab::Scan => "Scan",
            Tab::Collection => "Collection",
            Tab::Decks => "Decks",
        }
    }
}

/// The collection as last loaded, shared by the Home and Collection pages.
/// Loaded while one of those pages is open and the data is stale: on first
/// use, after a scan adds or undoes cards, or after an edit that changes the
/// 7-day value change. Refresh reloads it on demand.
#[derive(Clone, Copy)]
pub struct CollectionCache {
    pub data: RwSignal<Option<CollectionView>>,
    pub loading: RwSignal<bool>,
    stale: RwSignal<bool>,
}

impl CollectionCache {
    fn new() -> Self {
        Self {
            data: RwSignal::new(None),
            loading: RwSignal::new(false),
            stale: RwSignal::new(true),
        }
    }

    pub fn load(self, notify: Callback<(String, bool)>) {
        if self.loading.get_untracked() {
            return;
        }
        self.loading.set(true);
        self.stale.set(false);
        spawn_local(async move {
            match api::collection().await {
                Ok(view) => self.data.set(Some(view)),
                Err(e) => notify.run((format!("Could not load the collection: {e}"), true)),
            }
            self.loading.set(false);
        });
    }

    /// Reload whenever the cache is stale while `showing` is true, including
    /// after a load that was already running when more changes arrived.
    fn keep_fresh(self, showing: impl Fn() -> bool + 'static, notify: Callback<(String, bool)>) {
        Effect::new(move |_| {
            if showing() && self.stale.get() && !self.loading.get() {
                self.load(notify);
            }
        });
    }

    pub fn mark_stale(self) {
        self.stale.set(true);
    }
}

#[component]
pub fn App() -> impl IntoView {
    let state = RwSignal::new(None::<StateView>);
    let cards = RwSignal::new(Vec::<CardView>::new());
    let connected = RwSignal::new(false);
    // Bumped on every reconnect so the camera stream is reopened.
    let epoch = RwSignal::new(0u32);
    let tab = RwSignal::new(Tab::Home);
    let cache = CollectionCache::new();
    let preview = Preview::new();
    provide_context(preview);
    let prefs = CollectionPrefs::new();
    let decks = DeckCache::new();
    let snack = RwSignal::new(None::<(String, bool, u64)>);
    let snack_seq = StoredValue::new(0u64);

    // Show a message at the bottom of the screen for a few seconds.
    let notify = Callback::new(move |(message, error): (String, bool)| {
        let id = snack_seq.get_value() + 1;
        snack_seq.set_value(id);
        snack.set(Some((message, error, id)));
        spawn_local(async move {
            TimeoutFuture::new(if error { 8_000 } else { 4_000 }).await;
            snack.update(|s| {
                if s.as_ref().is_some_and(|(_, _, shown)| *shown == id) {
                    *s = None;
                }
            });
        });
    });

    // Home and Collection show the collection, so keep it current while either
    // is open (a scan adding cards marks it stale).
    cache.keep_fresh(
        move || matches!(tab.get(), Tab::Home | Tab::Collection | Tab::Decks),
        notify,
    );

    let load_session = move || {
        spawn_local(async move {
            match api::session().await {
                Ok(s) => {
                    state.set(Some(s.state));
                    cards.set(s.cards);
                }
                Err(e) => notify.run((format!("Could not reach mtg-scan: {e}"), true)),
            }
        })
    };
    load_session();

    let send = Callback::new(move |cmd: ClientCommand| {
        spawn_local(async move {
            if let Err(e) = api::send(cmd).await {
                notify.run((format!("Command failed: {e}"), true));
            }
        })
    });

    let subscribed = api::subscribe(
        move |event| match event {
            ServerEvent::State(s) => state.set(Some(s)),
            ServerEvent::Card(card) => {
                if card.outcome == CardOutcome::Added {
                    cache.mark_stale();
                }
                cards.update(|list| {
                    if !list.iter().any(|c| c.seq == card.seq) {
                        list.push(card);
                    }
                })
            }
            ServerEvent::Undone { seqs } => {
                cache.mark_stale();
                cards.update(|list| {
                    for c in list.iter_mut().filter(|c| seqs.contains(&c.seq)) {
                        c.undone = true;
                    }
                })
            }
            ServerEvent::Info { message, error } => notify.run((message, error)),
        },
        move |ok| {
            if ok && !connected.get_untracked() {
                connected.set(true);
                epoch.update(|e| *e += 1);
                // Catch up on anything missed while disconnected.
                load_session();
            } else if !ok && connected.get_untracked() {
                connected.set(false);
            }
        },
    );
    if let Err(e) = subscribed {
        notify.run((format!("Live updates unavailable: {e}"), true));
    }

    view! {
        <PreviewLayer preview />
        <header class="top-bar">
            <button class="brand" title="Home" on:click=move |_| tab.set(Tab::Home)>
                "MTG" <small>"scan"</small>
            </button>
            <nav class="tabs">
                {Tab::ALL
                    .into_iter()
                    .map(|t| {
                        view! {
                            <button
                                class="tab"
                                class:active=move || tab.get() == t
                                on:click=move |_| tab.set(t)
                            >
                                {t.label()}
                            </button>
                        }
                    })
                    .collect_view()}
            </nav>
            <div class="bar-end">
                {move || {
                    state
                        .get()
                        .filter(|s| s.dry_run)
                        .map(|_| view! { <span class="chip warning">"Dry run: nothing is saved"</span> })
                }}
                <span
                    class="bar-location"
                    title=move || state.get().map(|s| s.collection).unwrap_or_default()
                >
                    {move || state.get().map(|s| s.collection).unwrap_or_default()}
                </span>
                <span class="dot" class:on=move || connected.get()></span>
                {move || if connected.get() { "Connected" } else { "Offline" }}
            </div>
        </header>
        <main>
            {move || match tab.get() {
                Tab::Home => {
                    let go = Callback::new(move |t: Tab| tab.set(t));
                    view! { <HomePage state cache decks notify go /> }.into_any()
                }
                Tab::Scan => view! { <LivePage state cards send epoch /> }.into_any(),
                Tab::Collection => {
                    view! { <CollectionPage state cache prefs notify /> }.into_any()
                }
                Tab::Decks => view! { <DecksPage decks cache notify /> }.into_any(),
            }}
        </main>
        {move || {
            snack
                .get()
                .map(|(message, error, _)| {
                    view! {
                        <div class="snackbar" class:error=error role="status">
                            {message}
                        </div>
                    }
                })
        }}
    }
}

#[component]
fn LivePage(
    state: RwSignal<Option<StateView>>,
    cards: RwSignal<Vec<CardView>>,
    send: Callback<ClientCommand>,
    epoch: RwSignal<u32>,
) -> impl IntoView {
    let s = move || state.get();
    let busy = move || s().map(|s| s.busy || !s.ready).unwrap_or(true);
    let mode = move || s().map(|s| s.mode).unwrap_or(Mode::Single);

    view! {
        <div class="live">
            <section class="card">
                <div class="card-header">
                    <h2>"Camera"</h2>
                    <span class="chip">{move || s().map(|s| s.camera).unwrap_or_default()}</span>
                </div>
                <div class="camera">
                    <img
                        src=move || format!("/api/stream.mjpg?c={}", epoch.get())
                        alt="Live camera preview"
                    />
                </div>
                <div class="status">
                    {move || busy().then(|| view! { <div class="spinner"></div> })}
                    <span class="status-text">
                        {move || s().map(|s| s.status).unwrap_or_else(|| "Connecting…".into())}
                    </span>
                </div>
                <div class="controls">
                    <div class="control-group">
                        <span class="control-label">"Mode"</span>
                        <div class="segmented">
                            <button
                                class:selected=move || mode() == Mode::Single
                                disabled=busy
                                on:click=move |_| send.run(ClientCommand::SetMode { mode: Mode::Single })
                            >
                                "Single card"
                            </button>
                            <button
                                class:selected=move || mode() == Mode::Table
                                disabled=busy
                                on:click=move |_| send.run(ClientCommand::SetMode { mode: Mode::Table })
                            >
                                "Table"
                            </button>
                        </div>
                    </div>
                    <div class="control-group">
                        <span class="control-label">"Finish"</span>
                        <div class="segmented">
                            {Finish::ALL
                                .into_iter()
                                .map(|f| {
                                    view! {
                                        <button
                                            class:selected=move || s().map(|s| s.finish) == Some(f)
                                            on:click=move |_| send.run(ClientCommand::SetFinish { finish: f })
                                        >
                                            {f.label()}
                                        </button>
                                    }
                                })
                                .collect_view()}
                        </div>
                    </div>
                    <label class="switch">
                        <input
                            type="checkbox"
                            prop:checked=move || s().map(|s| s.auto).unwrap_or(true)
                            on:change=move |ev| {
                                send.run(ClientCommand::SetAuto { auto: event_target_checked(&ev) })
                            }
                        />
                        "Auto-scan"
                    </label>
                    <div class="actions">
                        <button
                            class="btn filled"
                            disabled=busy
                            on:click=move |_| send.run(ClientCommand::ScanNow)
                        >
                            "Scan now"
                        </button>
                        <Show when=move || mode() == Mode::Single>
                            <button
                                class="btn outlined"
                                disabled=busy
                                title="Identify the card in the guide again with the vision model, replacing the last add"
                                on:click=move |_| send.run(ClientCommand::RetryLlm)
                            >
                                "Retry with LLM"
                            </button>
                        </Show>
                        <Show when=move || mode() == Mode::Table>
                            <button
                                class="btn outlined"
                                disabled=busy
                                title="Forget the last pass so every card on the table is added again"
                                on:click=move |_| send.run(ClientCommand::Reset)
                            >
                                "Rescan all"
                            </button>
                        </Show>
                        <button
                            class="btn text"
                            disabled=busy
                            title="Remove the cards added by the last scan"
                            on:click=move |_| send.run(ClientCommand::Undo)
                        >
                            "Undo last"
                        </button>
                    </div>
                </div>
            </section>
            <SessionPanel cards />
        </div>
    }
}

#[component]
fn SessionPanel(cards: RwSignal<Vec<CardView>>) -> impl IntoView {
    let kept = move || {
        cards.with(|list| {
            list.iter()
                .filter(|c| !c.undone)
                .cloned()
                .collect::<Vec<_>>()
        })
    };
    let total =
        move |price: fn(&CardView) -> Option<f64>| kept().iter().filter_map(price).sum::<f64>();

    view! {
        <section class="card">
            <div class="card-header">
                <h2>"This session"</h2>
            </div>
            <div class="summary">
                <div class="metric">
                    <div class="label">"Cards"</div>
                    <div class="value">{move || kept().len()}</div>
                </div>
                <div class="metric">
                    <div class="label">"TCGplayer"</div>
                    <div class="value tcg">{move || usd(Some(total(|c| c.tcgplayer)))}</div>
                </div>
                <div class="metric">
                    <div class="label">"Card Kingdom"</div>
                    <div class="value ck">{move || usd(Some(total(|c| c.cardkingdom)))}</div>
                </div>
            </div>
            <Show
                when=move || cards.with(|list| !list.is_empty())
                fallback=|| {
                    view! {
                        <div class="empty">
                            "Put a card in the guide (or lay cards out in table mode) and hold still. "
                            "Each card appears here as soon as it is saved."
                        </div>
                    }
                }
            >
                <ul class="card-list">
                    <For
                        each=move || {
                            let mut list = cards.get();
                            list.reverse();
                            list
                        }
                        key=|c| (c.seq, c.undone)
                        children=|card| view! { <CardTile card /> }
                    />
                </ul>
            </Show>
        </section>
    }
}

#[component]
fn CardTile(card: CardView) -> impl IntoView {
    let c = card;
    let foil = c.finish != Finish::Nonfoil;
    let review = c.needs_review;
    view! {
        <li class="tile" class:undone=c.undone>
            {match c.image_url.clone() {
                Some(src) => view! { <img src=src alt="" loading="lazy" /> }.into_any(),
                None => view! { <div class="noimg"></div> }.into_any(),
            }}
            <div>
                <div class="tile-head">
                    <a class="tile-name" href=c.scryfall_url.clone() target="_blank" rel="noopener">
                        {c.name.clone()}
                    </a>
                    <span class="tile-time">{c.time.clone()}</span>
                </div>
                <div class="tile-set">
                    {format!("{} · {} #{} · {}", c.set_name, c.set_code, c.number, c.rarity)}
                </div>
                <div class="tile-chips">
                    <span class="chip" class:foil=foil>{c.finish.label()}</span>
                    {c.quantity.map(|q| view! { <span class="chip">{format!("{q} owned")}</span> })}
                    {(c.outcome == CardOutcome::Priced)
                        .then(|| view! { <span class="chip">"not saved"</span> })}
                    <span
                        class="chip"
                        class:primary=!review
                        class:warning=review
                        title=c.note.clone().unwrap_or_default()
                    >
                        {format!("{} · {}", c.method, c.quality)}
                    </span>
                    {c.undone.then(|| view! { <span class="chip">"undone"</span> })}
                </div>
                <div class="prices">
                    <PriceLink kind="tcg" label="TCGplayer" price=c.tcgplayer url=c.tcgplayer_url.clone() />
                    <PriceLink kind="ck" label="Card Kingdom" price=c.cardkingdom url=c.cardkingdom_url.clone() />
                    <span class="muted">{format!("buylist {}", usd(c.cardkingdom_buylist))}</span>
                </div>
            </div>
        </li>
    }
}

#[component]
fn PriceLink(
    kind: &'static str,
    label: &'static str,
    price: Option<f64>,
    url: Option<String>,
) -> impl IntoView {
    let text = format!("{label} {}", usd(price));
    match url {
        Some(href) => {
            view! { <a class=kind href=href target="_blank" rel="noopener">{text}</a> }.into_any()
        }
        None => view! { <span class=kind>{text}</span> }.into_any(),
    }
}

#[component]
fn CollectionPage(
    state: RwSignal<Option<StateView>>,
    cache: CollectionCache,
    prefs: CollectionPrefs,
    notify: Callback<(String, bool)>,
) -> impl IntoView {
    let data = cache.data;
    let loading = cache.loading;
    let filter = prefs.filter;
    let group_by = prefs.group_by;
    let editing = RwSignal::new(None::<CollectionRow>);
    let adding = RwSignal::new(false);
    let dry_run = Signal::derive(move || state.get().is_some_and(|s| s.dry_run));

    let on_edit = Callback::new(move |row: CollectionRow| editing.set(Some(row)));
    let on_close = Callback::new(move |()| {
        editing.set(None);
        adding.set(false);
    });
    // Patch the table in place rather than reloading the whole collection.
    // `original` is the edited row (`None` when a card was added).
    let on_saved = Callback::new(
        move |(original, result): (Option<CollectionRow>, EditResult)| {
            data.update(|d| {
                let Some(d) = d else { return };
                if let Some(o) = original {
                    d.rows
                        .retain(|r| !(r.scryfall_id == o.scryfall_id && r.finish == o.finish));
                }
                if let Some(row) = result.row {
                    // Moved or added onto a row that already existed: it now holds both.
                    d.rows
                        .retain(|r| !(r.scryfall_id == row.scryfall_id && r.finish == row.finish));
                    d.rows.push(row);
                    d.rows.sort_by(|a, b| {
                        a.name
                            .to_lowercase()
                            .cmp(&b.name.to_lowercase())
                            .then_with(|| a.set_code.cmp(&b.set_code))
                            .then_with(|| a.number.cmp(&b.number))
                    });
                }
                d.totals = Totals::of(&d.rows);
                // The 7-day change depends on quantities and prices; only the server
                // can recompute it, so reload in the background.
                if d.value_change.is_some() {
                    cache.mark_stale();
                }
            });
            editing.set(None);
            adding.set(false);
            notify.run((result.message, false));
        },
    );

    let load = move || cache.load(notify);

    // The rows that pass the search and the picked chips.
    let shown = Memo::new(move |_| {
        filter.with(|f| {
            data.with(|d| {
                d.as_ref()
                    .map(|d| d.rows.iter().filter(|r| f.matches(r)).cloned().collect())
                    .unwrap_or_default()
            })
        })
    });
    let shown_totals =
        Signal::derive(move || shown.with(|rows: &Vec<CollectionRow>| Totals::of(rows)));
    let lines = move || {
        prefs
            .collapsed
            .with(|c| items(shown.get(), group_by.get(), c))
    };

    view! {
        <div class="collection-head">
            <h1>"Collection"</h1>
            <span class="location">{move || data.get().map(|d| d.location).unwrap_or_default()}</span>
            <input
                class="search"
                type="search"
                placeholder="Search name, set, type, rarity or color"
                prop:value=move || filter.with(|f| f.text.clone())
                on:input=move |ev| filter.update(|f| f.text = event_target_value(&ev))
            />
            <button class="btn tonal" disabled=move || loading.get() on:click=move |_| load()>
                {move || if loading.get() { "Loading…" } else { "Refresh" }}
            </button>
            <button
                class="btn filled add-btn"
                disabled=move || dry_run.get()
                title=move || {
                    if dry_run.get() {
                        "Dry run: the collection can't be changed"
                    } else {
                        "Add a card from a Scryfall link"
                    }
                }
                on:click=move |_| adding.set(true)
            >
                <PlusIcon />
                "Add card"
            </button>
        </div>
        {move || {
            data.with(|d| d.is_some())
                .then(|| {
                    // Totals follow the filter.
                    let t = shown_totals.get();
                    view! {
                        <div class="metrics-row">
                            <Metric label="Printings" value=t.printings.to_string() kind="" />
                            <Metric label="Cards" value=t.cards.to_string() kind="" />
                            <Metric label="TCGplayer" value=usd(Some(t.tcgplayer)) kind="tcg" />
                            <Metric label="Card Kingdom" value=usd(Some(t.cardkingdom)) kind="ck" />
                            <Metric label="CK buylist" value=usd(Some(t.cardkingdom_buylist)) kind="" />
                        </div>
                    }
                })
        }}
        <FilterBar prefs data shown=shown_totals />
        <section class="card">
            <div class="table-wrap">
                <table>
                    <thead>
                        <tr>
                            <th class="num">"Qty"</th>
                            <th>"Name"</th>
                            <th>"Type"</th>
                            <th>"Rarity"</th>
                            <th>"Color"</th>
                            <th>"Set"</th>
                            <th>"#"</th>
                            <th>"Finish"</th>
                            <th class="num">"TCGplayer"</th>
                            <th class="num">"Card Kingdom"</th>
                            <th class="num">"CK buylist"</th>
                            <th>"Priced"</th>
                            <th></th>
                        </tr>
                    </thead>
                    <tbody>
                        <For
                            each=lines
                            key=Item::key
                            children=move |item| match item {
                                Item::Group { facet, label, totals, collapsed } => {
                                    view! { <GroupHead facet label totals collapsed prefs /> }
                                        .into_any()
                                }
                                Item::Card(row) => {
                                    view! { <Row row on_edit locked=dry_run /> }.into_any()
                                }
                            }
                        />
                    </tbody>
                </table>
            </div>
            <Show when=move || data.with(|d| d.as_ref().is_some_and(|d| d.rows.is_empty()))>
                <div class="empty">"No cards yet. Scan some on the Scan tab."</div>
            </Show>
            <Show when=move || {
                shown.with(Vec::is_empty) && data.with(|d| d.as_ref().is_some_and(|d| !d.rows.is_empty()))
            }>
                <div class="empty">"No cards match the search and filters."</div>
            </Show>
        </section>
        {move || editing.get().map(|row| view! { <EditDialog row on_close on_saved /> })}
        <Show when=move || adding.get()>
            <AddDialog on_close on_saved />
        </Show>
    }
}

#[component]
pub fn Metric(label: &'static str, value: String, kind: &'static str) -> impl IntoView {
    view! {
        <div class="metric">
            <div class="label">{label}</div>
            <div class=format!("value {kind}")>{value}</div>
        </div>
    }
}

#[component]
fn Row(
    row: CollectionRow,
    on_edit: Callback<CollectionRow>,
    /// Dry run: the collection can't be changed.
    locked: Signal<bool>,
) -> impl IntoView {
    let r = row;
    let edit_row = r.clone();
    let cell = |kind: &'static str, price: Option<f64>, url: Option<String>| match url {
        Some(href) => view! {
            <td class=format!("num {kind}")>
                <a href=href target="_blank" rel="noopener">{usd(price)}</a>
            </td>
        }
        .into_any(),
        None => view! { <td class=format!("num {kind}")>{usd(price)}</td> }.into_any(),
    };
    view! {
        <tr>
            <td class="num">{r.quantity}</td>
            <td>
                <CardLink id=r.scryfall_id.clone() name=r.name.clone() href=r.scryfall_url.clone() />
            </td>
            <TypeCell row=r.clone() />
            <RarityCell row=r.clone() />
            <ColorCell row=r.clone() />
            <td title=r.set_name.clone()>{r.set_code.clone()}</td>
            <td>{r.number.clone()}</td>
            <td>{r.finish.label()}</td>
            {cell("tcg", r.tcgplayer, r.tcgplayer_url.clone())}
            {cell("ck", r.cardkingdom, r.cardkingdom_url.clone())}
            <td class="num">{usd(r.cardkingdom_buylist)}</td>
            <td>{r.priced.clone().unwrap_or_default()}</td>
            <td class="row-actions">
                <button
                    class="icon-btn"
                    disabled=move || locked.get()
                    title=move || {
                        if locked.get() {
                            "Dry run: editing is off"
                        } else {
                            "Edit quantity, finish or printing"
                        }
                    }
                    aria-label="Edit"
                    on:click=move |_| on_edit.run(edit_row.clone())
                >
                    <PencilIcon />
                </button>
            </td>
        </tr>
    }
}

#[component]
fn PlusIcon() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" aria-hidden="true">
            <path d="M19 13h-6v6h-2v-6H5v-2h6V5h2v6h6v2z" />
        </svg>
    }
}

#[component]
fn PencilIcon() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" aria-hidden="true">
            <path d="M3 17.25V21h3.75L17.81 9.94l-3.75-3.75L3 17.25zm17.71-10.21a1 1 0 0 0 0-1.41l-2.34-2.34a1 1 0 0 0-1.41 0l-1.83 1.83 3.75 3.75 1.83-1.83z" />
        </svg>
    }
}

/// Edit one collection row: quantity, finish, printing (from a pasted
/// Scryfall link), or remove it.
#[component]
fn EditDialog(
    row: CollectionRow,
    on_close: Callback<()>,
    on_saved: Callback<(Option<CollectionRow>, EditResult)>,
) -> impl IntoView {
    let original = StoredValue::new(row.clone());
    let quantity = RwSignal::new(row.quantity);
    let finish = RwSignal::new(row.finish);
    // The row's own printing (image, finishes), loaded when the dialog opens.
    let current = RwSignal::new(None::<PrintingView>);
    // A different printing picked by pasting a Scryfall link.
    let target = RwSignal::new(None::<PrintingView>);
    let link = RwSignal::new(String::new());
    let looking_up = RwSignal::new(false);
    let saving = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);
    let confirm_remove = RwSignal::new(false);

    {
        let id = row.scryfall_id.clone();
        spawn_local(async move {
            if let Ok(p) = api::printing(&id).await {
                // The dialog may have been closed meanwhile.
                let _ = current.try_set(Some(p));
            }
        });
    }

    let shown = move || target.get().or_else(|| current.get());
    let available = move |f: Finish| {
        shown()
            .map(|p| p.finishes.is_empty() || p.finishes.contains(&f))
            .unwrap_or(true)
    };
    let changed = move || {
        let o = original.get_value();
        quantity.get() != o.quantity || finish.get() != o.finish || target.get().is_some()
    };

    let look_up = move || {
        let text = link.get_untracked().trim().to_string();
        error.set(None);
        if text.is_empty() {
            target.set(None);
            return;
        }
        looking_up.set(true);
        spawn_local(async move {
            match api::printing(&text).await {
                Ok(p) => {
                    // Keep the finish if the new printing has it.
                    if let Some(&first) = p.finishes.first()
                        && !p.finishes.contains(&finish.get_untracked())
                    {
                        let _ = finish.try_set(first);
                    }
                    let same = p.scryfall_id == original.get_value().scryfall_id;
                    let _ = target.try_set((!same).then_some(p));
                }
                Err(e) => {
                    let _ = target.try_set(None);
                    let _ = error.try_set(Some(e));
                }
            }
            let _ = looking_up.try_set(false);
        });
    };

    let submit = move |copies: u32| {
        let o = original.get_value();
        let edit = RowEdit {
            to_scryfall_id: target
                .get_untracked()
                .map(|p| p.scryfall_id)
                .unwrap_or_else(|| o.scryfall_id.clone()),
            to_finish: finish.get_untracked(),
            scryfall_id: o.scryfall_id.clone(),
            finish: o.finish,
            quantity: copies,
        };
        saving.set(true);
        error.set(None);
        spawn_local(async move {
            match api::edit(&edit).await {
                Ok(result) => {
                    let _ = saving.try_set(false);
                    on_saved.run((Some(o), result));
                }
                Err(e) => {
                    let _ = saving.try_set(false);
                    let _ = error.try_set(Some(e));
                }
            }
        });
    };

    view! {
        <div
            class="scrim"
            on:click=move |_| on_close.run(())
            on:keydown=move |ev| {
                if ev.key() == "Escape" {
                    on_close.run(());
                }
            }
        >
            <div
                class="dialog"
                role="dialog"
                aria-modal="true"
                aria-labelledby="edit-title"
                on:click=|ev| ev.stop_propagation()
            >
                <h2 id="edit-title">"Edit card"</h2>
                <div class="dialog-card">
                    {move || match shown().and_then(|p| p.image_url) {
                        Some(src) => view! { <img src=src alt="" /> }.into_any(),
                        None => view! { <div class="noimg"></div> }.into_any(),
                    }}
                    <div>
                        {move || {
                            let o = original.get_value();
                            let (name, url, set_line) = match shown() {
                                Some(p) => (
                                    p.name,
                                    p.scryfall_url,
                                    format!("{} · {} #{} · {}", p.set_name, p.set_code, p.number, p.rarity),
                                ),
                                None => (
                                    o.name,
                                    o.scryfall_url,
                                    format!("{} · {} #{} · {}", o.set_name, o.set_code, o.number, o.rarity),
                                ),
                            };
                            view! {
                                <a class="name" href=url target="_blank" rel="noopener">{name}</a>
                                <div class="set">{set_line}</div>
                            }
                        }}
                        {move || {
                            target
                                .get()
                                .map(|_| {
                                    let o = original.get_value();
                                    view! {
                                        <span class="chip warning">
                                            {format!("New printing (was {} #{})", o.set_code, o.number)}
                                        </span>
                                    }
                                })
                        }}
                    </div>
                </div>

                <div class="field">
                    <span class="control-label">"Quantity"</span>
                    <div class="stepper">
                        <button
                            class="icon-btn"
                            aria-label="One fewer"
                            disabled=move || quantity.get() <= 1
                            on:click=move |_| quantity.update(|q| *q = q.saturating_sub(1).max(1))
                        >
                            "−"
                        </button>
                        <input
                            class="text-input"
                            type="number"
                            min="1"
                            prop:value=move || quantity.get().to_string()
                            on:input=move |ev| {
                                quantity.set(event_target_value(&ev).trim().parse().unwrap_or(0))
                            }
                        />
                        <button
                            class="icon-btn"
                            aria-label="One more"
                            on:click=move |_| quantity.update(|q| *q += 1)
                        >
                            "+"
                        </button>
                    </div>
                    <Show when=move || quantity.get() == 0>
                        <span class="hint">"To delete the row, use Remove."</span>
                    </Show>
                </div>

                <div class="field">
                    <span class="control-label">"Finish"</span>
                    <div class="segmented">
                        {Finish::ALL
                            .into_iter()
                            .map(|f| {
                                view! {
                                    <button
                                        class:selected=move || finish.get() == f
                                        disabled=move || !available(f)
                                        title=move || {
                                            if available(f) { "" } else { "Not printed in this finish" }
                                        }
                                        on:click=move |_| finish.set(f)
                                    >
                                        {f.label()}
                                    </button>
                                }
                            })
                            .collect_view()}
                    </div>
                </div>

                <div class="field">
                    <span class="control-label">"Printing"</span>
                    <div class="link-row">
                        <input
                            class="text-input"
                            type="url"
                            placeholder="Paste a Scryfall link to change the printing"
                            prop:value=move || link.get()
                            on:input=move |ev| link.set(event_target_value(&ev))
                            on:change=move |_| look_up()
                            on:keydown=move |ev| {
                                if ev.key() == "Enter" {
                                    look_up();
                                }
                            }
                        />
                        <Show
                            when=move || target.get().is_some()
                            fallback=move || {
                                view! {
                                    <button
                                        class="btn tonal"
                                        disabled=move || looking_up.get()
                                        on:click=move |_| look_up()
                                    >
                                        {move || if looking_up.get() { "Looking…" } else { "Look up" }}
                                    </button>
                                }
                            }
                        >
                            <button
                                class="btn text"
                                title="Keep the current printing"
                                on:click=move |_| {
                                    target.set(None);
                                    link.set(String::new());
                                    finish.set(original.get_value().finish);
                                }
                            >
                                "Keep current"
                            </button>
                        </Show>
                    </div>
                </div>

                {move || error.get().map(|e| view! { <p class="error-text" role="alert">{e}</p> })}

                <div class="dialog-actions">
                    <button
                        class="btn danger"
                        class:confirm=move || confirm_remove.get()
                        disabled=move || saving.get()
                        on:click=move |_| {
                            if confirm_remove.get_untracked() {
                                submit(0);
                            } else {
                                confirm_remove.set(true);
                            }
                        }
                    >
                        {move || {
                            if confirm_remove.get() {
                                let n = original.get_value().quantity;
                                format!("Remove {n} {}?", if n == 1 { "copy" } else { "copies" })
                            } else {
                                "Remove".to_string()
                            }
                        }}
                    </button>
                    <span class="spacer"></span>
                    <button class="btn text" on:click=move |_| on_close.run(())>
                        "Cancel"
                    </button>
                    <button
                        class="btn filled"
                        disabled=move || {
                            saving.get() || looking_up.get() || quantity.get() == 0 || !changed()
                        }
                        on:click=move |_| submit(quantity.get_untracked())
                    >
                        {move || if saving.get() { "Saving…" } else { "Save" }}
                    </button>
                </div>
            </div>
        </div>
    }
}
