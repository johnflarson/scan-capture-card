//! "Add card" dialog on the Collection page: paste a Scryfall link, pick the
//! quantity, finish, condition and language, add.

use leptos::prelude::*;
use leptos::task::spawn_local;
use mtg_scan_shared::{AddCard, CollectionRow, Condition, EditResult, Finish, PrintingView};

use crate::api;
use crate::copies::CopiesFields;

#[component]
pub fn AddDialog(
    on_close: Callback<()>,
    /// Called with `None` (no row was edited) and the server's answer.
    on_saved: Callback<(Option<CollectionRow>, EditResult)>,
) -> impl IntoView {
    let link = RwSignal::new(String::new());
    let printing = RwSignal::new(None::<PrintingView>);
    let quantity = RwSignal::new(1u32);
    let finish = RwSignal::new(Finish::Nonfoil);
    let condition = RwSignal::new(Condition::Nm);
    // The printing's language until changed.
    let language = RwSignal::new("en".to_string());
    let looking_up = RwSignal::new(false);
    let saving = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);
    let input = NodeRef::<leptos::html::Input>::new();

    // Focus the link field when the dialog opens.
    Effect::new(move |_| {
        if let Some(el) = input.get() {
            let _ = el.focus();
        }
    });

    let available = move |f: Finish| {
        printing
            .get()
            .map(|p| p.finishes.is_empty() || p.finishes.contains(&f))
            .unwrap_or(false)
    };

    let look_up = move || {
        let text = link.get_untracked().trim().to_string();
        error.set(None);
        if text.is_empty() {
            printing.set(None);
            return;
        }
        looking_up.set(true);
        spawn_local(async move {
            match api::printing(&text).await {
                Ok(p) => {
                    // Keep the chosen finish if this printing has it.
                    if !p.finishes.is_empty() && !p.finishes.contains(&finish.get_untracked()) {
                        let _ = finish.try_set(p.finishes[0]);
                    }
                    let _ = language.try_set(p.lang.clone());
                    let _ = printing.try_set(Some(p));
                }
                Err(e) => {
                    let _ = printing.try_set(None);
                    let _ = error.try_set(Some(e));
                }
            }
            let _ = looking_up.try_set(false);
        });
    };

    let submit = move || {
        let Some(p) = printing.get_untracked() else {
            return;
        };
        let add = AddCard {
            scryfall_id: p.scryfall_id,
            finish: finish.get_untracked(),
            quantity: quantity.get_untracked(),
            condition: condition.get_untracked(),
            language: Some(language.get_untracked()),
        };
        saving.set(true);
        error.set(None);
        spawn_local(async move {
            let result = api::add(&add).await;
            let _ = saving.try_set(false);
            match result {
                Ok(result) => on_saved.run((None, result)),
                Err(e) => {
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
                aria-labelledby="add-title"
                on:click=|ev| ev.stop_propagation()
            >
                <h2 id="add-title">"Add card"</h2>
                <div class="field">
                    <span class="control-label">"Scryfall link"</span>
                    <div class="link-row">
                        <input
                            node_ref=input
                            class="text-input"
                            type="url"
                            placeholder="https://scryfall.com/card/a25/141/lightning-bolt"
                            prop:value=move || link.get()
                            on:input=move |ev| link.set(event_target_value(&ev))
                            on:change=move |_| look_up()
                            on:keydown=move |ev| {
                                if ev.key() == "Enter" {
                                    look_up();
                                }
                            }
                        />
                        <button
                            class="btn tonal"
                            disabled=move || looking_up.get()
                            on:click=move |_| look_up()
                        >
                            {move || if looking_up.get() { "Looking…" } else { "Look up" }}
                        </button>
                    </div>
                    <span class="hint">"A card page link, a link with a language, or a Scryfall ID."</span>
                </div>

                {move || {
                    printing
                        .get()
                        .map(|p| {
                            view! {
                                <div class="dialog-card">
                                    {match p.image_url.clone() {
                                        Some(src) => view! { <img src=src alt="" /> }.into_any(),
                                        None => view! { <div class="noimg"></div> }.into_any(),
                                    }}
                                    <div>
                                        <a class="name" href=p.scryfall_url.clone() target="_blank" rel="noopener">
                                            {p.name.clone()}
                                        </a>
                                        <div class="set">
                                            {format!("{} · {} #{} · {}", p.set_name, p.set_code, p.number, p.rarity)}
                                        </div>
                                    </div>
                                </div>
                            }
                        })
                }}

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
                                            if printing.get().is_none() {
                                                "Look the card up first"
                                            } else if available(f) {
                                                ""
                                            } else {
                                                "Not printed in this finish"
                                            }
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
                <CopiesFields condition language />

                {move || error.get().map(|e| view! { <p class="error-text">{e}</p> })}

                <div class="dialog-actions">
                    <span class="spacer"></span>
                    <button class="btn text" on:click=move |_| on_close.run(())>
                        "Cancel"
                    </button>
                    <button
                        class="btn filled"
                        disabled=move || {
                            printing.get().is_none() || quantity.get() == 0 || saving.get()
                                || looking_up.get()
                        }
                        on:click=move |_| submit()
                    >
                        {move || if saving.get() { "Adding…" } else { "Add to collection" }}
                    </button>
                </div>
            </div>
        </div>
    }
}
