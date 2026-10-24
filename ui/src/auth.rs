//! Signing in: the page shown until the browser has a session. It is the
//! sign-in form, the setup form that creates the admin account (`/setup`,
//! with the code the server printed), or an invite or password-reset link
//! (`/join`, `/reset`). Each ends by loading the app afresh.

use leptos::prelude::*;
use leptos::task::spawn_local;
use mtg_scan_shared::{LinkInfo, LinkKind, Redeem, SetUp, SignIn};

use crate::api;

const LOGO: &str = include_str!("../logo.svg");

/// What the address asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wanted {
    SignIn,
    Setup {
        code: String,
    },
    /// An invite or reset link.
    Link {
        code: String,
    },
}

/// The page the address asks for: `/setup?code=`, `/join?code=`,
/// `/reset?code=`, or the sign-in form.
pub fn wanted() -> Wanted {
    let Some(location) = web_sys::window().map(|w| w.location()) else {
        return Wanted::SignIn;
    };
    let path = location.pathname().unwrap_or_default();
    let code = query_value(&location.search().unwrap_or_default(), "code").unwrap_or_default();
    match path.trim_end_matches('/') {
        "/setup" => Wanted::Setup { code },
        "/join" | "/reset" if !code.is_empty() => Wanted::Link { code },
        _ => Wanted::SignIn,
    }
}

/// `name=` from a query string like `?code=abc&x=1`. Codes are URL-safe
/// base64, so they need no decoding.
fn query_value(search: &str, name: &str) -> Option<String> {
    search
        .trim_start_matches('?')
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value.to_string())
}

/// Open the app at its start page, signed in.
fn enter() {
    if let Some(window) = web_sys::window() {
        let _ = window.location().assign("/");
    }
}

/// The page shown without a session.
#[component]
pub fn AuthPage(setup_needed: bool) -> impl IntoView {
    let page = match wanted() {
        Wanted::Link { code } => view! { <LinkForm code /> }.into_any(),
        Wanted::Setup { code } if setup_needed => view! { <SetupForm code /> }.into_any(),
        Wanted::SignIn if setup_needed => view! { <SetupForm code=String::new() /> }.into_any(),
        _ => view! { <SignInForm /> }.into_any(),
    };
    view! {
        <div class="auth-screen">
            <div class="auth-card">
                <div class="auth-brand">
                    <span class="brand-logo" aria-hidden="true" inner_html=LOGO></span>
                    <span class="brand-name">"The Shuffling " <b>"Tome"</b></span>
                </div>
                {page}
            </div>
        </div>
    }
}

/// A labelled text or password input bound to `value`.
#[component]
fn Input(
    id: &'static str,
    label: &'static str,
    value: RwSignal<String>,
    #[prop(default = "text")] kind: &'static str,
    #[prop(default = "off")] autocomplete: &'static str,
    #[prop(default = false)] autofocus: bool,
) -> impl IntoView {
    view! {
        <label class="auth-label" for=id>{label}</label>
        <input
            id=id
            class="text-input"
            type=kind
            autocomplete=autocomplete
            autofocus=autofocus
            required
            prop:value=move || value.get()
            on:input=move |ev| value.set(event_target_value(&ev))
        />
    }
}

#[component]
fn SignInForm() -> impl IntoView {
    let name = RwSignal::new(String::new());
    let password = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        busy.set(true);
        error.set(None);
        let form = SignIn {
            name: name.get_untracked(),
            password: password.get_untracked(),
        };
        spawn_local(async move {
            match api::sign_in(&form).await {
                Ok(_) => enter(),
                Err(e) => {
                    error.set(Some(e));
                    busy.set(false);
                }
            }
        });
    };
    view! {
        <form class="auth-form" on:submit=submit>
            <h1>"Sign in"</h1>
            <Input id="name" label="Name" value=name autocomplete="username" autofocus=true />
            <Input
                id="password"
                label="Password"
                value=password
                kind="password"
                autocomplete="current-password"
            />
            {move || error.get().map(|e| view! { <p class="error-text">{e}</p> })}
            <button class="btn filled" type="submit" disabled=move || busy.get()>
                "Sign in"
            </button>
            <p class="muted small">
                "Forgot your password? An admin can give you a link to set a new one."
            </p>
        </form>
    }
}

/// The new password, typed twice, must match.
fn matching(password: &str, again: &str) -> Result<(), String> {
    if password != again {
        return Err("The two passwords don't match".into());
    }
    Ok(())
}

#[component]
fn SetupForm(code: String) -> impl IntoView {
    let show_code = code.is_empty();
    let code = RwSignal::new(code);
    let name = RwSignal::new(String::new());
    let password = RwSignal::new(String::new());
    let again = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        if let Err(e) = matching(&password.get_untracked(), &again.get_untracked()) {
            error.set(Some(e));
            return;
        }
        busy.set(true);
        error.set(None);
        let form = SetUp {
            code: code.get_untracked(),
            name: name.get_untracked(),
            password: password.get_untracked(),
        };
        spawn_local(async move {
            match api::set_up(&form).await {
                Ok(_) => enter(),
                Err(e) => {
                    error.set(Some(e));
                    busy.set(false);
                }
            }
        });
    };
    view! {
        <form class="auth-form" on:submit=submit>
            <h1>"Create the admin account"</h1>
            <p class="muted">
                "This server has no accounts yet. The first one is the admin: it invites "
                "others and can reset their passwords."
            </p>
            {show_code.then(|| view! {
                <Input id="code" label="Setup code" value=code autofocus=true />
                <p class="muted small">
                    "The server printed a setup link with this code when it started "
                    "(in Docker: " <code>"docker compose logs"</code> ")."
                </p>
            })}
            <Input
                id="name"
                label="Name"
                value=name
                autocomplete="username"
                autofocus=!show_code
            />
            <Input id="password" label="Password" value=password kind="password" autocomplete="new-password" />
            <Input id="again" label="Password again" value=again kind="password" autocomplete="new-password" />
            <p class="muted small">"At least 8 characters."</p>
            {move || error.get().map(|e| view! { <p class="error-text">{e}</p> })}
            <button class="btn filled" type="submit" disabled=move || busy.get()>
                "Create account"
            </button>
        </form>
    }
}

#[component]
fn LinkForm(code: String) -> impl IntoView {
    let info = RwSignal::new(None::<Result<LinkInfo, String>>);
    {
        let code = code.clone();
        spawn_local(async move { info.set(Some(api::link_info(&code).await)) });
    }
    let name = RwSignal::new(String::new());
    let password = RwSignal::new(String::new());
    let again = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let code = StoredValue::new(code);
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        if let Err(e) = matching(&password.get_untracked(), &again.get_untracked()) {
            error.set(Some(e));
            return;
        }
        busy.set(true);
        error.set(None);
        let form = Redeem {
            code: code.get_value(),
            name: name.get_untracked(),
            password: password.get_untracked(),
        };
        spawn_local(async move {
            match api::redeem(&form).await {
                Ok(_) => enter(),
                Err(e) => {
                    error.set(Some(e));
                    busy.set(false);
                }
            }
        });
    };
    move || {
        match info.get() {
        None => view! { <div class="empty"><div class="spinner inline"></div>"Checking the link…"</div> }
            .into_any(),
        Some(Err(e)) => view! {
            <div class="auth-form">
                <h1>"This link doesn't work"</h1>
                <p class="error-text">{e}</p>
                <a class="btn outlined" href="/">"Go to sign in"</a>
            </div>
        }
        .into_any(),
        Some(Ok(link)) => {
            let invite = link.kind == LinkKind::Invite;
            view! {
                <form class="auth-form" on:submit=submit>
                    <h1>
                        {if invite {
                            "Create your account".to_string()
                        } else {
                            format!("New password for {}", link.name.clone().unwrap_or_default())
                        }}
                    </h1>
                    {invite.then(|| view! {
                        <p class="muted">"You were invited to this collection scanner. Choose a name and a password."</p>
                        <Input id="name" label="Name" value=name autocomplete="username" autofocus=true />
                    })}
                    <Input
                        id="password"
                        label="Password"
                        value=password
                        kind="password"
                        autocomplete="new-password"
                        autofocus=!invite
                    />
                    <Input id="again" label="Password again" value=again kind="password" autocomplete="new-password" />
                    <p class="muted small">
                        {if invite {
                            "At least 8 characters."
                        } else {
                            "At least 8 characters. Every browser signed in to this account is signed out."
                        }}
                    </p>
                    {move || error.get().map(|e| view! { <p class="error-text">{e}</p> })}
                    <button class="btn filled" type="submit" disabled=move || busy.get()>
                        {if invite { "Create account" } else { "Set password" }}
                    </button>
                </form>
            }
            .into_any()
        }
    }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_code_from_the_query() {
        assert_eq!(
            query_value("?code=abc_-1&x=2", "code").as_deref(),
            Some("abc_-1")
        );
        assert_eq!(query_value("?x=2", "code"), None);
        assert_eq!(query_value("", "code"), None);
    }
}
