//! mtg-scan web UI. Built with `trunk build --release`; the server embeds
//! the output (`dist/`) and serves it from `mtg-scan ui`.

mod api;
mod app;
mod filters;
mod home;

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(app::App);
}
