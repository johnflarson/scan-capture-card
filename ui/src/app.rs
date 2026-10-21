//! Page layout and components.

use gloo_timers::future::TimeoutFuture;
use leptos::prelude::*;
use leptos::task::spawn_local;
use mtg_scan_shared::{
    CardOutcome, CardView, ClientCommand, CollectionRow, CollectionView, Finish, Mode, ServerEvent,
    StateView, usd,
};

use crate::api;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Live,
    Collection,
}

#[component]
pub fn App() -> impl IntoView {
    let state = RwSignal::new(None::<StateView>);
    let cards = RwSignal::new(Vec::<CardView>::new());
    let connected = RwSignal::new(false);
    // Bumped on every reconnect so the camera stream is reopened.
    let epoch = RwSignal::new(0u32);
    let tab = RwSignal::new(Tab::Live);
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
            ServerEvent::Card(card) => cards.update(|list| {
                if !list.iter().any(|c| c.seq == card.seq) {
                    list.push(card);
                }
            }),
            ServerEvent::Undone { seqs } => cards.update(|list| {
                for c in list.iter_mut().filter(|c| seqs.contains(&c.seq)) {
                    c.undone = true;
                }
            }),
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
        <header class="top-bar">
            <div class="brand">"MTG" <small>"scan"</small></div>
            <nav class="tabs">
                <button
                    class="tab"
                    class:active=move || tab.get() == Tab::Live
                    on:click=move |_| tab.set(Tab::Live)
                >
                    "Live"
                </button>
                <button
                    class="tab"
                    class:active=move || tab.get() == Tab::Collection
                    on:click=move |_| tab.set(Tab::Collection)
                >
                    "Collection"
                </button>
            </nav>
            <div class="bar-end">
                {move || {
                    state
                        .get()
                        .filter(|s| s.dry_run)
                        .map(|_| view! { <span class="chip warning">"Dry run: nothing is saved"</span> })
                }}
                <span title=move || state.get().map(|s| s.collection).unwrap_or_default()>
                    {move || state.get().map(|s| s.collection).unwrap_or_default()}
                </span>
                <span class="dot" class:on=move || connected.get()></span>
                {move || if connected.get() { "Connected" } else { "Offline" }}
            </div>
        </header>
        <main>
            {move || match tab.get() {
                Tab::Live => view! { <LivePage state cards send epoch /> }.into_any(),
                Tab::Collection => view! { <CollectionPage notify /> }.into_any(),
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
fn CollectionPage(notify: Callback<(String, bool)>) -> impl IntoView {
    let data = RwSignal::new(None::<CollectionView>);
    let loading = RwSignal::new(false);
    let filter = RwSignal::new(String::new());

    let load = move || {
        loading.set(true);
        spawn_local(async move {
            match api::collection().await {
                Ok(view) => data.set(Some(view)),
                Err(e) => notify.run((format!("Could not load the collection: {e}"), true)),
            }
            loading.set(false);
        });
    };
    load();

    let rows = move || {
        let needle = filter.get().to_lowercase();
        data.with(|d| {
            d.as_ref()
                .map(|d| {
                    d.rows
                        .iter()
                        .filter(|r| {
                            needle.is_empty()
                                || r.name.to_lowercase().contains(&needle)
                                || r.set_code.to_lowercase().contains(&needle)
                                || r.set_name.to_lowercase().contains(&needle)
                        })
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
    };

    view! {
        <div class="collection-head">
            <h1>"Collection"</h1>
            <span class="location">{move || data.get().map(|d| d.location).unwrap_or_default()}</span>
            <input
                class="search"
                type="search"
                placeholder="Filter by name or set"
                prop:value=move || filter.get()
                on:input=move |ev| filter.set(event_target_value(&ev))
            />
            <button class="btn tonal" disabled=move || loading.get() on:click=move |_| load()>
                {move || if loading.get() { "Loading…" } else { "Refresh" }}
            </button>
        </div>
        {move || {
            data.get()
                .map(|d| {
                    let t = d.totals;
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
        <section class="card">
            <div class="table-wrap">
                <table>
                    <thead>
                        <tr>
                            <th class="num">"Qty"</th>
                            <th>"Name"</th>
                            <th>"Set"</th>
                            <th>"#"</th>
                            <th>"Finish"</th>
                            <th class="num">"TCGplayer"</th>
                            <th class="num">"Card Kingdom"</th>
                            <th class="num">"CK buylist"</th>
                            <th>"Priced"</th>
                        </tr>
                    </thead>
                    <tbody>
                        <For
                            each=rows
                            key=|r| {
                                format!(
                                    "{}|{:?}|{}|{:?}|{:?}",
                                    r.scryfall_id,
                                    r.finish,
                                    r.quantity,
                                    r.tcgplayer,
                                    r.cardkingdom,
                                )
                            }
                            children=|row| view! { <Row row /> }
                        />
                    </tbody>
                </table>
            </div>
            <Show when=move || data.with(|d| d.as_ref().is_some_and(|d| d.rows.is_empty()))>
                <div class="empty">"No cards yet. Scan some on the Live tab."</div>
            </Show>
        </section>
    }
}

#[component]
fn Metric(label: &'static str, value: String, kind: &'static str) -> impl IntoView {
    view! {
        <div class="metric">
            <div class="label">{label}</div>
            <div class=format!("value {kind}")>{value}</div>
        </div>
    }
}

#[component]
fn Row(row: CollectionRow) -> impl IntoView {
    let r = row;
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
                <a href=r.scryfall_url.clone() target="_blank" rel="noopener">{r.name.clone()}</a>
            </td>
            <td title=r.set_name.clone()>{r.set_code.clone()}</td>
            <td>{r.number.clone()}</td>
            <td>{r.finish.label()}</td>
            {cell("tcg", r.tcgplayer, r.tcgplayer_url.clone())}
            {cell("ck", r.cardkingdom, r.cardkingdom_url.clone())}
            <td class="num">{usd(r.cardkingdom_buylist)}</td>
            <td>{r.priced.clone().unwrap_or_default()}</td>
        </tr>
    }
}
