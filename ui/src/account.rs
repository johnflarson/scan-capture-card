//! The Settings page's Account box (password, API tokens, signing out) and,
//! for admins, the Users box (invite links, reset links, admin and disabled
//! flags).

use leptos::prelude::*;
use leptos::task::spawn_local;
use mtg_scan_shared::{ApiTokenView, LinkView, Me, PasswordChange, UserChange, UserView};

use crate::api;

type Notify = Callback<(String, bool)>;

/// An RFC 3339 time in the browser's time zone, e.g. "24/10/2026, 14:03".
pub fn local_time(rfc3339: &str) -> String {
    let date = js_sys::Date::new(&rfc3339.into());
    if date.get_time().is_nan() {
        return rfc3339.to_string();
    }
    let options = js_sys::Object::new();
    for (key, value) in [("dateStyle", "medium"), ("timeStyle", "short")] {
        let _ = js_sys::Reflect::set(&options, &key.into(), &value.into());
    }
    date.to_locale_string("default", &options).into()
}

/// This page's address with `path` after it: links work however the server
/// was reached (its own address, a reverse proxy's name).
fn full_link(link: &LinkView) -> String {
    let origin = web_sys::window()
        .and_then(|w| w.location().origin().ok())
        .unwrap_or_default();
    format!("{origin}{}", link.path)
}

/// A secret shown once, selected when clicked so it is easy to copy.
#[component]
fn Secret(value: String, note: String) -> impl IntoView {
    view! {
        <div class="secret">
            <input
                class="text-input"
                readonly
                prop:value=value
                on:focus=move |ev| {
                    use wasm_bindgen::JsCast;
                    if let Some(input) = ev
                        .target()
                        .and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok())
                    {
                        input.select();
                    }
                }
            />
            <p class="muted small">{note}</p>
        </div>
    }
}

#[component]
pub fn AccountCard(me: Me, notify: Notify) -> impl IntoView {
    let current = RwSignal::new(String::new());
    let new = RwSignal::new(String::new());
    let again = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let change = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        if new.get_untracked() != again.get_untracked() {
            notify.run(("The two new passwords don't match".into(), true));
            return;
        }
        busy.set(true);
        let body = PasswordChange {
            current: current.get_untracked(),
            new: new.get_untracked(),
        };
        spawn_local(async move {
            match api::change_password(&body).await {
                Ok(()) => {
                    for s in [current, new, again] {
                        s.set(String::new());
                    }
                    notify.run((
                        "Password changed. Other browsers were signed out.".into(),
                        false,
                    ));
                }
                Err(e) => notify.run((e, true)),
            }
            busy.set(false);
        });
    };
    let sign_out = move |_| {
        spawn_local(async move {
            match api::sign_out().await {
                Ok(()) => api::signed_out(),
                Err(e) => notify.run((e, true)),
            }
        })
    };
    let password = |id: &'static str, label: &'static str, value: RwSignal<String>, auto| {
        view! {
            <div class="field">
                <label class="control-label" for=id>{label}</label>
                <input
                    id=id
                    class="text-input"
                    type="password"
                    autocomplete=auto
                    required
                    prop:value=move || value.get()
                    on:input=move |ev| value.set(event_target_value(&ev))
                />
            </div>
        }
    };
    view! {
        <section class="card">
            <div class="card-header">
                <h2>"Account"</h2>
                {me.is_admin.then(|| view! { <span class="chip primary">"Admin"</span> })}
                <span class="spacer"></span>
                <button class="btn text" on:click=sign_out>"Sign out"</button>
            </div>
            <div class="card-body">
                <p class="muted small">"Signed in as " <b>{me.name.clone()}</b></p>
                <form on:submit=change>
                    <h3 class="info-title">"Change password"</h3>
                    {password("current-password", "Current", current, "current-password")}
                    {password("new-password", "New", new, "new-password")}
                    {password("new-password-again", "Again", again, "new-password")}
                    <button class="btn tonal" type="submit" disabled=move || busy.get()>
                        "Change password"
                    </button>
                </form>
                <ApiTokens notify />
            </div>
        </section>
    }
}

/// Tokens for scripts: `Authorization: Bearer tome_...`.
#[component]
fn ApiTokens(notify: Notify) -> impl IntoView {
    let tokens = RwSignal::new(None::<Result<Vec<ApiTokenView>, String>>);
    let created = RwSignal::new(None::<String>);
    let name = RwSignal::new(String::new());
    let load = move || spawn_local(async move { tokens.set(Some(api::api_tokens().await)) });
    load();
    let create = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let wanted = name.get_untracked();
        spawn_local(async move {
            match api::new_api_token(&wanted).await {
                Ok(new) => {
                    created.set(Some(new.secret));
                    name.set(String::new());
                    load();
                }
                Err(e) => notify.run((e, true)),
            }
        });
    };
    let revoke = move |id: i64| {
        spawn_local(async move {
            match api::delete_api_token(id).await {
                Ok(()) => {
                    notify.run(("Token revoked".into(), false));
                    load();
                }
                Err(e) => notify.run((e, true)),
            }
        })
    };
    let list = move || match tokens.get() {
        None => view! { <p class="muted small">"Loading…"</p> }.into_any(),
        Some(Err(e)) => view! { <p class="error-text">{e}</p> }.into_any(),
        Some(Ok(list)) if list.is_empty() => {
            view! { <p class="muted small">"No tokens yet."</p> }.into_any()
        }
        Some(Ok(list)) => view! {
            <ul class="people-list">
                {list
                    .into_iter()
                    .map(|t| {
                        let used = t
                            .last_used
                            .as_deref()
                            .map(|u| format!("last used {}", local_time(u)))
                            .unwrap_or_else(|| "never used".into());
                        view! {
                            <li class="person-row">
                                <span class="person-name">{t.name}</span>
                                <span class="muted small">
                                    {format!("made {}, {used}", local_time(&t.created))}
                                </span>
                                <button class="btn danger" on:click=move |_| revoke(t.id)>
                                    "Revoke"
                                </button>
                            </li>
                        }
                    })
                    .collect_view()}
            </ul>
        }
        .into_any(),
    };
    view! {
        <h3 class="info-title">"API tokens"</h3>
        <p class="muted small">
            "For scripts that use this server's API: send "
            <code>"Authorization: Bearer <token>"</code>
            ". A token acts as you until revoked."
        </p>
        {list}
        {move || created.get().map(|secret| view! {
            <Secret value=secret note="Copy it now: it isn't shown again.".to_string() />
        })}
        <form class="input-row" on:submit=create>
            <input
                class="text-input"
                placeholder="What it's for, e.g. price script"
                aria-label="Token name"
                required
                prop:value=move || name.get()
                on:input=move |ev| name.set(event_target_value(&ev))
            />
            <button class="btn tonal" type="submit">"New token"</button>
        </form>
    }
}

/// Admins: everyone's accounts, invite links and reset links.
#[component]
pub fn UsersCard(me: Me, notify: Notify) -> impl IntoView {
    let users = RwSignal::new(None::<Result<Vec<UserView>, String>>);
    // The newest link and what it is for.
    let link = RwSignal::new(None::<(String, String)>);
    let load = move || spawn_local(async move { users.set(Some(api::users().await)) });
    load();
    let invite = move |_| {
        spawn_local(async move {
            match api::invite().await {
                Ok(l) => link.set(Some((
                    full_link(&l),
                    format!(
                        "Send this invite link to one person. It works once, until {}.",
                        local_time(&l.expires)
                    ),
                ))),
                Err(e) => notify.run((e, true)),
            }
        })
    };
    let reset = move |user: UserView| {
        spawn_local(async move {
            match api::reset_link(user.id).await {
                Ok(l) => link.set(Some((
                    full_link(&l),
                    format!(
                        "Send this to {} to set a new password. It works once, until {}.",
                        user.name,
                        local_time(&l.expires)
                    ),
                ))),
                Err(e) => notify.run((e, true)),
            }
        })
    };
    let change = move |user: UserView, change: UserChange| {
        spawn_local(async move {
            match api::change_user(user.id, &change).await {
                Ok(_) => load(),
                Err(e) => notify.run((e, true)),
            }
        })
    };
    let me_id = me.id;
    let list = move || {
        match users.get() {
        None => view! { <p class="muted small">"Loading…"</p> }.into_any(),
        Some(Err(e)) => view! { <p class="error-text">{e}</p> }.into_any(),
        Some(Ok(list)) => view! {
            <ul class="people-list">
                {list
                    .into_iter()
                    .map(|u| {
                        let seen = u
                            .last_seen
                            .as_deref()
                            .map(|s| format!("seen {}", local_time(s)))
                            .unwrap_or_else(|| "never signed in".into());
                        let flags = UserChange {
                            is_admin: u.is_admin,
                            disabled: u.disabled,
                        };
                        let (a, b, c) = (u.clone(), u.clone(), u.clone());
                        view! {
                            <li class="person-row" class:hidden=u.disabled>
                                <span class="person-name">
                                    {u.name.clone()}
                                    {(u.id == me_id).then_some(" (you)")}
                                </span>
                                <span class="person-chips">
                                    {u.is_admin.then(|| view! { <span class="chip primary">"Admin"</span> })}
                                    {u.disabled.then(|| view! { <span class="chip">"Disabled"</span> })}
                                    {(!u.has_password).then(|| view! { <span class="chip warning">"No password"</span> })}
                                    <span class="muted small">{seen}</span>
                                </span>
                                <span class="person-actions">
                                    <button class="btn text" on:click=move |_| reset(a.clone())>
                                        "Reset link"
                                    </button>
                                    <button
                                        class="btn text"
                                        on:click=move |_| change(b.clone(), UserChange {
                                            is_admin: !flags.is_admin,
                                            ..flags
                                        })
                                    >
                                        {if u.is_admin { "Remove admin" } else { "Make admin" }}
                                    </button>
                                    <button
                                        class="btn danger"
                                        on:click=move |_| change(c.clone(), UserChange {
                                            disabled: !flags.disabled,
                                            ..flags
                                        })
                                    >
                                        {if u.disabled { "Enable" } else { "Disable" }}
                                    </button>
                                </span>
                            </li>
                        }
                    })
                    .collect_view()}
            </ul>
        }
        .into_any(),
    }
    };
    view! {
        <section class="card">
            <div class="card-header">
                <h2>"Users"</h2>
                <span class="spacer"></span>
                <button class="btn tonal" on:click=invite>"Invite someone"</button>
            </div>
            <div class="card-body">
                <p class="muted small">
                    "People join with an invite link; there is no open sign-up. Disabling "
                    "someone signs them out and revokes their API tokens; their cards stay."
                </p>
                {move || link.get().map(|(url, note)| view! { <Secret value=url note /> })}
                {list}
            </div>
        </section>
    }
}
