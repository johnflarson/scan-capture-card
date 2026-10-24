//! What copies a collection row holds besides the printing and finish: their
//! condition and language. Fields for the Add and Edit dialogs, and the
//! short tags the Collection table shows.

use leptos::prelude::*;
use mtg_scan_shared::{CARD_LANGUAGES, Condition, language_name};

/// Condition and language selects.
#[component]
pub fn CopiesFields(condition: RwSignal<Condition>, language: RwSignal<String>) -> impl IntoView {
    // The row's language even when it isn't one of the usual ones.
    let languages = move || {
        let current = language.get();
        let mut list: Vec<(String, String)> = CARD_LANGUAGES
            .iter()
            .map(|(code, name)| (code.to_string(), name.to_string()))
            .collect();
        if !list.iter().any(|(code, _)| *code == current) {
            list.push((current.clone(), current));
        }
        list
    };
    view! {
        <div class="field">
            <label class="control-label" for="copies-condition">"Condition"</label>
            <select
                id="copies-condition"
                class="text-input"
                prop:value=move || condition.get().code()
                on:change=move |ev| {
                    if let Some(c) = Condition::from_code(&event_target_value(&ev)) {
                        condition.set(c);
                    }
                }
            >
                {Condition::ALL
                    .into_iter()
                    .map(|c| view! { <option value=c.code()>{c.label()}</option> })
                    .collect_view()}
            </select>
        </div>
        <div class="field">
            <label class="control-label" for="copies-language">"Language"</label>
            <select
                id="copies-language"
                class="text-input"
                prop:value=move || language.get()
                on:change=move |ev| language.set(event_target_value(&ev))
            >
                {move || {
                    languages()
                        .into_iter()
                        .map(|(code, name)| view! { <option value=code>{name}</option> })
                        .collect_view()
                }}
            </select>
        </div>
    }
}

/// `LP`, `JA`: the condition and language, when they aren't Near Mint and
/// English.
#[component]
pub fn CopyTags(condition: Condition, language: String) -> impl IntoView {
    view! {
        {(condition != Condition::Nm)
            .then(|| view! { <span class="copy-tag" title=condition.label()>{condition.short()}</span> })}
        {(language != "en")
            .then(|| {
                view! {
                    <span class="copy-tag" title=language_name(&language).to_string()>
                        {language.to_uppercase()}
                    </span>
                }
            })}
    }
}
