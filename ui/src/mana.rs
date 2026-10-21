//! Mana symbols for colors, as Scryfall's card-symbol SVGs (the same symbols
//! `https://api.scryfall.com/symbology` lists). Multicolor has no mana symbol,
//! so it is the gold coin MTG uses for multicolored cards.

use leptos::prelude::*;
use mtg_scan_shared::ColorGroup;

/// The symbol image for a color letter (`W`, `U`, `B`, `R`, `G`, `C`).
fn symbol_url(letter: &str) -> String {
    format!("https://svgs.scryfall.io/card-symbols/{letter}.svg")
}

fn color_name(letter: &str) -> &'static str {
    match letter {
        "W" => "White",
        "U" => "Blue",
        "B" => "Black",
        "R" => "Red",
        "G" => "Green",
        _ => "Colorless",
    }
}

/// One mana symbol, named on hover and for screen readers.
#[component]
pub fn Mana(letter: String) -> impl IntoView {
    let name = color_name(&letter);
    view! { <img class="mana" src=symbol_url(&letter) alt=name title=name /> }
}

/// The symbol for a color group: its mana symbol, or the gold coin for
/// Multicolor.
pub fn group_symbol(group: ColorGroup) -> AnyView {
    let letter = match group {
        ColorGroup::White => "W",
        ColorGroup::Blue => "U",
        ColorGroup::Black => "B",
        ColorGroup::Red => "R",
        ColorGroup::Green => "G",
        ColorGroup::Colorless => "C",
        ColorGroup::Multicolor => {
            return view! {
                <span class="mana multicolor" role="img" aria-label="Multicolor" title="Multicolor"></span>
            }
            .into_any();
        }
    };
    view! { <Mana letter=letter.to_string() /> }.into_any()
}

/// One symbol per color in WUBRG order; the colorless symbol when there are none.
#[component]
pub fn ManaSymbols(colors: Vec<String>) -> impl IntoView {
    let mut letters: Vec<String> = ["W", "U", "B", "R", "G"]
        .into_iter()
        .filter(|l| colors.iter().any(|c| c.eq_ignore_ascii_case(l)))
        .map(String::from)
        .collect();
    if letters.is_empty() {
        letters.push("C".into());
    }
    view! {
        <span class="mana-symbols">
            {letters.into_iter().map(|l| view! { <Mana letter=l /> }).collect_view()}
        </span>
    }
}
