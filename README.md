# mtg-scan

Scan Magic: The Gathering cards with a webcam, keep the collection in SQLite (or
your own PostgreSQL), and price every card with Scryfall's daily prices:
**TCGplayer** (USD), **Cardmarket** (EUR) and MTGO tix, plus **Card Kingdom**
(retail + buylist) if you switch it on.

Built in Rust for Windows and tested with an **Insta360 Link 2 Pro**. Any UVC
webcam works.

> **This is the `v1` branch: The Shuffling Tome 1.0, in progress.** 1.0 turns
> mtg-scan into an app friends and other self-hosters can run (Docker / Linux
> first, SQLite by default, accounts, prices from Scryfall). See
> [docs/v1-plan.md](docs/v1-plan.md). Until it is done, parts of this README
> describe the 0.x behaviour. The personal 0.x line lives on `main`.

## How it works

```
camera frame ─┬─ single card in guide ─────────────────────┐
              └─ whole table ─ black borders → each card ──┤
                               (vision LLM if none found)  │
                                                           ▼
              Windows OCR (fast, offline) ─→ vision LLM (fallback)
                                                           ▼
     name / set / number ─→ Scryfall (corrects misreads, picks printing)
                                                           ▼
   TCGplayer / Cardmarket prices (Scryfall) ─→ SQLite or PostgreSQL
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
rustup target add wasm32-unknown-unknown   # for the web UI
cargo install trunk --locked
cd ui; trunk build --release; cd ..        # web UI -> ui/dist (embedded below)
cargo build --release
copy mtg-scan.example.toml mtg-scan.toml   # settings; edit as needed
copy .env.example .env                     # secrets and settings: DB password, API keys, Moxfield user
```

## Usage

### Web UI

```powershell
mtg-scan ui            # single-card mode; add --table for every card in view
```

It opens `http://localhost:8787` and also prints an address for **other
computers on your network** (allow mtg-scan on *Private networks* when Windows
asks). The pages:

- **Home** (the start page): big Scan / Collection / Decks buttons next to
  collection statistics: card and printing counts, value, the 7-day value
  change (PostgreSQL), breakdowns by color, type, rarity, finish and set, the
  most valuable and most recently added cards. Click the MTG logo to return.
- **Scan**: the camera with the guide or detected-card boxes; mode, finish
  and auto-scan controls; Scan now / Retry with LLM / Rescan all / Undo; and
  each card the moment it is saved, with its image, set, price links and
  match confidence, plus session totals. Switch **Save to collection** off to
  only identify and price cards (nothing is written, not even the background
  price refresh); **Clear** empties the session list to start fresh (saved
  cards stay saved). **Top 5** shows the session's most expensive cards by
  TCGplayer and by Cardmarket price (and Card Kingdom, when on), each with its price; the × on a card hides it from there
  (e.g. a mis-scan) without touching the collection. When the printing is uncertain the
  card waits under **To review** instead: accept it, pick the right printing
  from thumbnails, or discard it (`[recognition] review_uncertain = false`
  turns this off).
- **Collection**: everything you own, with each card's type, rarity and color.
  Search by name, set, type, rarity or color (e.g. `blue creature`). Pick
  filter chips, e.g. Red + Green and Rare + Mythic for red or green rares and
  mythics, and group the table by type, rarity or color (click a group's
  header to collapse it); while grouped, a small pie beside the filters shows
  each group's share of the cards. **Table** / **Cards** switches between the
  table and a grid of card images with their names; hovering a card (tapping
  it on a phone) lists the Moxfield decks it is in. The totals follow
  the filter. **Add card** adds a card by hand from a Scryfall link (quantity
  and finish; merged into a row you already own). The pencil on
  each row edits it: change the quantity or finish, paste a Scryfall link
  (e.g. `https://scryfall.com/card/a25/141/lightning-bolt`) to fix the
  printing, or remove the row. Editing is off in `--dry-run`.
- **Decks**: your public Moxfield decks (enter your profile link, e.g.
  `https://moxfield.com/users/YourName`, on the Settings page). Each deck shows how
  much of it you already own (any printing counts) and what it costs to
  finish; open one for every card's status: owned, other printing, partly
  owned or missing. Prices are Moxfield's (TCGplayer and Card Kingdom).
- **Settings** (the cog, top right):
  - **Startup:** whether auto-scan is on when `mtg-scan ui` starts, single-card or
    table mode, and the default finish. `--manual`, `--table` and `--finish` override
    them for one run, and the page says so.
  - **Moxfield account.**
  - **Decks:** Show / Hide for each deck. Hidden (e.g. retired) decks are left off
    the Decks page and out of the deck statistics.
  - **Database:** where the collection is stored, with live status (server version,
    schema version, row counts). Never passwords.
  - **Connect:** how to connect from other computers and phones. On this computer it
    also shows the sign-in link when an access token is set.

  Changes are saved in the `[ui]` and `[decks]` sections of `mtg-scan.toml`, and the
  rest of that file is kept as it is.

Hover over any card name to see the card. It works on phones too (open the
"other computers" address). Dark theme, luminaldata colours.

**Who can use it.** This computer always can. By default so can anyone on your
network. To require a token from other computers:

```powershell
mtg-scan ui-token          # saves MTG_SCAN_UI_TOKEN in .env and prints a sign-in link
mtg-scan ui-token --show   # print it again
```

Other computers then get a sign-in page (or open the printed `?token=` link
once; the browser remembers it). To keep the UI on this computer only, run
`mtg-scan ui --bind 127.0.0.1`.

### Desktop window and command line

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
| `M` | Switch between single-card and table mode |
| `U` | Undo the last scan |
| `L` | (`scan`) Retry the current card with the LLM |
| `R` | (`scan-all`) Forget what's on the table so everything is re-added |
| `Q` / `Esc` | Quit |

Box colours: teal = added, amber = added but printing uncertain, dark teal =
unchanged since the last pass, blue = priced only (dry run), red = not
recognised.

### Camera tips (Insta360 Link 2 Pro)

* Turn **off** AI tracking / auto-framing in Insta360 Link Controller so the
  camera stays still. An overhead or desk-view mount works best.
* `scan-all` captures at 4K. OCR reads cards that fill a good part of the frame.
  Smaller cards are read by the LLM.
* `scan-all` finds the cards by their black borders, in under a second and
  without an LLM. Lay them out flat, near upright or sideways (up to about 25°
  off), not overlapping. A busy playmat is fine, and so is a dark one,
  except that a black-framed card on a black mat can be missed. Only when it finds nothing
  does the vision LLM look for cards; `[recognition] detection = "llm"` always
  uses the LLM instead (slower, but it copes with fanned piles).

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

### SQLite (default)

One file, no server, with daily price history. It lives in your data folder
(`%LOCALAPPDATA%\mtg-scan\collection.db` on Windows, `~/.local/share/mtg-scan`
on Linux, or `MTG_SCAN_DATA_DIR` when set). `[storage] path` or `--db <file>`
picks another file. It is created and upgraded automatically.

```powershell
mtg-scan db status                    # file, SQLite version, row counts
mtg-scan db import collection.md      # copy a Markdown collection in (sets quantities)
mtg-scan db export backup.md          # Markdown snapshot of the database
```

### Markdown

`collection.md` (`[storage] backend = "markdown"`) is a readable Markdown table
with a summary of totals, and no price history. You can edit quantities or
delete rows by hand. It is git-ignored by default.

### PostgreSQL (your own server)

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
mtg-scan movers --days 30             # biggest price gains and drops, collection value change
mtg-scan db export backup.md          # Markdown snapshot of the database
```

The database owns what isn't available online: what you have and its price
history. Card details (names, sets, images and Scryfall's daily prices) are a
local copy of Scryfall's daily bulk data, refreshed inside the app while it
runs (`mtg-scan db sync-cards` refreshes it now; the first copy is about 80 MB
to download and 50 MB on disk). Nothing depends on the copy: it can be rebuilt
from Scryfall at any time.

| Table / view | Columns |
|---|---|
| `collection` | `scryfall_id`, `finish`, `quantity`, `added_at`, `updated_at`. One row per **printing + finish**. |
| `price_history` | `scryfall_id`, `finish`, `price_date`, `price_tcg`, `price_ck`, `price_ck_buylist`, `captured_at`. One row per day. |
| `collection_value` (view) | Collection with its latest prices, `value_*` totals and a `scryfall_url` |
| `card_printing` | Local copy of Scryfall's card data: name, set, number, rarity, colors, type, finishes, images, TCGplayer / Cardmarket IDs, and Scryfall's daily USD, EUR and MTGO prices. One row per printing. |
| `app_job` | When background jobs (the daily card data sync) last finished |

Each Scryfall ID is one printing, so different printings of the same card
(say, Lightning Bolt from M11 and from A25) are separate rows, and so are
foil and nonfoil copies. Scans, `add` and `reprice` record that day's prices.
Price history is kept even after you remove a card. The schema is in
[`migrations/`](migrations/).

**Two database logins.** Day to day, the app uses a restricted login that can
read and write rows but can't change or drop tables. The schema owner's login
is kept separately in `.env` as `MTG_SCAN_DB_ADMIN_USER` / `_ADMIN_PASSWORD`,
and is only used for `db migrate` and `db setup-app-user`.

**Automatic price refresh.** While `mtg-scan ui` (or the desktop window) runs,
it copies Scryfall's daily card data and then records that day's prices for
every card you own: TCGplayer (USD), Cardmarket (EUR) and MTGO tix, in one
step and without a request per card. No OS scheduler is involved. That adds a
daily point to the price history whenever you use the app. `mtg-scan reprice`
does it now. Nothing is written while the Scan page's Save switch is off.

**Card Kingdom (optional).** `[prices] card_kingdom = true` also records Card
Kingdom's retail and buylist prices, from its public price list (about 70 MB,
downloaded at most once a day). The Card Kingdom columns, totals and Top 5 list
appear when those prices exist.

## Development

- [CONTRIBUTING.md](CONTRIBUTING.md): how work is tracked, built, tested, reviewed
  and released (GitHub Issues + Project board, branches and PRs, CI, tagged releases).
- [CHANGELOG.md](CHANGELOG.md): what changed in each version.
- [CLAUDE.md](CLAUDE.md): architecture, design decisions and lessons learned.

## License

Copyright (C) 2026 John Larson. The Shuffling Tome 1.0 and later is licensed
under the [GNU Affero General Public License v3.0 or later](LICENSE), with an
[additional term](LICENSE-ADDITIONAL-TERMS.md) that keeps the author credit
visible in the UI. Releases up to v0.3.0 were MIT-licensed.

Card data, images and prices come from [Scryfall](https://scryfall.com). The
Shuffling Tome is unofficial Fan Content permitted under the
[Fan Content Policy](https://company.wizards.com/en/legal/fancontentpolicy).
Not approved/endorsed by Wizards. Portions of the materials used are property
of Wizards of the Coast. ©Wizards of the Coast LLC.
