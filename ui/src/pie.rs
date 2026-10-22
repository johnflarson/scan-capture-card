//! Collection page pie chart: when grouping, each group's share of the shown
//! cards, in the filter box. The key names every group with its percentage,
//! so the colors are never the only way to tell them apart.

use leptos::prelude::*;
use mtg_scan_shared::{CARD_TYPES, CollectionRow, ColorGroup, Facet, PieSlice, percent, pie};

use crate::filters::count;

/// The class that colors a group's slice and key swatch: its mana color,
/// its rarity color, or (types) a fixed slot of the categorical palette in
/// `style.css`, so a type keeps its color whatever the filter leaves.
/// "Other" and "Unknown" are grey.
fn slice_class(facet: Facet, value: &str) -> String {
    match facet {
        Facet::Color => ColorGroup::ALL
            .into_iter()
            .find(|g| g.label() == value)
            .map_or("none", ColorGroup::class)
            .to_string(),
        Facet::Rarity => format!("rarity-{}", value.to_lowercase()),
        Facet::Type => match CARD_TYPES[..CARD_TYPES.len() - 1]
            .iter()
            .position(|t| *t == value)
        {
            Some(i) => format!("t{}", i + 1),
            None => "none".to_string(),
        },
    }
}

fn describe(s: &PieSlice) -> String {
    format!(
        "{}: {} · {}",
        s.label,
        count(s.cards as usize, "card"),
        percent(s.share)
    )
}

#[component]
pub fn GroupPie(
    facet: Facet,
    /// The rows that pass the search and the filters.
    rows: Signal<Vec<CollectionRow>>,
) -> impl IntoView {
    let slices = Memo::new(move |_| rows.with(|r| pie(&facet.present(r))));
    // The group under the pointer (slice or key); the others fade.
    let hovered = RwSignal::new(None::<String>);
    let dim =
        move |label: String| move || hovered.with(|h| h.as_ref().is_some_and(|h| *h != label));
    let hover = move |label: String| move |_| hovered.set(Some(label.clone()));
    let leave = move |_| hovered.set(None);

    move || {
        let slices = slices.get();
        (!slices.is_empty()).then(|| {
            let title = format!("Cards by {}", facet.label().to_lowercase());
            let total: u32 = slices.iter().map(|s| s.cards).sum();
            let all = slices.clone();
            // The hovered group in words, else how many cards the chart covers.
            let detail = move || {
                hovered
                    .with(|h| {
                        h.as_ref()
                            .and_then(|h| all.iter().find(|s| s.label == *h))
                            .map(describe)
                    })
                    .unwrap_or_else(|| count(total as usize, "card"))
            };
            view! {
                <figure class="group-pie">
                    <svg class="pie" viewBox="-50 -50 100 100" aria-hidden="true">
                        {slices
                            .iter()
                            .map(|s| {
                                view! {
                                    <path
                                        d=s.path.clone()
                                        class=slice_class(facet, &s.label)
                                        class:dim=dim(s.label.clone())
                                        on:pointerenter=hover(s.label.clone())
                                        on:pointerleave=leave
                                    />
                                }
                            })
                            .collect_view()}
                    </svg>
                    <figcaption>
                        <span class="pie-title">{title}</span>
                        <ul class="pie-key">
                            {slices
                                .iter()
                                .map(|s| {
                                    view! {
                                        <li
                                            title=describe(s)
                                            class:dim=dim(s.label.clone())
                                            on:pointerenter=hover(s.label.clone())
                                            on:pointerleave=leave
                                        >
                                            <i class=slice_class(facet, &s.label)></i>
                                            <span class="pie-label">{s.label.clone()}</span>
                                            <span class="pie-pct">{percent(s.share)}</span>
                                        </li>
                                    }
                                })
                                .collect_view()}
                        </ul>
                        <span class="pie-detail">{detail}</span>
                    </figcaption>
                </figure>
            }
        })
    }
}
