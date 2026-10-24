//! Settings page (the cog in the top bar): how `mtg-scan ui` starts, prices
//! and the card language, the Moxfield account, which decks are hidden, and
//! read-only database and connection details. Changes are saved in the settings file
//! (`mtg-scan.toml`); passwords stay in `.env` and are never shown.

use leptos::prelude::*;
use leptos::task::spawn_local;
use mtg_scan_shared::{
    CARD_LANGUAGES, Currency, DatabaseView, DecksView, Finish, InfoRow, Me, Mode, PriceSettings,
    SettingsChange, SettingsView, Startup,
};

use crate::account::{AccountCard, UsersCard};
use crate::api;
use crate::decks::DeckCache;
use crate::mana::ManaSymbols;

type Notify = Callback<(String, bool)>;

#[component]
pub fn SettingsPage(decks: DeckCache, notify: Notify) -> impl IntoView {
    let settings = RwSignal::new(None::<SettingsView>);
    let database = RwSignal::new(None::<Result<DatabaseView, String>>);
    let all_decks = RwSignal::new(None::<Result<DecksView, String>>);
    let saving = RwSignal::new(false);
    let currency = use_context::<RwSignal<Currency>>();
    let me = use_context::<Me>().expect("signed in");

    let load_decks =
        move || spawn_local(async move { all_decks.set(Some(api::all_decks().await)) });
    let load_database = move || {
        database.set(None);
        spawn_local(async move { database.set(Some(api::database().await)) })
    };
    spawn_local(async move {
        match api::settings().await {
            Ok(view) => settings.set(Some(view)),
            Err(e) => notify.run((format!("Could not load the settings: {e}"), true)),
        }
    });
    load_database();
    load_decks();

    // Save one change; the page then shows the settings as saved, and the
    // Decks page and deck statistics pick up account and visibility changes.
    let save = Callback::new(move |change: SettingsChange| {
        saving.set(true);
        // Card Kingdom as saved before this change, to tell whether it changed.
        let card_kingdom_before =
            settings.with_untracked(|s| s.as_ref().map(|s| s.prices.card_kingdom));
        spawn_local(async move {
            match api::change_settings(&change).await {
                Ok(view) => {
                    settings.set(Some(view));
                    match change {
                        SettingsChange::Startup(_) => notify.run((
                            "Saved. It applies the next time mtg-scan ui starts.".into(),
                            false,
                        )),
                        SettingsChange::Moxfield { .. } => {
                            notify.run(("Moxfield account saved".into(), false));
                            all_decks.set(None);
                            load_decks();
                            decks.load(false, notify);
                        }
                        SettingsChange::DeckHidden { .. } => decks.load(false, notify),
                        SettingsChange::Prices(p) => {
                            let card_kingdom_changed = card_kingdom_before != Some(p.card_kingdom);
                            if let Some(c) = currency {
                                c.set(p.currency);
                            }
                            notify.run((
                                if card_kingdom_changed {
                                    "Saved. Card Kingdom prices start or stop the next time mtg-scan starts."
                                } else {
                                    "Saved"
                                }
                                .into(),
                                false,
                            ));
                        }
                        SettingsChange::CardLanguage { .. } => notify.run((
                            "Saved. Scans use it the next time mtg-scan starts.".into(),
                            false,
                        )),
                    }
                }
                Err(e) => notify.run((e, true)),
            }
            saving.set(false);
        });
    });

    view! {
        <div class="collection-head">
            <h1>"Settings"</h1>
            <span class="location">
                {move || settings.with(|s| s.as_ref().map(|s| format!("Saved in {}", s.file)))}
            </span>
        </div>
        {move || {
            settings
                .with(|s| s.as_ref().and_then(|s| s.locked.clone()))
                .map(|message| view! { <div class="banner">{message}</div> })
        }}
        <Show
            when=move || settings.with(Option::is_some)
            fallback=|| {
                view! {
                    <section class="card">
                        <div class="empty">
                            <div class="spinner inline"></div>
                            "Loading settings…"
                        </div>
                    </section>
                }
            }
        >
            <div class="settings-grid">
                <div class="settings-column">
                    <AccountCard me=me.clone() notify />
                    <StartupCard settings save saving />
                    <PricesCard settings save saving />
                    <MoxfieldCard settings save saving />
                    <DeckVisibility settings all_decks save saving />
                </div>
                <div class="settings-column">
                    {me.is_admin.then(|| view! { <UsersCard me=me.clone() notify /> })}
                    <DatabaseCard database reload=Callback::new(move |()| load_database()) />
                    <ConnectCard settings />
                </div>
            </div>
        </Show>
    }
}

/// The headline currency, the optional Card Kingdom prices and the
/// language of the cards being scanned.
#[component]
fn PricesCard(
    settings: RwSignal<Option<SettingsView>>,
    save: Callback<SettingsChange>,
    saving: RwSignal<bool>,
) -> impl IntoView {
    let prices = Memo::new(move |_| settings.with(|s| s.as_ref().map(|s| s.prices)));
    let language = move || settings.with(|s| s.as_ref().map(|s| s.card_language.clone()));
    let update = move |apply: &dyn Fn(&mut PriceSettings)| {
        if let Some(mut p) = prices.get_untracked() {
            apply(&mut p);
            save.run(SettingsChange::Prices(p));
        }
    };
    let currency = move || prices.get().map(|p| p.currency);
    view! {
        <section class="card">
            <div class="card-header">
                <h2>"Prices and cards"</h2>
            </div>
            <div class="card-body">
                <div class="field">
                    <span class="control-label">"Currency"</span>
                    <div class="segmented" role="group" aria-label="Currency">
                        {Currency::ALL
                            .into_iter()
                            .map(|c| {
                                view! {
                                    <button
                                        class:selected=move || currency() == Some(c)
                                        disabled=move || saving.get()
                                        on:click=move |_| update(&|p| p.currency = c)
                                    >
                                        {format!("{} ({})", c.code(), c.source())}
                                    </button>
                                }
                            })
                            .collect_view()}
                    </div>
                </div>
                <p class="small muted">
                    "Totals, group values and the most valuable cards use this store's prices. "
                    "Both columns are always shown. Prices come from Scryfall's daily data."
                </p>
                <div class="field">
                    <span class="control-label">"Card Kingdom"</span>
                    <label class="switch">
                        <input
                            type="checkbox"
                            prop:checked=move || prices.get().is_some_and(|p| p.card_kingdom)
                            disabled=move || saving.get()
                            on:change=move |ev| {
                                let on = event_target_checked(&ev);
                                update(&|p| p.card_kingdom = on);
                            }
                        />
                        "Also record Card Kingdom retail and buylist prices"
                    </label>
                </div>
                <div class="field">
                    <span class="control-label">"Card language"</span>
                    <select
                        class="text-input"
                        disabled=move || saving.get()
                        prop:value=move || language().unwrap_or_default()
                        on:change=move |ev| {
                            save.run(SettingsChange::CardLanguage {
                                code: event_target_value(&ev),
                            })
                        }
                    >
                        {CARD_LANGUAGES
                            .into_iter()
                            .map(|(code, name)| view! { <option value=code>{name}</option> })
                            .collect_view()}
                    </select>
                </div>
                <p class="small muted">
                    "The language your cards are printed in: scans record that printing when "
                    "Scryfall has it (prices are mostly for English printings)."
                </p>
            </div>
        </section>
    }
}

/// Auto-scan, mode and finish when `mtg-scan ui` starts.
#[component]
fn StartupCard(
    settings: RwSignal<Option<SettingsView>>,
    save: Callback<SettingsChange>,
    saving: RwSignal<bool>,
) -> impl IntoView {
    let startup = Memo::new(move |_| settings.with(|s| s.as_ref().map(|s| s.startup)));
    let update = move |apply: &dyn Fn(&mut Startup)| {
        if let Some(mut s) = startup.get_untracked() {
            apply(&mut s);
            save.run(SettingsChange::Startup(s));
        }
    };
    let is = move |f: fn(&Startup) -> bool| startup.get().as_ref().is_some_and(f);
    let mode = move || {
        if is(|s| s.table_mode) {
            Mode::Table
        } else {
            Mode::Single
        }
    };
    let overridden =
        move || settings.with(|s| s.as_ref().map(|s| s.overridden.clone()).unwrap_or_default());

    view! {
        <section class="card">
            <div class="card-header">
                <h2>"When mtg-scan starts"</h2>
            </div>
            <div class="card-body">
                <div class="field">
                    <span class="control-label">"Auto-scan"</span>
                    <label class="switch">
                        <input
                            type="checkbox"
                            prop:checked=move || is(|s| s.auto_scan)
                            disabled=saving
                            on:change=move |ev| {
                                let on = event_target_checked(&ev);
                                update(&|s| s.auto_scan = on)
                            }
                        />
                        {move || if is(|s| s.auto_scan) { "On" } else { "Off" }}
                    </label>
                </div>
                <div class="field">
                    <span class="control-label">"Mode"</span>
                    <div class="segmented">
                        <button
                            class:selected=move || mode() == Mode::Single
                            disabled=saving
                            on:click=move |_| update(&|s| s.table_mode = false)
                        >
                            "Single card"
                        </button>
                        <button
                            class:selected=move || mode() == Mode::Table
                            disabled=saving
                            on:click=move |_| update(&|s| s.table_mode = true)
                        >
                            "Table"
                        </button>
                    </div>
                </div>
                <div class="field">
                    <span class="control-label">"Finish"</span>
                    <div class="segmented">
                        {Finish::ALL
                            .into_iter()
                            .map(|f| {
                                view! {
                                    <button
                                        class:selected=move || {
                                            startup.get().is_some_and(|s| s.finish == f)
                                        }
                                        disabled=saving
                                        on:click=move |_| update(&|s| s.finish = f)
                                    >
                                        {f.label()}
                                    </button>
                                }
                            })
                            .collect_view()}
                    </div>
                </div>
                <p class="muted small">
                    "These apply the next time mtg-scan ui starts. The Scan page changes them for the running session; "
                    <code>"--manual"</code> ", " <code>"--table"</code> " and " <code>"--finish"</code>
                    " override them for one run."
                </p>
                {move || {
                    let notes = overridden();
                    (!notes.is_empty())
                        .then(|| {
                            view! {
                                <p class="small override">
                                    "This run: "
                                    {notes.join("; ")}
                                </p>
                            }
                        })
                }}
            </div>
        </section>
    }
}

/// The Moxfield account whose public decks are shown.
#[component]
fn MoxfieldCard(
    settings: RwSignal<Option<SettingsView>>,
    save: Callback<SettingsChange>,
    saving: RwSignal<bool>,
) -> impl IntoView {
    let saved = Memo::new(move |_| {
        settings.with(|s| s.as_ref().map(|s| s.moxfield.clone()).unwrap_or_default())
    });
    // A value from .env can be saved unchanged: that moves it to the settings file.
    let in_settings_file = Memo::new(move |_| {
        settings.with(|s| {
            s.as_ref().is_some_and(|s| {
                s.moxfield_source
                    .as_ref()
                    .is_none_or(|src| !src.ends_with(".env"))
            })
        })
    });
    let input = RwSignal::new(saved.get_untracked());
    // Show what was saved (e.g. trimmed) after each save.
    Effect::new(move |_| input.set(saved.get()));
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        save.run(SettingsChange::Moxfield {
            user: input.get_untracked(),
        });
    };
    let status = move || {
        settings.with(|s| {
            let s = s.as_ref()?;
            Some(match (&s.moxfield_user, &s.moxfield_source) {
                (Some(user), source) => {
                    let url = format!("https://moxfield.com/users/{user}");
                    view! {
                        <p class="muted small">
                            "Showing the public decks of "
                            <a href=url target="_blank" rel="noopener">{user.clone()}</a>
                            {source.clone().map(|src| format!(" (set in {src})"))}
                        </p>
                    }
                    .into_any()
                }
                (None, _) => view! {
                    <p class="muted small">
                        "No account: the Decks page asks for one. Paste your profile link, e.g. "
                        <code>"https://moxfield.com/users/YourName"</code>
                    </p>
                }
                .into_any(),
            })
        })
    };

    view! {
        <section class="card">
            <div class="card-header">
                <h2>"Moxfield"</h2>
            </div>
            <form class="card-body" on:submit=submit>
                <div class="field">
                    <label class="control-label" for="moxfield-user">"Profile"</label>
                    <div class="input-row">
                        <input
                            id="moxfield-user"
                            class="text-input"
                            placeholder="https://moxfield.com/users/YourName"
                            autocomplete="off"
                            prop:value=move || input.get()
                            on:input=move |ev| input.set(event_target_value(&ev))
                        />
                        <button
                            class="btn filled"
                            type="submit"
                            disabled=move || {
                                saving.get()
                                    || (in_settings_file.get()
                                        && input.get().trim() == saved.get().trim())
                            }
                        >
                            "Save"
                        </button>
                    </div>
                </div>
                {status}
            </form>
        </section>
    }
}

/// Every deck with a Show / Hide button. Hidden decks are left off the
/// Decks page and out of the deck statistics.
#[component]
fn DeckVisibility(
    settings: RwSignal<Option<SettingsView>>,
    all_decks: RwSignal<Option<Result<DecksView, String>>>,
    save: Callback<SettingsChange>,
    saving: RwSignal<bool>,
) -> impl IntoView {
    let hidden_ids = move || {
        settings.with(|s| {
            s.as_ref()
                .map(|s| s.hidden_decks.clone())
                .unwrap_or_default()
        })
    };
    let body = move || match all_decks.get() {
        None => view! {
            <div class="empty">
                <div class="spinner inline"></div>
                "Loading decks…"
            </div>
        }
        .into_any(),
        Some(Err(e)) => {
            view! { <p class="error-text">{format!("Could not load the decks: {e}")}</p> }
                .into_any()
        }
        Some(Ok(view)) if view.user.is_none() => view! {
            <p class="muted">"Set a Moxfield account above to choose which decks to show."</p>
        }
        .into_any(),
        Some(Ok(view)) if view.decks.is_empty() => {
            view! { <p class="muted">"This Moxfield account has no public decks."</p> }.into_any()
        }
        Some(Ok(view)) => {
            let total = view.decks.len();
            let ids: Vec<String> = view.decks.iter().map(|d| d.id.clone()).collect();
            let summary = move || {
                let hidden = hidden_ids();
                let shown = ids.iter().filter(|id| !hidden.contains(id)).count();
                format!("{shown} of {total} decks shown")
            };
            let mut list = view.decks;
            list.sort_by_key(|d| d.name.to_lowercase());
            let rows = list
                .into_iter()
                .map(|d| {
                    let id = d.id.clone();
                    let hidden = Signal::derive(move || hidden_ids().contains(&id));
                    let toggle_id = d.id.clone();
                    view! {
                        <li class="visibility-row" class:hidden=hidden>
                            <ManaSymbols colors=d.colors.clone() />
                            <span class="visibility-name" title=d.name.clone()>
                                {d.name.clone()}
                            </span>
                            <span class="muted small">{d.format_label()}</span>
                            <button
                                class="btn outlined small"
                                disabled=saving
                                aria-pressed=move || hidden.get().to_string()
                                on:click=move |_| {
                                    save.run(SettingsChange::DeckHidden {
                                        id: toggle_id.clone(),
                                        hidden: !hidden.get_untracked(),
                                    })
                                }
                            >
                                {move || if hidden.get() { "Show" } else { "Hide" }}
                            </button>
                        </li>
                    }
                })
                .collect_view();
            view! {
                <p class="muted small">
                    {summary}
                    ". Hidden decks are left off the Decks page and out of deck statistics."
                </p>
                <ul class="visibility-list">{rows}</ul>
            }
            .into_any()
        }
    };
    view! {
        <section class="card">
            <div class="card-header">
                <h2>"Decks"</h2>
            </div>
            <div class="card-body">{body}</div>
        </section>
    }
}

/// Where the collection is stored, read-only.
#[component]
fn DatabaseCard(
    database: RwSignal<Option<Result<DatabaseView, String>>>,
    reload: Callback<()>,
) -> impl IntoView {
    let body = move || match database.get() {
        None => view! {
            <div class="empty">
                <div class="spinner inline"></div>
                "Checking the database…"
            </div>
        }
        .into_any(),
        Some(Err(e)) => view! { <p class="error-text">{e}</p> }.into_any(),
        Some(Ok(db)) => {
            let postgres = db.backend == "PostgreSQL";
            view! {
                <InfoTable rows=db.settings />
                {db.error.map(|e| view! { <p class="error-text small">{e}</p> })}
                {(!db.status.is_empty())
                    .then(|| {
                        view! {
                            <h3 class="info-title">"Status"</h3>
                            <InfoTable rows=db.status />
                        }
                    })}
                <p class="muted small">
                    {if postgres {
                        "Connection settings and passwords live in the .env file next to mtg-scan.toml (MTG_SCAN_DB_*). Change them there and restart mtg-scan."
                    } else {
                        "The file is set by collection in mtg-scan.toml or --collection. Set [storage] backend = \"postgres\" to keep price history in PostgreSQL."
                    }}
                </p>
            }
            .into_any()
        }
    };
    let backend = move || {
        database.with(|d| match d {
            Some(Ok(db)) => db.backend.clone(),
            _ => String::new(),
        })
    };
    view! {
        <section class="card">
            <div class="card-header">
                <h2>"Database"</h2>
                <span class="chip">{backend}</span>
                <span class="spacer"></span>
                <button class="btn text" on:click=move |_| reload.run(())>
                    "Check again"
                </button>
            </div>
            <div class="card-body">{body}</div>
        </section>
    }
}

/// How to open the UI here, on other computers and on phones.
#[component]
fn ConnectCard(settings: RwSignal<Option<SettingsView>>) -> impl IntoView {
    let body = move || {
        let c = settings.with(|s| s.as_ref().map(|s| s.connection.clone()))?;
        let link = |url: String| {
            let href = url.clone();
            view! { <a href=href target="_blank" rel="noopener">{url}</a> }
        };
        Some(view! {
            <dl class="info">
                <dt>"This computer"</dt>
                <dd>{link(c.this_computer.clone())}</dd>
                <dt>"Other computers and phones"</dt>
                <dd>
                    {match c.network.clone() {
                        Some(url) => link(url).into_any(),
                        None if c.local_only => {
                            view! {
                                <span class="muted">
                                    "Off: mtg-scan ui only listens on this computer (--bind 127.0.0.1)"
                                </span>
                            }
                                .into_any()
                        }
                        None => view! { <span class="muted">"No network address found"</span> }.into_any(),
                    }}
                </dd>
                <dt>"Access"</dt>
                <dd>
                    {if c.local_only {
                        "This computer only; everyone signs in"
                    } else {
                        "Everyone signs in with their account; admins invite people under Users"
                    }}
                </dd>
            </dl>
            {c.local_only.then(|| view! {
                <p class="muted small">"Start without --bind 127.0.0.1 to use it from other computers."</p>
            })}
        })
    };
    view! {
        <section class="card">
            <div class="card-header">
                <h2>"Connect"</h2>
            </div>
            <div class="card-body">{body}</div>
        </section>
    }
}

#[component]
fn InfoTable(rows: Vec<InfoRow>) -> impl IntoView {
    view! {
        <dl class="info">
            {rows
                .into_iter()
                .map(|r| view! {
                    <dt>{r.label}</dt>
                    <dd>{r.value}</dd>
                })
                .collect_view()}
        </dl>
    }
}
