# CLAUDE.md

Guidance for Claude Code (and humans) working on **mtg-scan**: a Rust CLI and
desktop tool that scans Magic: The Gathering cards with a webcam, tracks the
collection, and prices cards via TCGplayer and Card Kingdom.

## Development workflow (always follow)

The full process is in [CONTRIBUTING.md](CONTRIBUTING.md). The rules that matter for every change:

- **Never commit or push to `main` or `v1`.** Both are protected. Work on a branch named
  `feature/<issue>-<slug>`, `fix/<issue>-<slug>` or `chore/<slug>`, then open a PR.
- **Two tracks.** `main` is the personal 0.x line (last release v0.3.0). `v1` is The
  Shuffling Tome 1.0 for friends and self-hosters (plan: `docs/v1-plan.md`, milestone
  `1.0.0-alpha.1`, label `track:v1`). 1.0 work branches from and PRs into `v1`.
  1.0 is AGPL-3.0-or-later with a credit term (`LICENSE-ADDITIONAL-TERMS.md`); keep
  the About line in the UI.
- **The backlog is GitHub Issues** (labels `type:*`, `priority:P0-P3`, `area:*`) on the
  "mtg-scan" GitHub Project board. Not this file. Create or link an issue for any new work;
  the PR body says `Closes #N`.
- **Every PR** adds a line under `## [Unreleased]` in `CHANGELOG.md`, fills in the PR template,
  and needs green CI (`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
  `cargo test`, PostgreSQL integration test). Squash-merge.
- **Releases** are tags `vX.Y.Z` that match `Cargo.toml`; `.github/workflows/release.yml`
  publishes the Windows zip. See CONTRIBUTING.md "Releasing".
- **GitHub CLI:** `"C:\Program Files\GitHub CLI\gh.exe"` (not on the Git Bash PATH).

## Commands

```powershell
cargo build --release            # binary: target\release\mtg-scan.exe
cargo test                       # unit tests (no network, no camera)
cargo clippy --all-targets -- -D warnings   # same as CI; must be clean
cargo fmt
$env:MTG_SCAN_DEBUG=1            # print raw OCR/LLM reads during recognition
mtg-scan ocr-dump <image> [--engine paddle|windows]   # hidden: raw OCR lines with positions and time
mtg-scan scan-all --image <photo> --dry-run   # test the table pipeline offline from the camera
mtg-scan ui --frames-from <photo> --table --dry-run   # hidden: the web UI on a photo instead of a camera
mtg-scan ui --dry-run --no-open    # web UI on :8787 without saving scans
mtg-scan health                    # exit 0 when the local web UI answers /api/health

# Docker (Dockerfile + compose.yaml; CI job "Docker image" builds and runs both)
docker compose up -d --build       # SQLite in the `data` volume; --profile postgres / ollama

# Web UI (Leptos -> WebAssembly), embedded into the binary by `cargo build`
cd ui; trunk build --release; cd ..
cargo clippy -p mtg-scan-ui --target wasm32-unknown-unknown -- -D warnings

# PostgreSQL integration test (ignored by default; creates and drops its own schema)
docker run -d --name mtg-scan-test-pg -e POSTGRES_USER=mtg -e POSTGRES_PASSWORD=mtgtest -e POSTGRES_DB=mtg -p 55432:5432 postgres:18
$env:MTG_SCAN_TEST_DATABASE_URL="postgres://mtg:mtgtest@localhost:55432/mtg"; cargo test -- --ignored
```

Toolchain: Rust stable (edition 2024). On Windows: the MSVC target and VS Build
Tools (C++ workload). On Linux: `libclang-dev` (bindgen for the Video4Linux
bindings), and `cargo build --no-default-features --features paddle-ocr`
(headless: no `desktop` feature, so no minifb window; `mtg-scan ui` is the front
end). The `paddle-ocr` feature builds ONNX Runtime in (`ort` downloads it while
building). Platform code is
behind `cfg`: the camera backend (Media Foundation / Video4Linux / AVFoundation)
in `camera.rs`, Windows OCR in `ocr/windows.rs`, the `windows` crate. TLS is rustls with
the ring provider and the OS certificate store (`tls.rs`), so no OpenSSL.

## Architecture (`src/`)

| Module | Role |
|---|---|
| `main.rs` | clap CLI; config overrides; small commands (add/remove/price/list/reprice/history/movers/db) |
| `movers.rs` | `mtg-scan movers`: ranks `CollectionStore::price_moves` (price then vs now per row) by change for the copies owned |
| `config.rs` | `mtg-scan.toml` (`Config`, `StorageConfig`, `RecognitionMode`, `LlmProvider`, `UiConfig` / `DecksConfig` / `PricesConfig` (`currency`, `card_kingdom`) / `[recognition] card_language` for the Settings page), provider defaults; `save_settings` rewrites only those via `toml_edit` (comments kept) |
| `camera.rs` | nokhwa capture (Media Foundation on Windows, Video4Linux on Linux); picks the camera mode itself; MJPEG/NV12/YUYV decode |
| `engine/` | Scanning engine shared by both front ends: capture thread (frames from a `source::FrameSource`: the camera, or a photo with the hidden `--frames-from`; `Watcher` auto-scan trigger; preview frames via `draw`, with the guide and boxes burnt in only for the desktop window (`EngineOptions::draw_overlay`), the web page draws them as SVG from `StateView::overlay` (`ui/src/overlay.rs`); with `EngineOptions::wait_for_camera`, the web UI's, it runs without a camera and looks for one every 5 s, `StateView::camera_error` says why) and worker thread (`Pipeline`); `Command`s in, `EngineEvent` broadcast + `StateView` out. `StateView::save` (Scan page switch, runtime only) picks per scan job whether to add or only price; while off the reprice pauses, so nothing is written. `Command::Clear` empties the session list and `Pipeline::clear`s table memory and undo |
| `live.rs` | Desktop front end (minifb window, keys; `desktop` feature), console logger, `scan_table_once` |
| `web/` | Web front end: Axum server for `mtg-scan ui` (MJPEG preview, SSE events, JSON API, `/api/health`, embedded `ui/dist` via rust-embed); `camera.rs` is the `/api/camera` WebSocket a page sends its camera's JPEGs over (one per `"next"`, same-origin only, close code 4000 = another browser took over) into an `engine::BrowserFeed`; `access.rs` guards other computers with `MTG_SCAN_UI_TOKEN` (cookie / Bearer / `?token=` link, loopback exempt); `settings.rs` serves the Settings page (startup options, Moxfield account, hidden decks, read-only database and connection details) |
| `../shared/` | `mtg-scan-shared`: serde types exchanged by server and UI (`StateView`, `CardView`, `ServerEvent`, `ClientCommand`, `CollectionRow`, …) and logic both sides use: `Stats::of` (landing statistics), `ColorGroup`, `main_type`, `Totals::of`, `RowFilter` + `Facet` (Collection search, filter chips, group-by). Statistics and filtering are computed in the browser from `/api/collection` |
| `../ui/` | `mtg-scan-ui`: Leptos 0.8 CSR app (`camera.rs` "Use this device's camera": getUserMedia, a hidden `<video>`, canvas to JPEG, the WebSocket, remembered in localStorage, reopened when `StateView::capture_size` changes; `overlay.rs` the guide and boxes as SVG; `app.rs` components, `home.rs` landing page, `top.rs` Scan page top 5 (ranking is `shared::top_cards`), `filters.rs` Collection filters, `pie.rs` group-by pie (geometry is `shared::pie`), `gallery.rs` Collection card view with the deck popup (`shared::decks_by_card`; images from Scryfall's CDN via `shared::card_image`, not the rate-limited API redirect), `mana.rs` mana symbols, `api.rs` fetch + EventSource), `style.css` Material 3 tokens |
| `pipeline.rs` | `Pipeline`: `scan_single`, `scan_table` (detect, crop, identify, dedupe against the last pass), undo; with `hold_uncertain` (web UI, `[recognition] review_uncertain`) uncertain matches become `Outcome::Held`, which the engine saves on `Command::Accept` or drops on `Discard` |
| `recognize.rs` | `Recognizer` (OCR, then LLM, then visual refine), `resolve()` hints to a Scryfall printing, `NameIndex`, `MatchQuality` |
| `detect.rs` | Table-mode card detection without an LLM: straight edges (LSD-style region growing, collinear pieces joined), facing pairs → 63:88 rectangles, side check (dark border inside, lighter table outside, edge on the side; on a dark mat a second pass finds the light frame inside the border), then size consensus. `mtg-scan detect-dump <photo> --out x.png` draws the boxes and edges |
| `ocr/` | `OcrEngine` trait (lines with normalised boxes), `open(OcrChoice)` (`[recognition] ocr`); `paddle.rs` PaddleOCR via `oar-ocr` (PP-OCRv5 mobile det + English rec, downloaded on first use to `MTG_SCAN_MODELS_DIR` or `<cache>/models`, SHA-256 checked); `windows.rs` Windows OCR |
| `tls.rs` | rustls setup: `init()` installs the ring provider (reqwest is built without one), `postgres()` is the PostgreSQL connector |
| `llm.rs` | Vision LLM: `read_card`, `detect_cards` (bbox_2d, 0-1000). OpenAI wire format, or Ollama native `/api/chat` |
| `printing.rs` | Visual printing matcher (coarse z-normalised thumbnail correlation against Scryfall images) |
| `scryfall.rs` | Scryfall client: endpoint-aware throttling, 429 retry, printings cache, name/set catalogs, bulk data metadata |
| `bulk.rs` | Scryfall's daily `default_cards` bulk file (gzip JSON Lines, ~80 MB, ~110k paper printings) streamed into the store's card cache (`CardCache`); `spawn_daily` (hourly check, own connection, skipped in `--dry-run`), job state in `app_job` |
| `pricing/` | `PriceService`: Scryfall's prices from the card data (`scryfall_prices`: TCGplayer USD, Cardmarket EUR, MTGO tix per finish; TCGplayer product links), plus `cardkingdom.rs` (daily price list, then a compact index) only when `[prices] card_kingdom` is on |
| `reprice.rs` | The daily price refresh: `record_cached_prices` (one SQL statement per store: today's point for every owned row from `card_printing`) plus Card Kingdom per row when on; once a day (`app_job` `daily_prices`), after the bulk sync and on `mtg-scan reprice`. Same-day upserts `coalesce`, so sources recorded separately keep each other's prices |
| `store/` | `CollectionStore` trait (add, remove, `set_quantity`, `relocate` to another printing/finish, `import`, prices), shared `hydrate` (card details from Scryfall) and `conformance` test suite; `sqlite.rs` (default since 1.0: rusqlite bundled, WAL, `BEGIN IMMEDIATE` writes, migrations applied on open, cents and ISO dates); `postgres.rs` (migrations via `db migrate`, upserts, price history); `markdown.rs` (parse, render, atomic write; import/export format). Migrations live per dialect in `migrations/sqlite/` and `migrations/postgres/` with matching version numbers |
| `moxfield.rs` | Moxfield decks of `[decks] moxfield_user` (Settings page), else `MTG_SCAN_MOXFIELD_USER` (`.env`): deck list and deck contents, cached, 1 req/s |
| `cache.rs` | JSON cache in `MTG_SCAN_CACHE_DIR`, else the OS cache folder (`%LOCALAPPDATA%\mtg-scan`) |
| `model.rs` | `Card`, `Finish`, `Prices`, `CollectionEntry`, `PricePoint` |
| `migrations/` | Numbered SQL files embedded via `include_str!` and listed in `store::postgres::MIGRATIONS` |

### PostgreSQL design (iteration 2)
- **Own only what isn't online; cache the rest.** The collection and price history are the
  only source-of-truth tables. Card details (names, sets, images, Scryfall's daily prices) live
  in `card_printing`, a rebuildable cache filled from Scryfall's daily bulk file by `bulk.rs`
  (`bulk::spawn_daily` checks hourly inside the app; `db sync-cards` runs it now) and, for
  printings not synced yet, from `/cards/collection`. Nothing references the cache. When adding
  a column to an owned table, ask first whether Scryfall already has it.
- `collection`: unique `(scryfall_id, finish)`. A Scryfall ID is one printing, so every printing
  and every finish is its own row. Quantity is always > 0; a row is deleted when it reaches 0.
- `price_history`: unique `(scryfall_id, finish, price_date)`, so one point per day (same-day
  observations upsert). No FK to `collection`, so history survives removing a card.
- `collection_value` view: latest price per row via `LATERAL`; use it for reporting.
- **Secrets live in `.env`** (git-ignored, loaded by `config::load_env_files` from `./.env`, then
  `%APPDATA%\mtg-scan\.env`; real env vars win). The user doesn't want Windows environment
  variables. DB connection: `MTG_SCAN_DB_HOST/_PORT/_USER/_PASSWORD/_NAME/_SSLMODE` (no
  URL-encoding needed), or `MTG_SCAN_DATABASE_URL`. API keys (`OPENROUTER_API_KEY`) go there too.
- Tables live in a configurable schema (default `mtg_scan`). **Two logins:** the app runs as a
  least-privilege role (`MTG_SCAN_DB_USER`, e.g. `mtg_scan_app`; rows only), and the schema owner
  (`MTG_SCAN_DB_ADMIN_USER`) runs `db migrate` and `db setup-app-user`. `PostgresStore::connect`
  never changes the schema; it refuses to run while migrations are pending. Migrations run in
  `connect_admin`, inside a transaction holding a lock on `schema_migrations`.
- Owner-created future tables are granted to the app role via `ALTER DEFAULT PRIVILEGES`, so new
  migrations need no grant statements.
- **No OS dependencies for app behaviour.** The user doesn't want the app to rely on OS
  features such as Task Scheduler or Windows env vars. Periodic work runs inside the app:
  `bulk::spawn_daily` (own thread and connection, hourly) copies Scryfall's daily data and
  then `reprice::refresh` records the day's prices. It pauses while the engine isn't saving
  (`Engine::saving`: dry run or the Save switch off).
- **Backups:** none yet. The user will design the backup process (a GitHub issue tracks it). Don't
  add local dump files or backup jobs on your own.
- Driver: sync `postgres` crate with rustls (`tokio-postgres-rustls`). It uses prepared statements, so connect
  directly or through a *session*-mode pooler, not a transaction-mode one (Supabase port 6543).
- Money is `numeric(10,2)`. The app reads and writes it as `float8` through SQL casts, and UUIDs
  as `text::uuid`, to avoid extra crate type features.

Identification flow: hints `{name, set, number}`, then `resolve()`:
1. set + number lookup, accepted when the read name agrees (`Exact`);
2. name corrected via `NameIndex`, then fuzzy lookup, then choose from that card's printings list
   (`OnlyPrinting` / `NameAndSet` / `NameOnly`);
3. if still `NameOnly`, `printing::best_printing` (`Visual`).

## Hard-won facts (don't relearn these)

- **Never `taskkill /IM msedge.exe`** (or any browser by image name): it closes the user's own
  browser windows. To test the UI, install `puppeteer-core` into the scratchpad and launch the
  installed Edge headless with its own `userDataDir`; `browser.close()` ends only that instance.
  Navigate with `waitUntil: 'domcontentloaded'`: the MJPEG `<img>` never finishes loading, so
  "load"/"networkidle" (and Edge's `--virtual-time-budget`) hang.
- **Test UI and edits against a throwaway collection**, never the user's database. The repo
  folder's (git-ignored) `mtg-scan.toml` and `.env` point at the user's real PostgreSQL, so
  never run the binary from the repo folder or without a scratch config: run it from a scratch
  folder with `--config <scratch>.toml` (`[storage] backend = "sqlite"`, `path = "<scratch>.db"`),
  e.g. `mtg-scan --config scratch.toml ui --manual --no-open --port 8788`, and stop if the build
  failed (a stale binary may not know new flags). This once imported test rows into the real
  collection.
- **Several stores can be open on one collection** (scanner worker, each web request). PostgreSQL
  handles that. `MarkdownStore` makes every change one locked read-modify-write (exclusive
  `std::fs::File::lock` on `<collection>.md.lock`, re-read, change, write), so concurrent changes,
  even from another process, are never lost. Re-reading alone wasn't enough: two writers could
  read the same snapshot.
- **Vision-LLM detection can loop** on messy scenes (e.g. a fanned pile), repeating near-identical
  boxes until it runs out of tokens. Hence `num_predict`/`max_tokens` caps, the regex box parser
  that survives truncated JSON, the card-shape filter (`llm::plausible_cards`), and dropping
  same-card overlapping detections (`pipeline::is_duplicate`).
- `rust-embed` needs `ui/dist` to exist at compile time; `build.rs` creates it (empty) so the server
  builds without the UI and explains how to build it at runtime.

- **The DB owner account can't create logins** on the user's server (no CREATEROLE).
  `db setup-app-user` therefore prints a `CREATE ROLE ... PASSWORD 'SCRAM-SHA-256$...'` statement
  for a superuser to run, keeps the generated password in `.env` as
  `MTG_SCAN_DB_APP_PASSWORD_PENDING`, and finishes on the next run.
- `CREATE SCHEMA IF NOT EXISTS` checks the CREATE privilege *before* the existence check, so a
  least-privilege role can't run it. Only the admin path creates the schema.
- **Docker image layout**: `/data` is the volume and working folder (`MTG_SCAN_DATA_DIR`,
  `MTG_SCAN_CONFIG=/data/mtg-scan.toml`, `MTG_SCAN_CACHE_DIR=/data/cache`, and a `/data/.env`
  is read); models in `/opt/mtg-scan/models` (`MTG_SCAN_MODELS_DIR`), fetched at build time by
  the hidden `fetch-ocr-models`. Requests reach the container from Docker's network, not
  loopback, so `MTG_SCAN_UI_TOKEN` applies to every browser; compose therefore publishes on
  127.0.0.1 unless `MTG_SCAN_PUBLISH` says otherwise. `load_env_files` runs before clap parses,
  so `.env` values reach the flags' `env = ...` variables.
- **Browser cameras**: getUserMedia needs a secure context (HTTPS, `http://localhost` or
  `http://127.0.0.1`), so another computer reaching the UI over plain http can't use its camera.
  `applyConstraints` with an `ideal` size often keeps the current mode, so a mode change reopens
  the camera instead. Headless Edge with `--use-fake-ui-for-media-stream
  --use-fake-device-for-media-stream` gives a fake camera for testing (it stays at 1920x1080).
- **GitHub Actions `windows-latest`** ships PostgreSQL with the service stopped (superuser
  `postgres`, password `root`). CI starts it for the integration test. There are no service
  containers on Windows runners.

- **`.env` values must stay literal.** dotenvy expands `$name` inside values, which broke a real
  password. `config::parse_env` does no substitution, escapes or inline comments; keep it that way.

- **Scryfall rate limits**: `/cards/named` and `/cards/search` allow about 2 req/s. The rest of
  the API allows 10/s. Exceeding them gives HTTP 429. Throttling is per endpoint class in `scryfall.rs`.
- **The vision LLM invents set codes and collector numbers.** It also reads P/T "4/5" as a
  collector number. Names are reliable. So LLM set/number is only used to pick among the
  card's real printings, and a mismatching set + number is only trusted from OCR.
- **Windows OCR returns nothing** for cards ~300 px wide in a 4K desk shot (too soft). It works
  when a card fills much of the frame (single-card `scan`) and on some `scan-all` crops.
  **PaddleOCR reads them**: on `detect1.jpg` (9 cards, 4K) it identified 4 by OCR alone where
  Windows OCR got 0, and it is 4-5x faster (~0.2-0.6 s per crop on CPU). Hence `ocr = "auto"`
  is PaddleOCR everywhere. PP-OCRv6 tiny was worse on camera crops; the Latin v5 model read
  names as well as the English one but the set line ("DMU · EN") worse.
- **Ollama + Qwen3-VL**: the `qwen3-vl:8b` tag is a *thinking* model. `reasoning_effort` is
  ignored and output can be empty or loop. Use `qwen3-vl:8b-instruct` with the native API and
  `think: false` (about 3 s per card, about 13 s to detect 10 cards on an RTX 3080 Ti).
- **Whole-table name reading with an 8B model fails**: it loops. Detect boxes on the full frame,
  then read each crop.
- **Classical detection (`detect.rs`) pitfalls**, each seen on the desk photos: glare greys a
  border (so contrast is relative, and 3 of 4 sides suffice); the camera squashes cards ~4% off
  63:88 (so the free side length is searched ±8%); a card's art window and "title + art" look like
  sideways cards, and a card plus its neighbour's inner border edge makes a bigger card-shaped
  rectangle (so nested rectangles and the size consensus decide, in that order: drop the inner
  one when the outer scores about as well, then drop widths >12% off the typical card, then
  resolve overlaps). Rules keyed on "all four sides" or "crosswise" broke other photos.
  On a **dark mat** the border merges into the mat: `Look::Frame` finds the light frame inside
  the border (light between facing edges, frame proportions) and `Card::around_frame` adds the
  border back. A black-framed card on a black mat shows neither outline and is missed.
- **nokhwa `RequestedFormatType::Closest` picked a 1 fps mode**, and Media Foundation reports
  the active fps as 1. `camera::choose_format` selects the exact mode instead (4K MJPEG
  actually delivers about 19-30 fps).
- **Card Kingdom** `https://api.cardkingdom.com/api/v2/pricelist` is about 70 MB, keyed by
  `scryfall_id`, with separate foil rows. Downloaded at most daily.
- **TCGplayer** has no public API for new developers (deprecated 2023). 0.x used the
  site's internal `mpapi.tcgplayer.com/.../pricepoints`; 1.0 dropped it (fragile, likely
  against their terms) for Scryfall's TCGplayer-sourced `usd` / `usd_foil` / `usd_etched`.
  Scryfall has no Cardmarket price for etched foils and no Card Kingdom prices at all.
- **Moxfield** has no documented API. The site's own endpoints work without a key:
  `api2.moxfield.com/v2/decks/search?authorUserNames=<user>&showIllegal=true` (deck list;
  without `showIllegal` it silently drops decks not legal in their format, 21 of 57 for the
  user; private/unlisted decks need a sign-in; `/v2/users/<u>/decks` and `/v3/users/...` are 404) and `/v3/decks/all/<publicId>` (cards with `scryfall_id`, type line
  and prices: `usd`/`usd_foil` = TCGplayer, `ck`/`ck_foil` = Card Kingdom). Double-faced cards
  have empty top-level `colors`; take them from `card_faces`. The server only serves decks of the
  configured user, so it isn't a general proxy.
- Scryfall `game:paper` printings exclude digital-only versions. Newer sets (e.g. `HOB`,
  The Hobbit) often have one printing per common, hence `OnlyPrinting`.

## Conventions

- Sync code (`reqwest::blocking`). Long work runs on the live worker thread, never the UI loop.
- Errors: `anyhow` with `.context(...)`. Recoverable lookup failures degrade, e.g. a missing
  CK price prints a warning rather than failing the scan.
- Storage goes through `CollectionStore` only. Never touch `collection.md` from other modules.
- Add unit tests for parsing and heuristics (OCR line parsing, LLM JSON, markdown round-trip,
  watcher). Network and camera paths are verified manually with `--dry-run`.
- `mtg-scan.toml` and `collection.md` are git-ignored (local settings and personal data).

## Roadmap and iteration history

Planned work lives in **GitHub Issues** and on the **mtg-scan Project board**; see
CONTRIBUTING.md. Release-by-release detail is in [CHANGELOG.md](CHANGELOG.md).

| Iteration | Version | Summary |
|---|---|---|
| 1 | v0.1.0 | Webcam scanning (single card + whole table), OCR + vision-LLM recognition, Scryfall resolution, TCGplayer + Card Kingdom prices, Markdown collection |
| 2 | v0.2.0 | PostgreSQL storage (one row per printing + finish, daily price history), `.env` secrets, least-privilege app role, background daily reprice, CI/CD + GitHub process |
| 3 | v0.3.0 | Web UI (collection, decks, statistics, Top 5, card view), classical table detection (also on dark mats), Moxfield decks; last release of the personal 0.x line |
| 4 | 1.0 (`v1` branch) | The Shuffling Tome for friends and self-hosters: see `docs/v1-plan.md` |
