//! Collection page card view: each card's image and name in a grid, under
//! the same group headers as the table. Hovering a card pops up the decks
//! it is in (any printing counts); on touch screens a tap does, and the next
//! tap elsewhere closes it.

use std::collections::HashMap;

use gloo_timers::future::TimeoutFuture;
use leptos::ev::PointerEvent;
use leptos::prelude::*;
use leptos::task::spawn_local;
use mtg_scan_shared::{CollectionRow, DeckUse, Finish, card_image, decks_using};

use crate::decks::DeckCache;
use crate::filters::{CollectionPrefs, GroupToggle, Item, count};

/// Popup width (also in `style.css`), and the space kept from the card and
/// the window edges.
const WIDTH: f64 = 280.0;
const GAP: f64 = 12.0;
/// Hover pause before the popup shows, so sweeping over the grid doesn't
/// flicker.
const DELAY_MS: u32 = 150;
/// Decks listed before "and N more".
const MAX_DECKS: usize = 14;

/// The card whose decks are shown, and its image's place in the window
/// (left, top, right, bottom).
#[derive(Clone, PartialEq)]
struct Shown {
    name: String,
    id: String,
    rect: (f64, f64, f64, f64),
}

#[derive(Clone, Copy)]
struct Popup {
    shown: RwSignal<Option<Shown>>,
    /// Bumped on every show and hide, so a pending show for a card the
    /// pointer already left is dropped.
    seq: StoredValue<u64>,
}

impl Popup {
    fn show(self, card: Shown, delay: bool) {
        let seq = self.seq.get_value() + 1;
        self.seq.set_value(seq);
        if !delay {
            self.shown.set(Some(card));
            return;
        }
        spawn_local(async move {
            TimeoutFuture::new(DELAY_MS).await;
            if self.seq.try_get_value() == Some(seq) {
                let _ = self.shown.try_set(Some(card));
            }
        });
    }

    fn hide(self) {
        self.seq.update_value(|s| *s += 1);
        if self.shown.with_untracked(Option::is_some) {
            self.shown.set(None);
        }
    }
}

fn window_size() -> (f64, f64) {
    let w = window().inner_width().ok().and_then(|w| w.as_f64());
    let h = window().inner_height().ok().and_then(|h| h.as_f64());
    w.zip(h).unwrap_or((1280.0, 800.0))
}

/// Beside the card (right, else left, else centred when the window is too
/// narrow), level with its top in the upper half of the window and with its
/// bottom in the lower half, so the popup grows away from the nearer edge.
fn place((left, top, right, bottom): (f64, f64, f64, f64), (w, h): (f64, f64)) -> String {
    let x = if right + GAP + WIDTH + GAP <= w {
        right + GAP
    } else if left - GAP - WIDTH >= GAP {
        left - GAP - WIDTH
    } else {
        ((w - WIDTH) / 2.0).max(GAP)
    };
    let y = if (top + bottom) / 2.0 < h / 2.0 {
        format!("top: {:.0}px", top.max(GAP))
    } else {
        format!("bottom: {:.0}px", (h - bottom).max(GAP))
    };
    format!("left: {x:.0}px; {y}")
}

/// The cards (and group headers) of `lines` as a grid, with the deck popup.
#[component]
pub fn CardGrid(
    lines: Signal<Vec<Item>>,
    prefs: CollectionPrefs,
    decks: DeckCache,
    /// Which decks use each card (`decks_by_card`).
    index: Memo<HashMap<String, Vec<DeckUse>>>,
) -> impl IntoView {
    let popup = Popup {
        shown: RwSignal::new(None),
        seq: StoredValue::new(0),
    };
    // A tap elsewhere closes a popup opened by a tap; scrolling moves the
    // card away from it.
    let taps = window_event_listener(leptos::ev::pointerdown, move |ev| {
        if ev.pointer_type() == "touch" {
            popup.hide();
        }
    });
    let scrolls = window_event_listener(leptos::ev::scroll, move |_| popup.hide());
    on_cleanup(move || {
        taps.remove();
        scrolls.remove();
    });

    view! {
        <div class="card-grid">
            <For
                each=move || lines.get()
                key=Item::key
                children=move |item| match item {
                    Item::Group { facet, label, totals, collapsed } => {
                        view! {
                            <div class="grid-group" class:collapsed=collapsed>
                                <GroupToggle facet label totals collapsed prefs />
                            </div>
                        }
                            .into_any()
                    }
                    Item::Card(row) => view! { <CardTile row popup /> }.into_any(),
                }
            />
        </div>
        {move || {
            popup
                .shown
                .get()
                .map(|card| {
                    let style = place(card.rect, window_size());
                    view! { <DeckPopup card decks index style /> }
                })
        }}
    }
}

#[component]
fn CardTile(row: CollectionRow, popup: Popup) -> impl IntoView {
    let art = NodeRef::<leptos::html::Div>::new();
    let (name, id) = (row.name.clone(), row.scryfall_id.clone());
    let shown = move || {
        let r = art.get_untracked()?.get_bounding_client_rect();
        Some(Shown {
            name: name.clone(),
            id: id.clone(),
            rect: (r.left(), r.top(), r.right(), r.bottom()),
        })
    };
    let open = shown.clone();
    let finish = (row.finish != Finish::Nonfoil).then(|| row.finish.label());
    view! {
        <div class="card-tile">
            <div
                class="tile-art"
                class:foil=finish.is_some()
                node_ref=art
                on:pointerenter=move |ev: PointerEvent| {
                    if ev.pointer_type() != "touch"
                        && let Some(card) = shown()
                    {
                        popup.show(card, true);
                    }
                }
                on:pointerleave=move |ev: PointerEvent| {
                    if ev.pointer_type() != "touch" {
                        popup.hide();
                    }
                }
                on:pointerup=move |ev: PointerEvent| {
                    if ev.pointer_type() == "touch"
                        && let Some(card) = open()
                    {
                        popup.show(card, false);
                    }
                }
            >
                <img src=card_image(&row.scryfall_id) alt=row.name.clone() loading="lazy" decoding="async" />
            </div>
            <a class="tile-name" href=row.scryfall_url.clone() target="_blank" rel="noopener" title=row.name.clone()>
                {row.name.clone()}
            </a>
            {finish.map(|f| view! { <span class="tile-finish">{f}</span> })}
        </div>
    }
}

/// The decks that use a card, or why they can't be shown yet.
#[component]
fn DeckPopup(
    card: Shown,
    decks: DeckCache,
    index: Memo<HashMap<String, Vec<DeckUse>>>,
    style: String,
) -> impl IntoView {
    let name = card.name.clone();
    let body = move || {
        let configured = decks.list.with(|l| l.as_ref().map(|l| l.user.is_some()));
        match configured {
            None if decks.loading.get() => {
                view! { <p class="muted">"Loading your decks…"</p> }.into_any()
            }
            None => view! { <p class="muted">"Decks aren't loaded."</p> }.into_any(),
            Some(false) => view! {
                <p class="muted">"Set a Moxfield account in Settings to see the decks that use this card."</p>
            }
            .into_any(),
            Some(true) => {
                let uses: Vec<DeckUse> = index.with(|i| decks_using(i, &card.name).to_vec());
                let (done, listed) = decks.progress();
                let checking = done < listed;
                let playing = uses.iter().filter(|u| !u.maybe_only()).count();
                let heading = if playing > 0 {
                    format!("In {}", count(playing, "deck"))
                } else if !uses.is_empty() {
                    "Only on a maybeboard".to_string()
                } else if checking {
                    "Not in the decks checked so far".to_string()
                } else {
                    "Not in any of your decks".to_string()
                };
                let more = uses.len().saturating_sub(MAX_DECKS);
                let id = card.id.clone();
                view! {
                    <p class="deck-pop-count">{heading}</p>
                    <ul class="deck-pop-list">
                        {uses
                            .into_iter()
                            .take(MAX_DECKS)
                            .map(|u| {
                                let exact = u.printings.contains(&id);
                                view! {
                                    <li class:maybe=u.maybe_only()>
                                        <span class="deck-pop-deck">{u.name.clone()}</span>
                                        <span class="deck-pop-detail">{u.detail()}</span>
                                        {exact
                                            .then(|| {
                                                view! {
                                                    <span class="deck-pop-exact" title="The deck lists this printing">
                                                        "this printing"
                                                    </span>
                                                }
                                            })}
                                    </li>
                                }
                            })
                            .collect_view()}
                    </ul>
                    {(more > 0).then(|| view! { <p class="muted">{format!("and {more} more")}</p> })}
                    {checking
                        .then(|| {
                            view! {
                                <p class="muted deck-pop-progress">
                                    {format!("Checking decks… {done} of {listed}")}
                                </p>
                            }
                        })}
                }
                .into_any()
            }
        }
    };
    view! {
        <div class="deck-pop" style=style role="tooltip">
            <p class="deck-pop-name">{name}</p>
            {body}
        </div>
    }
}
