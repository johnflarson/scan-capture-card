//! Scan page "Top 5" box: this session's most expensive cards by TCGplayer
//! and by Card Kingdom price, as card images with that store's price. A card can be hidden (e.g. a
//! mis-scan); that only changes this box, never the collection.

use leptos::prelude::*;
use mtg_scan_shared::{CardView, top_cards, usd};

use crate::preview::Preview;

const COUNT: usize = 5;

#[component]
pub fn TopCards(
    cards: RwSignal<Vec<CardView>>,
    /// Keys ([`CardView::key`]) of the cards hidden from this box.
    hidden: RwSignal<Vec<String>>,
) -> impl IntoView {
    view! {
        <section class="card top-cards">
            <div class="card-header">
                <h2>"Top 5"</h2>
                <span class="spacer"></span>
                <Show when=move || hidden.with(|h| !h.is_empty())>
                    <button
                        class="btn text"
                        title="Show the cards hidden from this box again"
                        on:click=move |_| hidden.set(Vec::new())
                    >
                        {move || format!("Show hidden ({})", hidden.with(Vec::len))}
                    </button>
                </Show>
            </div>
            <TopList title="TCGplayer" kind="tcg" price=|c| c.tcgplayer cards hidden />
            <TopList title="Card Kingdom" kind="ck" price=|c| c.cardkingdom cards hidden />
        </section>
    }
}

#[component]
fn TopList(
    title: &'static str,
    kind: &'static str,
    price: fn(&CardView) -> Option<f64>,
    cards: RwSignal<Vec<CardView>>,
    hidden: RwSignal<Vec<String>>,
) -> impl IntoView {
    let top = move || cards.with(|list| hidden.with(|h| top_cards(list, price, h, COUNT)));
    view! {
        <div class="top-list">
            <h3 class=kind>{title}</h3>
            <Show
                when=move || !top().is_empty()
                fallback=|| view! { <p class="muted">"No priced cards yet."</p> }
            >
                <ol class="top-grid">
                    <For
                        each=top
                        key=|c| c.key()
                        children=move |c| {
                            let key = c.key();
                            let label = format!("{} · {}", c.name, usd(price(&c)));
                            // Hovering shows the card full size, as card names do.
                            let preview = use_context::<Preview>();
                            let id = c.scryfall_id.clone();
                            view! {
                                <li class="top-card">
                                    <a
                                        href=c.scryfall_url.clone()
                                        target="_blank"
                                        rel="noopener"
                                        title=label.clone()
                                        on:pointerenter=move |ev| {
                                            if let Some(p) = preview {
                                                p.enter(id.clone(), &ev);
                                            }
                                        }
                                        on:pointermove=move |ev| {
                                            if let Some(p) = preview {
                                                p.moved(&ev);
                                            }
                                        }
                                        on:pointerleave=move |_| {
                                            if let Some(p) = preview {
                                                p.leave();
                                            }
                                        }
                                    >
                                        {match c.image_url.clone() {
                                            Some(src) => view! { <img src=src alt=label.clone() loading="lazy" /> }.into_any(),
                                            None => view! { <div class="noimg">{c.name.clone()}</div> }.into_any(),
                                        }}
                                    </a>
                                    <span class=format!("top-price {kind}")>{usd(price(&c))}</span>
                                    <button
                                        class="top-hide"
                                        title="Hide from the top 5 (e.g. a mis-scan); the collection is unchanged"
                                        aria-label=format!("Hide {} from the top 5", c.name)
                                        on:click=move |_| hidden.update(|h| h.push(key.clone()))
                                    >
                                        "×"
                                    </button>
                                </li>
                            }
                        }
                    />
                </ol>
            </Show>
        </div>
    }
}
