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
```

Toolchain: Rust stable (edition 2024) with the MSVC target. Building needs VS
Build Tools (C++ workload). Windows only for now: it uses Media Foundation for
the camera and `Windows.Media.Ocr` for OCR.

## Architecture (`src/`)

| Module | Role |
|---|---|
| `main.rs` | clap CLI; config overrides; small commands (add/remove/price/list/reprice) |
| `config.rs` | `mtg-scan.toml` (`Config`, `RecognitionMode`, `LlmProvider`), provider defaults |
| `camera.rs` | nokhwa/Media Foundation capture; picks the camera mode itself; MJPEG/NV12/YUYV decode |
| `live.rs` | Live window (minifb) + worker thread; `Watcher` motion/change detector for auto-scan; `scan_table_once` |
| `pipeline.rs` | `Pipeline`: `scan_single`, `scan_table` (detect, crop, identify, dedupe against the last pass), undo |
| `recognize.rs` | `Recognizer` (OCR, then LLM, then visual refine), `resolve()` hints to a Scryfall printing, `NameIndex`, `MatchQuality` |
| `ocr.rs` | Windows OCR wrapper; returns lines with normalised boxes |
| `llm.rs` | Vision LLM: `read_card`, `detect_cards` (bbox_2d, 0-1000). OpenAI wire format, or Ollama native `/api/chat` |
| `printing.rs` | Visual printing matcher (coarse z-normalised thumbnail correlation against Scryfall images) |
| `scryfall.rs` | Scryfall client: endpoint-aware throttling, 429 retry, printings cache, name/set catalogs |
| `pricing/` | `tcgplayer.rs` (mpapi pricepoints, Scryfall fallback), `cardkingdom.rs` (daily price list, then a compact index), `PriceService` |
| `store/` | `CollectionStore` trait; `markdown.rs` implementation (parse, render, atomic write) |
| `cache.rs` | JSON cache in `%LOCALAPPDATA%\mtg-scan` |
| `model.rs` | `Card`, `Finish`, `Prices`, `CollectionEntry` |

Identification flow: hints `{name, set, number}`, then `resolve()`:
1. set + number lookup, accepted when the read name agrees (`Exact`);
2. name corrected via `NameIndex`, then fuzzy lookup, then choose from that card's printings list
   (`OnlyPrinting` / `NameAndSet` / `NameOnly`);
3. if still `NameOnly`, `printing::best_printing` (`Visual`).

## Hard-won facts (don't relearn these)

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

### Iteration 2: PostgreSQL storage (next)
- [ ] `store/postgres.rs` implementing `CollectionStore` (`postgres` crate, sync).
- [ ] Schema: `cards` (scryfall_id PK, name, set, number, rarity, …),
      `collection` (scryfall_id, finish, quantity, added),
      `price_history` (scryfall_id, finish, source, price, captured_at) so value is tracked over time.
- [ ] Config `[storage] backend = "markdown" | "postgres"`, `database_url` (env var name).
- [ ] `mtg-scan migrate` to import an existing `collection.md` into Postgres.
- [ ] Keep markdown export: `mtg-scan export collection.md`.

### Backlog
- [ ] On-screen text labels in the live window (bitmap font) instead of console-only names.
- [ ] Classical card detection (contours of the black border) so `scan-all` works without an LLM.
- [ ] Foil detection (sheen across frames), and language detection for non-English cards.
- [ ] Condition tracking (NM/LP/MP) using Card Kingdom condition prices.
- [ ] Price-change report after `reprice` (biggest movers).
- [ ] Batch-confirm UI: review uncertain (orange) identifications before saving.
- [ ] Linux/macOS: v4l/AVFoundation via nokhwa, Tesseract OCR fallback.

## Changelog

### Iteration 1 (2026-10-20)
- Rust CLI `mtg-scan` with live webcam window (Insta360 Link 2 Pro, 4K MJPEG).
- Continuous hands-free scanning: still-scene detection, no duplicate adds, undo.
- `scan` (single card + guide) and `scan-all` (whole table via LLM detection), `--once`, `--image`, `--dry-run`.
- Recognition: Windows OCR, then vision LLM (Ollama / LM Studio / OpenRouter / OpenAI-compatible).
- Scryfall resolution with name correction, printing selection and visual printing match.
- Prices: TCGplayer market (live) and Card Kingdom retail + buylist (daily list).
- Markdown collection store (`collection.md`) behind the `CollectionStore` trait.
- First real test: 10 *The Hobbit* commons on a desk mat, 10/10 identified and priced.
