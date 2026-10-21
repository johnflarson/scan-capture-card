# mtg-scan web UI

[Leptos](https://leptos.dev) 0.8, client-side rendered and compiled to
WebAssembly by [Trunk](https://trunkrs.dev). `mtg-scan ui` serves it, with
the build output (`dist/`) embedded in the binary.

## Build

```powershell
rustup target add wasm32-unknown-unknown   # once
cargo install trunk --locked               # once
cd ui
trunk build --release                      # -> ui/dist
cd ..
cargo build --release                      # embeds ui/dist
```

## Develop with live reload

```powershell
mtg-scan ui --no-open --dry-run   # terminal 1: scanner + API on :8787
cd ui; trunk serve --open         # terminal 2: UI on :8080, /api forwarded to :8787
```

Edits to `src/*.rs` or `style.css` rebuild and reload the page.

## Layout

| File | |
|---|---|
| `src/main.rs` | entry point |
| `src/app.rs` | top bar and tabs, the shared `CollectionCache`, Scan page (camera, controls, session list), Collection page and edit dialog |
| `src/filters.rs` | Collection page filter chips, group-by and the type / rarity / color cells (matching and grouping live in `mtg_scan_shared::RowFilter` and `Facet`) |
| `src/add.rs` | Collection page "Add card" dialog (Scryfall link, quantity, finish) |
| `src/decks.rs` | Decks page (deck list, deck detail with ownership) and the shared `DeckCache`; ownership is `mtg_scan_shared::DeckOwnership` |
| `src/mana.rs` | Mana symbols for colors (Scryfall's card-symbol SVGs): `Mana`, `ManaSymbols`, `group_symbol` |
| `src/preview.rs` | Card image on hovering a card name: `CardLink`, the `Preview` context and `PreviewLayer` |
| `src/settings.rs` | Settings page (cog in the top bar): startup options, Moxfield account, deck Show / Hide, database and connection details |
| `src/review.rs` | Scan page "To review" list: accept, pick another printing, or discard held cards |
| `src/home.rs` | Home (landing) page: launch buttons and statistics (computed by `mtg_scan_shared::Stats`), Decks placeholder |
| `src/api.rs` | JSON requests and the Server-Sent Events subscription |
| `style.css` | Material 3 role tokens with the luminaldata palette (dark by default) |

Types shared with the server come from `../shared` (`mtg-scan-shared`), so an
API change that isn't made on both sides fails to compile.
