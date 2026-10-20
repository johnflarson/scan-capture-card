# CLAUDE.md

Guidance for Claude Code (and humans) working on **mtg-scan**: a Rust CLI and
desktop tool that scans Magic: The Gathering cards with a webcam, tracks the
collection, and prices cards via TCGplayer and Card Kingdom.

## Commands

```powershell
cargo build --release            # binary: target\release\mtg-scan.exe
cargo test                       # unit tests (no network, no camera)
cargo clippy --release           # keep at zero warnings
cargo fmt
$env:MTG_SCAN_DEBUG=1            # print raw OCR/LLM reads during recognition
mtg-scan ocr-dump <image>        # hidden: raw Windows OCR lines with positions
mtg-scan scan-all --image <photo> --dry-run   # test the table pipeline offline from the camera

# PostgreSQL integration test (ignored by default; creates and drops its own schema)
docker run -d --name mtg-scan-test-pg -e POSTGRES_USER=mtg -e POSTGRES_PASSWORD=mtgtest -e POSTGRES_DB=mtg -p 55432:5432 postgres:18
$env:MTG_SCAN_TEST_DATABASE_URL="postgres://mtg:mtgtest@localhost:55432/mtg"; cargo test -- --ignored
```

Toolchain: Rust stable (edition 2024) with the MSVC target. Building needs VS
Build Tools (C++ workload). Windows only for now: it uses Media Foundation for
the camera and `Windows.Media.Ocr` for OCR.

## Architecture (`src/`)

| Module | Role |
|---|---|
| `main.rs` | clap CLI; config overrides; small commands (add/remove/price/list/reprice/history/db) |
| `config.rs` | `mtg-scan.toml` (`Config`, `StorageConfig`, `RecognitionMode`, `LlmProvider`), provider defaults |
| `camera.rs` | nokhwa/Media Foundation capture; picks the camera mode itself; MJPEG/NV12/YUYV decode |
| `live.rs` | Live window (minifb) + worker thread; `Watcher` motion/change detector for auto-scan; `scan_table_once` |
| `pipeline.rs` | `Pipeline`: `scan_single`, `scan_table` (detect, crop, identify, dedupe against the last pass), undo |
| `recognize.rs` | `Recognizer` (OCR, then LLM, then visual refine), `resolve()` hints to a Scryfall printing, `NameIndex`, `MatchQuality` |
| `ocr.rs` | Windows OCR wrapper; returns lines with normalised boxes |
| `llm.rs` | Vision LLM: `read_card`, `detect_cards` (bbox_2d, 0-1000). OpenAI wire format, or Ollama native `/api/chat` |
| `printing.rs` | Visual printing matcher (coarse z-normalised thumbnail correlation against Scryfall images) |
| `scryfall.rs` | Scryfall client: endpoint-aware throttling, 429 retry, printings cache, name/set catalogs |
| `pricing/` | `tcgplayer.rs` (mpapi pricepoints, Scryfall fallback), `cardkingdom.rs` (daily price list, then a compact index), `PriceService` |
| `store/` | `CollectionStore` trait; `markdown.rs` (parse, render, atomic write); `postgres.rs` (migrations, upserts, Scryfall hydration, price history) |
| `cache.rs` | JSON cache in `%LOCALAPPDATA%\mtg-scan` |
| `model.rs` | `Card`, `Finish`, `Prices`, `CollectionEntry`, `PricePoint` |
| `migrations/` | Numbered SQL files embedded via `include_str!` and listed in `store::postgres::MIGRATIONS` |

### PostgreSQL design (iteration 2)
- **Store only what isn't online.** No names, sets or images; card details are fetched from
  Scryfall `/cards/collection` (75 IDs per request) whenever rows are listed. When adding a
  column, ask first whether Scryfall already has it.
- `collection`: unique `(scryfall_id, finish)`. A Scryfall ID is one printing, so every printing
  and every finish is its own row. Quantity is always > 0; a row is deleted when it reaches 0.
- `price_history`: unique `(scryfall_id, finish, price_date)`, so one point per day (same-day
  observations upsert). No FK to `collection`, so history survives removing a card.
- `collection_value` view: latest price per row via `LATERAL`; use it for reporting.
- **Secrets live in `.env`** (git-ignored, loaded by `config::load_env_files` from `./.env`, then
  `%APPDATA%\mtg-scan\.env`; real env vars win). The user doesn't want Windows environment
  variables. DB connection: `MTG_SCAN_DB_HOST/_PORT/_USER/_PASSWORD/_NAME/_SSLMODE` (no
  URL-encoding needed), or `MTG_SCAN_DATABASE_URL`. API keys (`OPENROUTER_API_KEY`) go there too.
- Tables live in a configurable schema (default `mtg_scan`). Migrations run automatically on
  connect, inside a transaction holding a lock on `schema_migrations`.
- Driver: sync `postgres` crate with native-tls. It uses prepared statements, so connect
  directly or through a *session*-mode pooler, not a transaction-mode one (Supabase port 6543).
- Money is `numeric(10,2)`. The app reads and writes it as `float8` through SQL casts, and UUIDs
  as `text::uuid`, to avoid extra crate type features.

Identification flow: hints `{name, set, number}`, then `resolve()`:
1. set + number lookup, accepted when the read name agrees (`Exact`);
2. name corrected via `NameIndex`, then fuzzy lookup, then choose from that card's printings list
   (`OnlyPrinting` / `NameAndSet` / `NameOnly`);
3. if still `NameOnly`, `printing::best_printing` (`Visual`).

## Hard-won facts (don't relearn these)

- **`.env` values must stay literal.** dotenvy expands `$name` inside values, which broke a real
  password. `config::parse_env` does no substitution, escapes or inline comments; keep it that way.

- **Scryfall rate limits**: `/cards/named` and `/cards/search` allow about 2 req/s. The rest of
  the API allows 10/s. Exceeding them gives HTTP 429. Throttling is per endpoint class in `scryfall.rs`.
- **The vision LLM invents set codes and collector numbers.** It also reads P/T "4/5" as a
  collector number. Names are reliable. So LLM set/number is only used to pick among the
  card's real printings, and a mismatching set + number is only trusted from OCR.
- **Windows OCR returns nothing** for cards ~300 px wide in a 4K desk shot (too soft). It works
  when a card fills much of the frame (single-card `scan`) and on some `scan-all` crops.
- **Ollama + Qwen3-VL**: the `qwen3-vl:8b` tag is a *thinking* model. `reasoning_effort` is
  ignored and output can be empty or loop. Use `qwen3-vl:8b-instruct` with the native API and
  `think: false` (about 3 s per card, about 13 s to detect 10 cards on an RTX 3080 Ti).
- **Whole-table name reading with an 8B model fails**: it loops. Detect boxes on the full frame,
  then read each crop.
- **nokhwa `RequestedFormatType::Closest` picked a 1 fps mode**, and Media Foundation reports
  the active fps as 1. `camera::choose_format` selects the exact mode instead (4K MJPEG
  actually delivers about 19-30 fps).
- **Card Kingdom** `https://api.cardkingdom.com/api/v2/pricelist` is about 70 MB, keyed by
  `scryfall_id`, with separate foil rows. Downloaded at most daily.
- **TCGplayer** `https://mpapi.tcgplayer.com/v2/product/{id}/pricepoints` needs no key.
  `printingType` is "Normal" or "Foil". Etched cards use `tcgplayer_etched_id`.
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

## Roadmap

Track every iteration here. When a feature lands, move it to the changelog with the date.

### Iteration 2: PostgreSQL storage (done)
- [x] Schema `migrations/0001_init.sql`: `collection`, `price_history`, `collection_value` view.
- [x] `store/postgres.rs` implementing `CollectionStore`, with auto-migration on connect.
- [x] Config `[storage] backend`, `database_url_env` (default `MTG_SCAN_DATABASE_URL`), `schema`; `--storage` flag.
- [x] `db migrate | status | import | export`, `history <card>`.
- [x] Verified against a local Postgres 18 container (integration test + CLI end to end).
- [x] `.env` support for the connection and secrets; the user's server (PostgreSQL on a LAN host,
      port 5433, no SSL offered) is reachable.
- [x] Connected to the user's database (`cardcollection`, schema `mtg_scan`) and imported `collection.md`.
- [x] `[storage] backend = "postgres"` is the default in the user's `mtg-scan.toml`.

### Backlog
- [ ] On-screen text labels in the live window (bitmap font) instead of console-only names.
- [ ] Classical card detection (contours of the black border) so `scan-all` works without an LLM.
- [ ] Foil detection (sheen across frames), and language detection for non-English cards.
- [ ] Condition tracking (NM/LP/MP) using Card Kingdom condition prices.
- [ ] Price-change report after `reprice` (biggest movers).
- [ ] Batch-confirm UI: review uncertain (orange) identifications before saving.
- [ ] Linux/macOS: v4l/AVFoundation via nokhwa, Tesseract OCR fallback.

## Changelog

### Iteration 2, part 1 (2026-10-20)
- PostgreSQL backend: `collection` (one row per Scryfall printing + finish) and daily
  `price_history` (`price_tcg`, `price_ck`, `price_ck_buylist`); card data stays on Scryfall.
- `collection_value` view with latest prices and values.
- CLI: `--storage`, `db migrate/status/import/export`, `history`.
- Scryfall batch lookup (`cards_by_id`); the Scryfall client is shared via `Rc` between App and store.
- `.env` file support for DB connection fields and API keys; `.env.example` template.
- Own literal `.env` parser (`config::parse_env`) replaced dotenvy, whose `$VAR` substitution
  silently corrupted a password containing `$`.
- Live on the user's PostgreSQL server; the 10 Hobbit cards were imported.

### Iteration 1 (2026-10-20)
- Rust CLI `mtg-scan` with live webcam window (Insta360 Link 2 Pro, 4K MJPEG).
- Continuous hands-free scanning: still-scene detection, no duplicate adds, undo.
- `scan` (single card + guide) and `scan-all` (whole table via LLM detection), `--once`, `--image`, `--dry-run`.
- Recognition: Windows OCR, then vision LLM (Ollama / LM Studio / OpenRouter / OpenAI-compatible).
- Scryfall resolution with name correction, printing selection and visual printing match.
- Prices: TCGplayer market (live) and Card Kingdom retail + buylist (daily list).
- Markdown collection store (`collection.md`) behind the `CollectionStore` trait.
- First real test: 10 *The Hobbit* commons on a desk mat, 10/10 identified and priced.
