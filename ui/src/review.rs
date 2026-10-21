//! Scan page review list: cards whose printing is uncertain wait here until
//! the user accepts them, picks another printing, or discards them.

use leptos::prelude::*;
use leptos::task::spawn_local;
use mtg_scan_shared::{CardView, ClientCommand, Finish, PrintingView, usd};

use crate::api;

/// The held cards, newest first, each with its actions.
#[component]
pub fn ReviewList(held: Signal<Vec<CardView>>, send: Callback<ClientCommand>) -> impl IntoView {
    view! {
        <Show when=move || held.with(|h| !h.is_empty())>
            <div class="review">
                <div class="review-head">
                    <h3>{move || format!("To review ({})", held.with(Vec::len))}</h3>
                    <span class="muted">"Printing uncertain: not saved until you accept it."</span>
                </div>
                <ul class="card-list review-list">
                    <For
                        each=move || held.get()
                        key=|c| c.seq
                        children=move |card| view! { <ReviewTile card send /> }
                    />
                </ul>
            </div>
        </Show>
    }
}

#[component]
fn ReviewTile(card: CardView, send: Callback<ClientCommand>) -> impl IntoView {
    let seq = card.seq;
    // A click waits for the server to move the card out of the list.
    let pending = RwSignal::new(false);
    let picking = RwSignal::new(false);
    let accept = move |scryfall_id: String, finish: Finish| {
        pending.set(true);
        send.run(ClientCommand::Accept {
            seq,
            scryfall_id,
            finish,
        });
    };
    let id = card.scryfall_id.clone();
    let finish = card.finish;
    let c = card.clone();
    view! {
        <li class="tile review-tile">
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
                    <span class="chip" class:foil=c.finish != Finish::Nonfoil>{c.finish.label()}</span>
                    <span class="chip warning" title=c.note.clone().unwrap_or_default()>
                        {format!("{} · {}", c.method, c.quality)}
                    </span>
                    <span class="muted">{format!("TCGplayer {}", usd(c.tcgplayer))}</span>
                </div>
                <div class="review-actions">
                    <button
                        class="btn filled"
                        disabled=move || pending.get()
                        on:click=move |_| accept(id.clone(), finish)
                    >
                        "Accept"
                    </button>
                    <button
                        class="btn tonal"
                        disabled=move || pending.get()
                        aria-expanded=move || picking.get().to_string()
                        on:click=move |_| picking.update(|p| *p = !*p)
                    >
                        "Other printing"
                    </button>
                    <button
                        class="btn danger"
                        disabled=move || pending.get()
                        on:click=move |_| {
                            pending.set(true);
                            send.run(ClientCommand::Discard { seq });
                        }
                    >
                        "Discard"
                    </button>
                </div>
                <Show when=move || picking.get()>
                    <PrintingPicker card=card.clone() pending on_pick=Callback::new(move |(id, f)| accept(id, f)) />
                </Show>
            </div>
        </li>
    }
}

/// Every printing of the card as thumbnails; pick one and a finish.
#[component]
fn PrintingPicker(
    card: CardView,
    pending: RwSignal<bool>,
    on_pick: Callback<(String, Finish)>,
) -> impl IntoView {
    let printings = RwSignal::new(None::<Result<Vec<PrintingView>, String>>);
    let chosen = RwSignal::new(None::<PrintingView>);
    let finish = RwSignal::new(card.finish);
    let identified = card.scryfall_id.clone();
    {
        let name = card.name.clone();
        spawn_local(async move {
            let result = api::printings(&name).await;
            let _ = printings.try_set(Some(result));
        });
    }
    let pick = move |p: PrintingView| {
        if !p.finishes.is_empty() && !p.finishes.contains(&finish.get_untracked()) {
            finish.set(p.finishes[0]);
        }
        chosen.set(Some(p));
    };
    let available = move |f: Finish| {
        chosen
            .get()
            .is_some_and(|p| p.finishes.is_empty() || p.finishes.contains(&f))
    };

    view! {
        <div class="picker">
            {move || match printings.get() {
                None => view! { <p class="muted">"Loading printings…"</p> }.into_any(),
                Some(Err(e)) => view! { <p class="error-text">{e}</p> }.into_any(),
                Some(Ok(list)) => {
                    let identified = identified.clone();
                    view! {
                        <p class="muted">{format!("{} printings: pick the one you have.", list.len())}</p>
                        <div class="picker-grid">
                            {list
                                .into_iter()
                                .map(|p| {
                                    let id = p.scryfall_id.clone();
                                    let is_identified = id == identified;
                                    let thumb = format!(
                                        "https://api.scryfall.com/cards/{id}?format=image&version=small"
                                    );
                                    let label = format!("{} #{}", p.set_code, p.number);
                                    let title = format!("{} ({})", p.set_name, label);
                                    let selected = {
                                        let id = id.clone();
                                        move || chosen.with(|c| c.as_ref().is_some_and(|c| c.scryfall_id == id))
                                    };
                                    view! {
                                        <button
                                            class="picker-item"
                                            class:selected=selected
                                            title=title
                                            on:click=move |_| pick(p.clone())
                                        >
                                            <img src=thumb alt="" loading="lazy" />
                                            <span>{label}</span>
                                            {is_identified.then(|| view! { <span class="muted">"identified"</span> })}
                                        </button>
                                    }
                                })
                                .collect_view()}
                        </div>
                    }
                    .into_any()
                }
            }}
            <Show when=move || chosen.get().is_some()>
                <div class="picker-actions">
                    <div class="segmented">
                        {Finish::ALL
                            .into_iter()
                            .map(|f| {
                                view! {
                                    <button
                                        class:selected=move || finish.get() == f
                                        disabled=move || !available(f)
                                        on:click=move |_| finish.set(f)
                                    >
                                        {f.label()}
                                    </button>
                                }
                            })
                            .collect_view()}
                    </div>
                    <button
                        class="btn filled"
                        disabled=move || pending.get()
                        on:click=move |_| {
                            if let Some(p) = chosen.get_untracked() {
                                on_pick.run((p.scryfall_id, finish.get_untracked()));
                            }
                        }
                    >
                        {move || {
                            chosen
                                .get()
                                .map(|p| format!("Add {} #{}", p.set_code, p.number))
                                .unwrap_or_default()
                        }}
                    </button>
                </div>
            </Show>
        </div>
    }
}
