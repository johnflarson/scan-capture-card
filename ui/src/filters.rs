//! Collection page filters: chips per column, group-by, and the type,
//! rarity and color cells.

use leptos::prelude::*;
use mtg_scan_shared::{
    CollectionRow, CollectionView, ColorGroup, Facet, RowFilter, Totals, UNKNOWN, usd,
};

/// The Collection page's search, picks and grouping. Lives in `App` so they
/// survive switching tabs.
#[derive(Clone, Copy)]
pub struct CollectionPrefs {
    pub filter: RwSignal<RowFilter>,
    pub group_by: RwSignal<Option<Facet>>,
}

impl CollectionPrefs {
    pub fn new() -> Self {
        Self {
            filter: RwSignal::new(RowFilter::default()),
            group_by: RwSignal::new(None),
        }
    }
}

/// One line of the table: a group header or a card.
#[derive(Clone)]
pub enum Item {
    Group {
        facet: Facet,
        label: String,
        totals: Totals,
    },
    Card(CollectionRow),
}

impl Item {
    pub fn key(&self) -> String {
        match self {
            Item::Group { label, totals, .. } => format!(
                "group|{label}|{}|{}|{}",
                totals.printings, totals.cards, totals.tcgplayer
            ),
            Item::Card(r) => format!(
                "{}|{:?}|{}|{:?}|{:?}",
                r.scryfall_id, r.finish, r.quantity, r.tcgplayer, r.cardkingdom
            ),
        }
    }
}

/// `rows` as table lines, split under group headers when grouping.
pub fn items(rows: Vec<CollectionRow>, group_by: Option<Facet>) -> Vec<Item> {
    match group_by {
        None => rows.into_iter().map(Item::Card).collect(),
        Some(facet) => facet
            .group(&rows)
            .into_iter()
            .flat_map(|(label, rows)| {
                let totals = Totals::of(&rows);
                std::iter::once(Item::Group {
                    facet,
                    label,
                    totals,
                })
                .chain(rows.into_iter().map(Item::Card))
            })
            .collect(),
    }
}

/// The small marker before a value: a mana-colored dot or a rarity diamond.
fn marker(facet: Facet, value: &str) -> AnyView {
    match facet {
        Facet::Color => match ColorGroup::ALL.into_iter().find(|g| g.label() == value) {
            Some(g) => view! { <span class=format!("swatch {}", g.class())></span> }.into_any(),
            None => ().into_any(),
        },
        Facet::Rarity => view! { <span class=rarity_class(value)></span> }.into_any(),
        Facet::Type => ().into_any(),
    }
}

/// "1 card", "3 cards".
fn count(n: usize, noun: &str) -> String {
    format!("{n} {noun}{}", if n == 1 { "" } else { "s" })
}

fn rarity_class(label: &str) -> String {
    format!("rarity-dot rarity-{}", label.to_lowercase())
}

#[component]
pub fn GroupHead(facet: Facet, label: String, totals: Totals) -> impl IntoView {
    view! {
        <tr class="group-head">
            <td colspan="13">
                {marker(facet, &label)}
                <span class="group-label">{label}</span>
                <span class="muted">
                    {format!(
                        "{} · {} · {}",
                        count(totals.cards as usize, "card"),
                        count(totals.printings, "printing"),
                        usd(Some(totals.tcgplayer)),
                    )}
                </span>
            </td>
        </tr>
    }
}

/// Main type, with the full type line on hover.
#[component]
pub fn TypeCell(row: CollectionRow) -> impl IntoView {
    view! { <td title=row.type_line.clone()>{row.main_type()}</td> }
}

#[component]
pub fn RarityCell(row: CollectionRow) -> impl IntoView {
    let label = row.rarity_label();
    view! {
        <td>
            <span class=rarity_class(&label)></span>
            {label.clone()}
        </td>
    }
}

/// One pip per color, then the color group; the letters on hover.
#[component]
pub fn ColorCell(row: CollectionRow) -> impl IntoView {
    let Some(group) = row.color_group() else {
        return view! { <td class="muted" title="Card details unavailable">{UNKNOWN}</td> }
            .into_any();
    };
    let title = if row.colors.is_empty() {
        group.label().to_string()
    } else {
        format!("{} ({})", group.label(), row.colors.join(""))
    };
    let pips: Vec<String> = if row.colors.is_empty() {
        vec!["c".into()]
    } else {
        row.colors.iter().map(|c| c.to_lowercase()).collect()
    };
    view! {
        <td title=title>
            <span class="pips">
                {pips
                    .into_iter()
                    .map(|c| view! { <span class=format!("swatch {c}")></span> })
                    .collect_view()}
            </span>
            {group.label()}
        </td>
    }
    .into_any()
}

/// Chips for each column's values (OR within a column, AND across columns),
/// group-by, and a summary of what is hidden.
#[component]
pub fn FilterBar(
    prefs: CollectionPrefs,
    data: RwSignal<Option<CollectionView>>,
    /// Printings and copies shown after filtering.
    shown: Signal<Totals>,
) -> impl IntoView {
    let filter = prefs.filter;
    let group_by = prefs.group_by;

    let chips = move |facet: Facet| {
        move || {
            // Values in the whole collection (not just the shown rows), so
            // picking one never hides the others; plus anything still picked.
            let mut values = data.with(|d| {
                d.as_ref()
                    .map(|d| facet.present(&d.rows))
                    .unwrap_or_default()
            });
            filter.with(|f| {
                for picked in f.picked.get(&facet).into_iter().flatten() {
                    if !values.iter().any(|(v, _)| v == picked) {
                        values.push((picked.clone(), 0));
                    }
                }
            });
            values
                .into_iter()
                .map(|(value, copies)| {
                    let v = value.clone();
                    let selected = Signal::derive(move || filter.with(|f| f.is_picked(facet, &v)));
                    let toggle = value.clone();
                    view! {
                        <button
                            class="chip filter"
                            class:selected=selected
                            aria-pressed=move || selected.get().to_string()
                            on:click=move |_| filter.update(|f| f.toggle(facet, &toggle))
                        >
                            {marker(facet, &value)}
                            {value.clone()}
                            <span class="count">{copies}</span>
                        </button>
                    }
                })
                .collect_view()
        }
    };

    let summary = move || {
        let all = data.with(|d| d.as_ref().map(|d| d.totals.printings).unwrap_or(0));
        let s = shown.get();
        filter.with(RowFilter::is_active).then(|| {
            format!(
                "Showing {} of {all} printings ({})",
                s.printings,
                count(s.cards as usize, "card")
            )
        })
    };

    view! {
        <section class="card filters">
            {Facet::ALL
                .into_iter()
                .map(|facet| {
                    view! {
                        <div class="filter-row">
                            <span class="filter-label">{facet.label()}</span>
                            {chips(facet)}
                        </div>
                    }
                })
                .collect_view()}
            <div class="filter-row">
                <span class="filter-label">"Group by"</span>
                <div class="segmented" role="group" aria-label="Group by">
                    <button
                        class:selected=move || group_by.get().is_none()
                        on:click=move |_| group_by.set(None)
                    >
                        "None"
                    </button>
                    {Facet::ALL
                        .into_iter()
                        .map(|facet| {
                            view! {
                                <button
                                    class:selected=move || group_by.get() == Some(facet)
                                    on:click=move |_| group_by.set(Some(facet))
                                >
                                    {facet.label()}
                                </button>
                            }
                        })
                        .collect_view()}
                </div>
                <span class="spacer"></span>
                <span class="filter-summary">{summary}</span>
                <Show when=move || filter.with(RowFilter::is_active)>
                    <button
                        class="btn text"
                        on:click=move |_| filter.set(RowFilter::default())
                    >
                        "Clear filters"
                    </button>
                </Show>
            </div>
        </section>
    }
}
