# mtg-scan

Scan Magic: The Gathering cards with a webcam, keep the collection in a Markdown
file, and price every card with **TCGplayer** (market price) and **Card Kingdom**
(retail + buylist).

Built in Rust for Windows and tested with an **Insta360 Link 2 Pro**. Any UVC
webcam works.

## How it works

```
camera frame ─┬─ single card in guide ─────────────────────┐
              └─ whole table ─ vision LLM finds each card ─┤
                                                           ▼
              Windows OCR (fast, offline) ─→ vision LLM (fallback)
                                                           ▼
     name / set / number ─→ Scryfall (corrects misreads, picks printing)
                                                           ▼
           TCGplayer + Card Kingdom prices ─→ collection.md
```

* **Hands-free.** The camera watches continuously. When something new holds
  still, it scans it. Cards left in place are never added twice.
* **Recognition.** Windows' built-in OCR runs first, so you don't need to
  install Tesseract. When OCR can't read the card, a vision LLM takes over:
  **Ollama / LM Studio** running locally, or **OpenRouter** in the cloud.
* **Picking the right printing.** Misread names are corrected against
  Scryfall's full name list. If a card has several printings, the photo is
  compared with each printing's image. That separates regular, showcase and
  borderless frames, which is usually where the price differences are.

## Install

Prerequisites, all installed via `winget`:

```powershell
winget install Rustlang.Rustup
winget install Microsoft.VisualStudio.BuildTools --override "--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
winget install Ollama.Ollama          # optional: local vision LLM
ollama pull qwen3-vl:8b-instruct      # ~6 GB; runs well on a 12 GB GPU
```

Build:

```powershell
cargo build --release
copy mtg-scan.example.toml mtg-scan.toml   # settings; edit as needed
copy .env.example .env                     # secrets: DB password, API keys
```

## Usage

```powershell
# Lay cards out under the camera; every card in view is scanned and added
mtg-scan scan-all

# One card at a time inside an on-screen guide (OCR works best this way)
mtg-scan scan

# One-shot: capture, report prices, exit. --dry-run doesn't save anything.
mtg-scan scan-all --once --dry-run

mtg-scan list                         # collection + totals
mtg-scan reprice                      # refresh every price
mtg-scan price "Sheoldred, the Apocalypse" --set dmu
mtg-scan add "Lightning Bolt" --set m11 --number 149 --finish foil --qty 2
mtg-scan remove "Lightning Bolt" --set m11
mtg-scan identify photo1.jpg photo2.jpg --add
mtg-scan cameras                      # list cameras
mtg-scan snapshot frame.jpg           # check framing / focus
```

Live window keys:

| Key | Action |
|---|---|
| (none) | Scanning is automatic. Swap cards and hold still. |
| `Space` | Scan now |
| `A` | Toggle auto-scan |
| `F` | Cycle finish for new cards: nonfoil, foil, etched |
| `U` | Undo the last scan |
| `L` | (`scan`) Retry the current card with the LLM |
| `R` | (`scan-all`) Forget what's on the table so everything is re-added |
| `Q` / `Esc` | Quit |

Box colours: green = added, orange = added but printing uncertain, dark green =
unchanged since the last pass, red = not recognised.

### Camera tips (Insta360 Link 2 Pro)

* Turn **off** AI tracking / auto-framing in Insta360 Link Controller so the
  camera stays still. An overhead or desk-view mount works best.
* `scan-all` captures at 4K. OCR reads cards that fill a good part of the frame.
  Smaller cards are read by the LLM.

## Configuration

`mtg-scan.toml` in the working directory, or `%APPDATA%\mtg-scan\config.toml`.
See [`mtg-scan.example.toml`](mtg-scan.example.toml). CLI flags `--mode`,
`--llm-provider`, `--llm-model` and `--collection` override the file.

| Provider | Setup |
|---|---|
| `ollama` (default in the example) | `ollama pull qwen3-vl:8b-instruct` |
| `lmstudio` | Load a vision model and start the local server |
| `openrouter` | `OPENROUTER_API_KEY=sk-or-...` in `.env` (default model `google/gemini-2.5-flash`) |
| `openai` | `OPENAI_API_KEY=...` in `.env`, or any compatible `base_url` |

Caches (Card Kingdom price list, Scryfall names/sets, card images) live in
`%LOCALAPPDATA%\mtg-scan`.

## Storage

### Markdown (default)

`collection.md` is a readable Markdown table with a summary of totals. It is
also the app's source of truth. You can edit quantities or delete rows by hand.
It is git-ignored by default.

### PostgreSQL (price history)

Connection settings and the password live in a `.env` file next to
`mtg-scan.toml`. It is git-ignored, and real environment variables override it:

```powershell
copy .env.example .env   # fill in MTG_SCAN_DB_HOST, _PORT, _USER, _PASSWORD, _NAME, _SSLMODE
# then set [storage] backend = "postgres" in mtg-scan.toml, or pass --storage postgres
mtg-scan db migrate                   # create/upgrade the schema (schema owner account)
mtg-scan db setup-app-user            # create a least-privilege login for daily use
mtg-scan db status                    # connection, server version, row counts
mtg-scan db import collection.md      # move an existing Markdown collection over
mtg-scan history "Lightning Bolt" --set m11
mtg-scan db export backup.md          # Markdown snapshot of the database
```

The database stores only what isn't available online. Card details come from
Scryfall by ID.

| Table / view | Columns |
|---|---|
| `collection` | `scryfall_id`, `finish`, `quantity`, `added_at`, `updated_at`. One row per **printing + finish**. |
| `price_history` | `scryfall_id`, `finish`, `price_date`, `price_tcg`, `price_ck`, `price_ck_buylist`, `captured_at`. One row per day. |
| `collection_value` (view) | Collection with its latest prices, `value_*` totals and a `scryfall_url` |

Each Scryfall ID is one printing, so different printings of the same card
(say, Lightning Bolt from M11 and from A25) are separate rows, and so are
foil and nonfoil copies. Scans, `add` and `reprice` record that day's prices.
Price history is kept even after you remove a card. The schema is in
[`migrations/`](migrations/).

**Two database logins.** Day to day, the app uses a restricted login that can
read and write rows but can't change or drop tables. The schema owner's login
is kept separately in `.env` as `MTG_SCAN_DB_ADMIN_USER` / `_ADMIN_PASSWORD`,
and is only used for `db migrate` and `db setup-app-user`.

**Automatic price refresh.** When `scan` or `scan-all` starts, mtg-scan reprices
any card not yet priced today in the background, one card at a time while it
waits for scans. No OS scheduler is involved. That adds a daily point to the
price history whenever you use the app. `mtg-scan reprice` refreshes everything
on demand.

## Development

- [CONTRIBUTING.md](CONTRIBUTING.md): how work is tracked, built, tested, reviewed
  and released (GitHub Issues + Project board, branches and PRs, CI, tagged releases).
- [CHANGELOG.md](CHANGELOG.md): what changed in each version.
- [CLAUDE.md](CLAUDE.md): architecture, design decisions and lessons learned.
