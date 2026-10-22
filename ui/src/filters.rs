//! Collection page filters: chips per column, group-by, and the type,
//! rarity and color cells.

use std::collections::HashSet;

use leptos::prelude::*;
use mtg_scan_shared::{
    CollectionRow, CollectionView, ColorGroup, Facet, RowFilter, Totals, UNKNOWN, usd,
};

use crate::mana::{ManaSymbols, group_symbol};
use crate::pie::GroupPie;

/// How the Collection page shows the cards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Table,
    /// Card images with their names (see `gallery`).
    Cards,
}

/// The Collection page's view, search, picks, grouping and collapsed groups.
/// Lives in `App` so they survive switching tabs.
#[derive(Clone, Copy)]
pub struct CollectionPrefs {
    pub layout: RwSignal<Layout>,
    pub filter: RwSignal<RowFilter>,
    pub group_by: RwSignal<Option<Facet>>,
    /// Collapsed groups, per grouping column, so each grouping keeps its own.
    pub collapsed: RwSignal<HashSet<(Facet, String)>>,
}

impl CollectionPrefs {
    pub fn new() -> Self {
        Self {
            layout: RwSignal::new(Layout::Table),
            filter: RwSignal::new(RowFilter::default()),
            group_by: RwSignal::new(None),
            collapsed: RwSignal::new(HashSet::new()),
        }
    }

    pub fn toggle_group(self, facet: Facet, label: &str) {
        self.collapsed.update(|c| {
            let key = (facet, label.to_string());
            if !c.remove(&key) {
                c.insert(key);
            }
        });
    }
}

/// One line of the table: a group header or a card.
#[derive(Clone)]
pub enum Item {
    Group {
        facet: Facet,
        label: String,
        totals: Totals,
        collapsed: bool,
    },
    Card(CollectionRow),
}

impl Item {
    pub fn key(&self) -> String {
        match self {
            Item::Group {
                label,
                totals,
                collapsed,
                ..
            } => format!(
                "group|{label}|{}|{}|{}|{collapsed}",
                totals.printings, totals.cards, totals.tcgplayer
            ),
            Item::Card(r) => format!(
                "{}|{:?}|{}|{:?}|{:?}|{:?}",
                r.scryfall_id, r.finish, r.quantity, r.tcgplayer, r.cardmarket, r.cardkingdom
            ),
        }
    }
}

/// `rows` as table lines, split under group headers when grouping; a
/// collapsed group keeps its header (with its totals) but not its rows.
pub fn items(
    rows: Vec<CollectionRow>,
    group_by: Option<Facet>,
    collapsed: &HashSet<(Facet, String)>,
) -> Vec<Item> {
    match group_by {
        None => rows.into_iter().map(Item::Card).collect(),
        Some(facet) => facet
            .group(&rows)
            .into_iter()
            .flat_map(|(label, rows)| {
                let totals = Totals::of(&rows);
                let collapsed = collapsed.contains(&(facet, label.clone()));
                let cards = if collapsed { Vec::new() } else { rows };
                std::iter::once(Item::Group {
                    facet,
                    label,
                    totals,
                    collapsed,
                })
                .chain(cards.into_iter().map(Item::Card))
            })
            .collect(),
    }
}

/// How a value is shown: a color as its mana symbol alone (named on hover),
/// a rarity as a diamond and its name, a type (or Unknown) as its name.
fn facet_label(facet: Facet, value: &str) -> AnyView {
    match facet {
        Facet::Color => match ColorGroup::ALL.into_iter().find(|g| g.label() == value) {
            Some(g) => group_symbol(g),
            None => value.to_string().into_any(),
        },
        Facet::Rarity => view! {
            <span class=rarity_class(value)></span>
            {value.to_string()}
        }
        .into_any(),
        Facet::Type => value.to_string().into_any(),
    }
}

/// "1 card", "3 cards".
pub fn count(n: usize, noun: &str) -> String {
    format!("{n} {noun}{}", if n == 1 { "" } else { "s" })
}

fn rarity_class(label: &str) -> String {
    format!("rarity-dot rarity-{}", label.to_lowercase())
}

/// A group's header row; clicking it collapses or expands the group.
#[component]
pub fn GroupHead(
    facet: Facet,
    label: String,
    totals: Totals,
    collapsed: bool,
    prefs: CollectionPrefs,
) -> impl IntoView {
    view! {
        <tr class="group-head" class:collapsed=collapsed>
            // Spans every column, however many price columns are shown.
            <td colspan="99">
                <GroupToggle facet label totals collapsed prefs />
            </td>
        </tr>
    }
}

/// A group's name, card count, printings and value; a click collapses or
/// expands the group. Used by the table and the card grid.
#[component]
pub fn GroupToggle(
    facet: Facet,
    label: String,
    totals: Totals,
    collapsed: bool,
    prefs: CollectionPrefs,
) -> impl IntoView {
    let toggle = label.clone();
    view! {
        <button
            class="group-toggle"
            aria-expanded=(!collapsed).to_string()
            title=if collapsed { "Show this group" } else { "Hide this group" }
            on:click=move |_| prefs.toggle_group(facet, &toggle)
        >
            <span class="chevron" aria-hidden="true"></span>
            <span class="group-label">{facet_label(facet, &label)}</span>
            <span class="muted">
                {format!(
                    "{} · {} · {}",
                    count(totals.cards as usize, "card"),
                    count(totals.printings, "printing"),
                    usd(Some(totals.tcgplayer)),
                )}
            </span>
        </button>
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

/// One mana symbol per color (the colorless symbol when there are none).
#[component]
pub fn ColorCell(row: CollectionRow) -> impl IntoView {
    let Some(group) = row.color_group() else {
        return view! { <td class="muted" title="Card details unavailable">{UNKNOWN}</td> }
            .into_any();
    };
    view! {
        <td class="color-cell" aria-label=group.label()>
            <ManaSymbols colors=row.colors.clone() />
        </td>
    }
    .into_any()
}

/// Chips for each column's values (OR within a column, AND across columns),
/// group-by (with a pie of the groups' shares), and a summary of what is
/// hidden.
#[component]
pub fn FilterBar(
    prefs: CollectionPrefs,
    data: RwSignal<Option<CollectionView>>,
    /// Printings and copies shown after filtering.
    shown: Signal<Totals>,
    /// The rows shown after filtering.
    rows: Signal<Vec<CollectionRow>>,
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
                            {facet_label(facet, &value)}
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
            <div class="filter-rows">
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
                {move || {
                    group_by
                        .get()
                        .map(|facet| {
                            // Every value of this column in the collection, so
                            // groups hidden by a filter collapse too.
                            let all = move || {
                                data.with(|d| {
                                    d.as_ref()
                                        .map(|d| facet.present(&d.rows))
                                        .unwrap_or_default()
                                })
                            };
                            view! {
                                <button
                                    class="btn text"
                                    on:click=move |_| {
                                        prefs.collapsed.update(|c| c.retain(|(f, _)| *f != facet))
                                    }
                                >
                                    "Expand all"
                                </button>
                                <button
                                    class="btn text"
                                    on:click=move |_| {
                                        let labels = all();
                                        prefs
                                            .collapsed
                                            .update(|c| {
                                                c.extend(labels.into_iter().map(|(v, _)| (facet, v)))
                                            })
                                    }
                                >
                                    "Collapse all"
                                </button>
                            }
                        })
                }}
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
            </div>
            {move || group_by.get().map(|facet| view! { <GroupPie facet rows /> })}
        </section>
    }
}
