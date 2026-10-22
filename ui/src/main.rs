//! mtg-scan web UI. Built with `trunk build --release`; the server embeds
//! the output (`dist/`) and serves it from `mtg-scan ui`.

mod add;
mod api;
mod app;
mod decks;
mod filters;
mod gallery;
mod home;
mod mana;
mod pie;
mod preview;
mod review;
mod settings;
mod top;

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(app::App);
}
