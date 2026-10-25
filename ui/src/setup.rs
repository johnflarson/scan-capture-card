//! The setup wizard: what an admin sees after creating the server's first
//! account (and later from Settings). One step at a time: where the cards
//! are stored, how they are recognised (the printed text, a model on this
//! server, or a cloud model with a key; with a download and a one-card
//! test), the camera, prices and the card language, and usage statistics.

use gloo_timers::future::TimeoutFuture;
use leptos::prelude::*;
use leptos::task::spawn_local;
use mtg_scan_shared::{
    BenchmarkView, CARD_LANGUAGES, Currency, OllamaView, PullView, RecognitionChoice,
    RecognitionSetup, SettingsChange, SettingsView, SetupView, StateView, TelemetryView,
};

use crate::api;
use crate::app::CameraChoice;

type Notify = Callback<(String, bool)>;

const STEPS: [&str; 6] = [
    "Storage",
    "Reading cards",
    "Camera",
    "Prices and cards",
    "Usage statistics",
    "Done",
];

#[component]
pub fn SetupPage(
    state: RwSignal<Option<StateView>>,
    notify: Notify,
    /// Leave the wizard (to the Home page).
    finish: Callback<()>,
) -> impl IntoView {
    let setup = RwSignal::new(None::<SetupView>);
    let step = RwSignal::new(0usize);
    spawn_local(async move {
        match api::setup().await {
            Ok(view) => setup.set(Some(view)),
            Err(e) => notify.run((format!("Could not load the setup: {e}"), true)),
        }
    });
    let close = move |finished: bool| {
        spawn_local(async move {
            match api::setup_done(true).await {
                Ok(()) => {
                    if finished {
                        notify.run(("All set. Happy scanning!".into(), false));
                    }
                    finish.run(());
                }
                Err(e) => notify.run((e, true)),
            }
        })
    };
    let body = move || {
        let Some(view) = setup.get() else {
            return view! { <div class="empty"><div class="spinner inline"></div>"Loading…"</div> }
                .into_any();
        };
        match step.get() {
            0 => view! { <StorageStep view /> }.into_any(),
            1 => view! { <RecognitionStep view setup notify /> }.into_any(),
            2 => view! { <CameraStep state /> }.into_any(),
            3 => view! { <PricesStep notify /> }.into_any(),
            4 => view! { <TelemetryStep view setup notify /> }.into_any(),
            _ => view! { <DoneStep view /> }.into_any(),
        }
    };
    view! {
        <div class="collection-head">
            <h1>"Set up The Shuffling Tome"</h1>
            <span class="spacer"></span>
            <button class="btn text" on:click=move |_| close(false)>"Skip for now"</button>
        </div>
        <div class="wizard">
            <ol class="wizard-steps">
                {STEPS
                    .into_iter()
                    .enumerate()
                    .map(|(i, label)| {
                        view! {
                            <li
                                class:current=move || step.get() == i
                                class:past=move || { step.get() > i }
                            >
                                <button class="wizard-step" on:click=move |_| step.set(i)>
                                    {label}
                                </button>
                            </li>
                        }
                    })
                    .collect_view()}
            </ol>
            <section class="card wizard-body">
                <div class="card-body">{body}</div>
                <div class="dialog-actions wizard-nav">
                    <button
                        class="btn text"
                        disabled=move || step.get() == 0
                        on:click=move |_| step.update(|s| *s = s.saturating_sub(1))
                    >
                        "Back"
                    </button>
                    <span class="spacer"></span>
                    {move || {
                        if step.get() + 1 < STEPS.len() {
                            view! {
                                <button class="btn filled" on:click=move |_| step.update(|s| *s += 1)>
                                    "Next"
                                </button>
                            }
                                .into_any()
                        } else {
                            view! {
                                <button class="btn filled" on:click=move |_| close(true)>
                                    "Finish"
                                </button>
                            }
                                .into_any()
                        }
                    }}
                </div>
            </section>
        </div>
    }
}

#[component]
fn StorageStep(view: SetupView) -> impl IntoView {
    view! {
        <h2 class="wizard-title">"Where your cards are kept"</h2>
        <dl class="info">
            <dt>"Database"</dt>
            <dd>{view.storage.clone()}</dd>
            <dt>"Location"</dt>
            <dd>{view.storage_location.clone()}</dd>
        </dl>
        {if view.storage == "SQLite" {
            view! {
                <p class="muted">
                    "One file, nothing to install. It holds everyone's collections, the price history "
                    "and a copy of Scryfall's card data. Back it up with the rest of the data folder, "
                    "including secret.key."
                </p>
                <p class="muted small">
                    "Prefer your own PostgreSQL server? Set the MTG_SCAN_DB_* values in .env, run "
                    <code>"mtg-scan db migrate"</code> " and " <code>"mtg-scan db copy --to postgres"</code>
                    ", then restart with " <code>"[storage] backend = \"postgres\""</code> "."
                </p>
            }
                .into_any()
        } else {
            view! { <p class="muted">"Your own PostgreSQL server: the connection is set in .env."</p> }
                .into_any()
        }}
    }
}

#[component]
fn RecognitionStep(
    view: SetupView,
    setup: RwSignal<Option<SetupView>>,
    notify: Notify,
) -> impl IntoView {
    let r = view.recognition.clone();
    let choice = RwSignal::new(r.choice);
    let ollama_url = RwSignal::new(
        r.base_url
            .clone()
            .filter(|_| r.choice == RecognitionChoice::Local)
            .unwrap_or_else(|| "http://localhost:11434".into()),
    );
    let ollama = RwSignal::new(None::<OllamaView>);
    let pull = RwSignal::new(None::<PullView>);
    let provider = RwSignal::new(if r.choice == RecognitionChoice::Cloud {
        r.provider.clone()
    } else {
        "anthropic".into()
    });
    let model = RwSignal::new(if r.choice == RecognitionChoice::Cloud {
        r.model.clone()
    } else {
        String::new()
    });
    let key = RwSignal::new(String::new());
    let saving = RwSignal::new(false);
    let testing = RwSignal::new(false);
    let result = RwSignal::new(None::<BenchmarkView>);
    let providers = StoredValue::new(view.cloud_providers.clone());
    let local_model = view.local_model.clone();

    let check = move || {
        let url = ollama_url.get_untracked();
        spawn_local(async move {
            ollama.set(None);
            match api::setup_ollama(&url).await {
                Ok(v) => ollama.set(Some(v)),
                Err(e) => notify.run((e, true)),
            }
        })
    };
    if r.choice == RecognitionChoice::Local {
        check();
    }

    let save = move || {
        let body = RecognitionSetup {
            choice: choice.get_untracked(),
            provider: provider.get_untracked(),
            model: model.get_untracked(),
            base_url: ollama_url.get_untracked(),
        };
        let key_text = key.get_untracked();
        saving.set(true);
        result.set(None);
        spawn_local(async move {
            if body.choice == RecognitionChoice::Cloud && !key_text.trim().is_empty() {
                if let Err(e) =
                    api::change_settings(&SettingsChange::LlmKey { key: key_text }).await
                {
                    notify.run((e, true));
                    saving.set(false);
                    return;
                }
                key.set(String::new());
            }
            match api::setup_recognition(&body).await {
                Ok(v) => {
                    setup.set(Some(v));
                    notify.run(("Saved: the next scan uses it".into(), false));
                }
                Err(e) => notify.run((e, true)),
            }
            saving.set(false);
        });
    };

    let test = move || {
        testing.set(true);
        result.set(None);
        spawn_local(async move {
            match api::benchmark().await {
                Ok(v) => result.set(Some(v)),
                Err(e) => notify.run((e, true)),
            }
            testing.set(false);
        })
    };

    let download = {
        let local_model = local_model.clone();
        move || {
            let wanted = local_model.clone();
            spawn_local(async move {
                match api::start_pull(&wanted).await {
                    Ok(p) => pull.set(Some(p)),
                    Err(e) => {
                        notify.run((e, true));
                        return;
                    }
                }
                // Follow the download until it ends.
                loop {
                    TimeoutFuture::new(1_000).await;
                    match api::pull_progress().await {
                        Ok(p) => {
                            let running = p.running;
                            let _ = pull.try_set(Some(p));
                            if !running {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                check();
            })
        }
    };

    let current = move || {
        setup.with(|s| {
            s.as_ref().map(|s| {
                let r = &s.recognition;
                match r.choice {
                    RecognitionChoice::Ocr => "Printed text only".to_string(),
                    _ => format!("Printed text, then {} ({})", r.model, r.provider),
                }
            })
        })
    };
    let option = move |c: RecognitionChoice, title: &'static str, text: &'static str| {
        view! {
            <label class="option" class:selected=move || choice.get() == c>
                <input
                    type="radio"
                    name="recognition"
                    prop:checked=move || choice.get() == c
                    on:change=move |_| {
                        choice.set(c);
                        if c == RecognitionChoice::Local && ollama.get_untracked().is_none() {
                            check();
                        }
                    }
                />
                <span>
                    <b>{title}</b>
                    <span class="muted small">{text}</span>
                </span>
            </label>
        }
    };

    view! {
        <h2 class="wizard-title">"How cards are read"</h2>
        <p class="muted small">"Now: " {current}</p>
        <div class="options">
            {option(
                RecognitionChoice::Ocr,
                "The printed text only",
                "Works on any computer, offline. Reads the name well; some printings need a pick from a list.",
            )}
            {option(
                RecognitionChoice::Local,
                "A model on this server (Ollama)",
                "Reads what the text can't. Needs a graphics card with about 8 GB of memory; free and private.",
            )}
            {option(
                RecognitionChoice::Cloud,
                "A cloud model with your API key",
                "Reads what the text can't, on any computer. Each card costs a fraction of a cent on your key.",
            )}
        </div>

        <Show when=move || choice.get() == RecognitionChoice::Local>
            <div class="field">
                <label class="control-label" for="ollama-url">"Ollama"</label>
                <div class="input-row">
                    <input
                        id="ollama-url"
                        class="text-input"
                        prop:value=move || ollama_url.get()
                        on:input=move |ev| ollama_url.set(event_target_value(&ev))
                    />
                    <button class="btn tonal" on:click=move |_| check()>"Check"</button>
                </div>
            </div>
            {
                let download = download.clone();
                let local_model = local_model.clone();
                move || {
                    let download = download.clone();
                    let local_model = local_model.clone();
                    ollama
                        .get()
                        .map(|o| match (&o.version, &o.error) {
                            (_, Some(e)) => view! {
                                <p class="error-text">{e.clone()}</p>
                                <p class="muted small">
                                    "Install Ollama from "
                                    <a href="https://ollama.com/download" target="_blank" rel="noopener">"ollama.com"</a>
                                    " (in Docker: the ollama profile of compose.yaml), then check again."
                                </p>
                            }
                                .into_any(),
                            (Some(version), None) => view! {
                                <p class="muted small">
                                    {format!("Ollama {version} answers; it has {} model(s).", o.models.len())}
                                </p>
                                {if o.ready {
                                    view! { <p class="ok-text">{format!("✓ {local_model} is downloaded and reads images.")}</p> }
                                        .into_any()
                                } else {
                                    view! {
                                        <p class="muted small">
                                            {format!("The suggested model, {local_model}, is about 6 GB.")}
                                        </p>
                                        <button
                                            class="btn tonal"
                                            disabled=move || pull.get().is_some_and(|p| p.running)
                                            on:click=move |_| download()
                                        >
                                            {format!("Download {local_model}")}
                                        </button>
                                    }
                                        .into_any()
                                }}
                            }
                                .into_any(),
                            _ => view! { <p class="muted">"Checking…"</p> }.into_any(),
                        })
                }
            }
            {move || {
                pull.get()
                    .map(|p| {
                        let percent = p.percent.unwrap_or(0.0);
                        view! {
                            <div class="pull">
                                <div class="progress"><span style=format!("width: {percent:.0}%")></span></div>
                                <span class="muted small">
                                    {match (&p.error, p.percent) {
                                        (Some(e), _) => e.clone(),
                                        (None, Some(pct)) => format!("{}: {pct:.0}%", p.status),
                                        (None, None) => p.status.clone(),
                                    }}
                                </span>
                            </div>
                        }
                    })
            }}
        </Show>

        <Show when=move || choice.get() == RecognitionChoice::Cloud>
            <div class="field">
                <label class="control-label" for="cloud-provider">"Service"</label>
                <select
                    id="cloud-provider"
                    class="text-input"
                    prop:value=move || provider.get()
                    on:change=move |ev| {
                        provider.set(event_target_value(&ev));
                        model.set(String::new());
                    }
                >
                    {providers
                        .get_value()
                        .into_iter()
                        .map(|p| view! { <option value=p.id.clone()>{p.label.clone()}</option> })
                        .collect_view()}
                </select>
            </div>
            <div class="field">
                <label class="control-label" for="cloud-model">"Model"</label>
                <input
                    id="cloud-model"
                    class="text-input"
                    placeholder=move || {
                        providers
                            .get_value()
                            .into_iter()
                            .find(|p| p.id == provider.get())
                            .map(|p| p.default_model)
                            .unwrap_or_default()
                    }
                    prop:value=move || model.get()
                    on:input=move |ev| model.set(event_target_value(&ev))
                />
            </div>
            <div class="field">
                <label class="control-label" for="cloud-key">"Your key"</label>
                <input
                    id="cloud-key"
                    class="text-input"
                    type="password"
                    autocomplete="off"
                    placeholder=move || {
                        if setup.with(|s| s.as_ref().is_some_and(|s| s.recognition.own_key)) {
                            "Saved (leave empty to keep it)"
                        } else {
                            "Paste your API key"
                        }
                    }
                    prop:value=move || key.get()
                    on:input=move |ev| key.set(event_target_value(&ev))
                />
                <span class="hint">
                    {move || {
                        providers
                            .get_value()
                            .into_iter()
                            .find(|p| p.id == provider.get())
                            .map(|p| {
                                view! {
                                    "Get one at "
                                    <a href=p.key_url.clone() target="_blank" rel="noopener">{p.key_url.clone()}</a>
                                    ". It is stored encrypted and used for your scans; others add their own on Settings."
                                }
                            })
                    }}
                </span>
            </div>
        </Show>

        <div class="input-row wizard-actions">
            <button class="btn filled" disabled=move || saving.get() on:click=move |_| save()>
                {move || if saving.get() { "Saving…" } else { "Use this" }}
            </button>
            <Show when=move || choice.get() != RecognitionChoice::Ocr>
                <button
                    class="btn tonal"
                    disabled=move || testing.get()
                    title="Reads one card (Lightning Bolt) with the saved choice"
                    on:click=move |_| test()
                >
                    {move || if testing.get() { "Reading a card…" } else { "Try it" }}
                </button>
            </Show>
        </div>
        {move || {
            result
                .get()
                .map(|b| {
                    let read = b.read.clone().unwrap_or_else(|| "nothing".into());
                    view! {
                        <dl class="info benchmark">
                            <dt>"Read"</dt>
                            <dd>
                                {if b.correct { format!("✓ {read}") } else { format!("{read} (it was {})", b.expected) }}
                            </dd>
                            <dt>"Time"</dt>
                            <dd>{format!("{:.1} s", b.seconds)}</dd>
                            {b.gpu_share.map(|g| view! {
                                <dt>"On the GPU"</dt>
                                <dd>{format!("{:.0}%", g * 100.0)}</dd>
                            })}
                        </dl>
                        <p class=if b.error.is_some() { "error-text" } else { "muted" }>
                            {b.verdict.clone()}
                            {b.error.clone().map(|e| format!(" {e}"))}
                        </p>
                    }
                })
        }}
    }
}

#[component]
fn CameraStep(state: RwSignal<Option<StateView>>) -> impl IntoView {
    let camera = move || {
        state.with(|s| {
            s.as_ref()
                .map(|s| (s.camera.clone(), s.camera_error.clone()))
        })
    };
    view! {
        <h2 class="wizard-title">"The camera"</h2>
        <p class="muted">
            "Mount it above the table, looking straight down. The camera attached to this server "
            "is yours; anyone can also use the camera of the device their browser runs on."
        </p>
        {move || {
            camera()
                .map(|(name, error)| match error {
                    None => view! {
                        <p>"Camera: " <b>{name}</b></p>
                        <img class="wizard-preview" src="/api/stream.mjpg" alt="Camera preview" />
                    }
                        .into_any(),
                    Some(e) => view! { <p class="muted small">{e}</p> }.into_any(),
                })
        }}
        <CameraChoice state />
    }
}

#[component]
fn PricesStep(notify: Notify) -> impl IntoView {
    let settings = RwSignal::new(None::<SettingsView>);
    spawn_local(async move {
        if let Ok(v) = api::settings().await {
            settings.set(Some(v));
        }
    });
    let currency_ctx = use_context::<RwSignal<Currency>>();
    let save = move |change: SettingsChange| {
        spawn_local(async move {
            match api::change_settings(&change).await {
                Ok(v) => {
                    if let Some(c) = currency_ctx {
                        c.set(v.prices.currency);
                    }
                    settings.set(Some(v));
                }
                Err(e) => notify.run((e, true)),
            }
        })
    };
    view! {
        <h2 class="wizard-title">"Prices and cards"</h2>
        <div class="field">
            <span class="control-label">"Currency"</span>
            <div class="segmented">
                {Currency::ALL
                    .into_iter()
                    .map(|c| {
                        view! {
                            <button
                                class:selected=move || settings.with(|s| s.as_ref().is_some_and(|s| s.prices.currency == c))
                                on:click=move |_| {
                                    if let Some(mut p) = settings.with_untracked(|s| s.as_ref().map(|s| s.prices)) {
                                        p.currency = c;
                                        save(SettingsChange::Prices(p));
                                    }
                                }
                            >
                                {format!("{} ({})", c.code(), c.source())}
                            </button>
                        }
                    })
                    .collect_view()}
            </div>
        </div>
        <div class="field">
            <span class="control-label">"Card language"</span>
            <select
                class="text-input"
                prop:value=move || settings.with(|s| s.as_ref().map(|s| s.card_language.clone()).unwrap_or_default())
                on:change=move |ev| save(SettingsChange::CardLanguage { code: event_target_value(&ev) })
            >
                {CARD_LANGUAGES
                    .into_iter()
                    .map(|(code, name)| view! { <option value=code>{name}</option> })
                    .collect_view()}
            </select>
        </div>
        <p class="muted small">
            "These are yours: everyone you invite picks their own. Prices come from Scryfall's daily data."
        </p>
    }
}

#[component]
fn TelemetryStep(
    view: SetupView,
    setup: RwSignal<Option<SetupView>>,
    notify: Notify,
) -> impl IntoView {
    let _ = view;
    let changed = Callback::new(move |enabled: bool| {
        setup.update(|s| {
            if let Some(s) = s {
                s.telemetry = Some(enabled);
            }
        })
    });
    view! {
        <h2 class="wizard-title">"Usage statistics"</h2>
        <p>
            "May this server tell the author, anonymously, that it runs? That helps decide what to "
            "work on next."
        </p>
        <UsageStatistics notify changed />
    }
}

/// Usage statistics: what is and isn't sent, the choice, and exactly what
/// the next ping sends (the wizard and the Settings page).
#[component]
pub fn UsageStatistics(
    notify: Notify,
    /// Told the new choice once it is saved.
    #[prop(optional)]
    changed: Option<Callback<bool>>,
) -> impl IntoView {
    let telemetry = RwSignal::new(None::<Result<TelemetryView, String>>);
    let load = move || {
        spawn_local(async move {
            telemetry.set(Some(api::telemetry().await));
        })
    };
    load();
    let chosen = move || {
        telemetry.with(|t| {
            t.as_ref()
                .and_then(|t| t.as_ref().ok())
                .and_then(|t| t.enabled)
        })
    };
    let choose = move |enabled: bool| {
        spawn_local(async move {
            match api::setup_telemetry(enabled).await {
                Ok(()) => {
                    if let Some(changed) = changed {
                        changed.run(enabled);
                    }
                    load();
                }
                Err(e) => notify.run((e, true)),
            }
        })
    };
    let status = move || {
        telemetry.get().map(|t| match t {
            Ok(t) => {
                let status = match (t.do_not_track, t.enabled) {
                    (true, _) => {
                        "DO_NOT_TRACK is set on the server, so nothing is sent.".to_string()
                    }
                    (false, Some(true)) => format!(
                        "Sent once a day while the server runs, to {}. {}",
                        t.url,
                        match &t.last_sent {
                            Some(day) => format!("Last sent on {day}."),
                            None => "Nothing sent yet.".to_string(),
                        }
                    ),
                    (false, Some(false)) => "Off: nothing is sent.".to_string(),
                    (false, None) => "Not chosen yet: nothing is sent.".to_string(),
                };
                let next = serde_json::to_string_pretty(&t.next).unwrap_or_default();
                view! {
                    <p class="muted small">{status}</p>
                    <details class="ping">
                        <summary>"Exactly what the next ping sends"</summary>
                        <pre class="code">{next}</pre>
                    </details>
                }
                .into_any()
            }
            Err(e) => view! { <p class="muted small">{e}</p> }.into_any(),
        })
    };
    view! {
        <ul class="muted small">
            <li>"Sent, once a day: the version, the platform (Windows, Linux, Docker), the choices made in the setup, rough scan counts and kinds of errors, with a random id for this server."</li>
            <li>"Never sent: cards, collections, decks, images, names, addresses, messages or keys."</li>
        </ul>
        <div class="segmented">
            <button class:selected=move || chosen() == Some(true) on:click=move |_| choose(true)>
                "Yes, send them"
            </button>
            <button class:selected=move || chosen() == Some(false) on:click=move |_| choose(false)>
                "No"
            </button>
        </div>
        {status}
    }
}

#[component]
fn DoneStep(view: SetupView) -> impl IntoView {
    let _ = view;
    view! {
        <h2 class="wizard-title">"Ready"</h2>
        <ul class="muted">
            <li>"Scan: lay cards out under the camera; they are added as they're recognised."</li>
            <li>"Invite people under Settings → Users: each gets their own collection and scan session."</li>
            <li>"This setup is on the Settings page whenever you want it again."</li>
        </ul>
    }
}
