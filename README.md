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
               PaddleOCR (fast, offline) ─→ vision LLM (fallback)
                                                           ▼
     name / set / number ─→ Scryfall (corrects misreads, picks printing)
                                                           ▼
   TCGplayer / Cardmarket prices (Scryfall) ─→ SQLite or PostgreSQL
```

* **Hands-free.** The camera watches continuously. When something new holds
  still, it scans it. Cards left in place are never added twice.
* **Recognition.** OCR runs first: PaddleOCR (PP-OCRv5, built in; its models,
  ~13 MB, download on first use), or Windows' own OCR on Windows
  (`[recognition] ocr`). When OCR can't read the card, a vision LLM takes over:
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

### Linux (headless)

Linux builds have no desktop window: `mtg-scan ui` is the front end, and it starts
without a camera too. OCR is PaddleOCR.

```sh
sudo apt install build-essential libclang-dev   # Debian/Ubuntu
rustup target add wasm32-unknown-unknown && cargo install trunk --locked
(cd ui && trunk build --release)
cargo build --release --no-default-features --features paddle-ocr
./target/release/mtg-scan ui --no-open
```

### Docker

The image runs the web UI on port 8787 and keeps everything (the SQLite
database, settings, caches) in the `/data` volume. PaddleOCR and its models are
built in. From a checkout of this repository (for `compose.yaml`):

```sh
docker compose up -d                      # http://localhost:8787
docker compose --profile postgres up -d   # with a PostgreSQL server
docker compose --profile ollama up -d     # with a local vision LLM (Ollama)
```

Settings and secrets go in a `.env` file next to `compose.yaml` (see
[`.env.example`](.env.example)). For example, with the `postgres` profile:

```sh
MTG_SCAN_STORAGE=postgres
MTG_SCAN_DB_HOST=postgres
MTG_SCAN_DB_USER=mtg
MTG_SCAN_DB_PASSWORD=choose-a-password
MTG_SCAN_DB_NAME=mtg
```

then create the tables once with
`docker compose --profile postgres run --rm mtg-scan db migrate`. With the
`ollama` profile, set `MTG_SCAN_LLM_PROVIDER=ollama` and
`MTG_SCAN_LLM_BASE_URL=http://ollama:11434`, and download the model with
`docker compose --profile ollama exec ollama ollama pull qwen3-vl:8b-instruct`.

* **First start.** The log has a setup link (`docker compose logs mtg-scan`):
  open it to create the admin account.
* **Other computers.** The UI is published on this computer only. Set
  `MTG_SCAN_PUBLISH=0.0.0.0` to open it to your network; everyone signs in.
* **Camera.** Docker Desktop on Windows and macOS can't pass cameras to
  containers: open the UI at `http://localhost:8787` on the computer the camera is
  plugged into and press **Use this device's camera** on the Scan page. On Linux
  you can also pass the camera in: uncomment `devices` in `compose.yaml` (or
  `docker run --device /dev/video0`).
* **Commands** run in the container: `docker compose exec mtg-scan mtg-scan db status`.
* Without compose: `docker run -d -p 127.0.0.1:8787:8787 -v mtg-scan:/data
  ghcr.io/johnflarson/scan-capture-card`.

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
- **Scan**: the camera with the guide or detected-card boxes. The camera is
  the one attached to the computer running mtg-scan, or, with **Use this
  device's camera**, the camera of the computer the browser runs on (e.g.
  mtg-scan in Docker, or on another machine). Browsers allow that only on
  `http://localhost` and HTTPS pages; the page remembers the choice, and with
  several cameras it lets you pick one. Then the mode, finish
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
  - **Prices and cards:** the headline currency (USD from TCGplayer or EUR from
    Cardmarket) for totals, group values and the most valuable cards; Card Kingdom
    prices on or off (admins, for everyone); and the language your cards are
    printed in (scans record that printing when Scryfall has it).
  - **Moxfield account.**
  - **Decks:** Show / Hide for each deck. Hidden (e.g. retired) decks are left off
    the Decks page and out of the deck statistics.
  - **Database** (admins): where the collection is stored, with live status
    (server version, schema version, row counts). Never passwords.
  - **Account:** change your password, sign out, and API tokens for scripts.
  - **Users** (admins): invite links, password-reset links, admin and disabled.
  - **Connect:** how to connect from other computers and phones.

  Each account's settings are its own and saved with it. What an account hasn't
  set comes from `mtg-scan.toml` (`[ui]`, `[prices] currency`, `[recognition]
  card_language`); the first account also starts with the file's Moxfield
  account and hidden decks. Card Kingdom is saved in `[prices] card_kingdom`, and
  the rest of that file is kept as it is.

Hover over any card name to see the card. It works on phones too (open the
"other computers" address). Dark theme, luminaldata colours.

**Who can use it.** Everyone signs in, on this computer too. The first time
`mtg-scan ui` starts it prints (and opens) a setup link that creates the admin
account, then walks the admin through the setup: how cards are read (the printed
text, a local Ollama model it can download and test, or a cloud model with your
key), the camera, prices and language, and usage statistics. Settings → Server
setup opens it again. The admin invites others from Settings → Users: an invite link works
once, for 7 days. There is no open sign-up.

* Forgot a password? An admin makes a reset link under Users (it works once,
  for 24 hours). An admin who can't sign in runs
  `mtg-scan user reset-link <name>` on the server.
* Scripts use an API token from Settings → Account:
  `Authorization: Bearer tome_...`.
* Sessions last 30 days from the last visit. After 5 wrong passwords for a name
  (or 20 from one address) sign-in waits 15 minutes.
* `mtg-scan user list` lists the accounts. `mtg-scan ui --bind 127.0.0.1` keeps
  the UI on this computer only.

**Scanning together.** Every account has its own scan session (mode, finish,
auto-scan, the cards scanned, undo), and everyone's scans take turns on the
one recognizer. Scans go into the scanner's own collection, in their card
language. The camera attached to the server belongs to the first account's
session; everyone else presses **Use this device's camera** on the Scan page.

**Vision model keys.** With a cloud vision model (`[llm] provider =
"openrouter"`, `"openai"`, `"anthropic"` or `"gemini"`), each person can save
their own API key under Settings → Vision model, so their scans use their key.
The server's key in `.env` (`OPENROUTER_API_KEY`, `OPENAI_API_KEY`,
`ANTHROPIC_API_KEY`, `GEMINI_API_KEY`) is for everyone who hasn't saved one, and can be
left out. Saved keys are encrypted with the server's key: `secret.key` in the
data folder (made on first start; keep it with your database backups) or
`MTG_SCAN_SECRET_KEY` in `.env`. Without it, saved keys can't be read and
people enter them again.

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
mtg-scan remove "Command Tower" --condition lp --language ja
mtg-scan --user Bob list              # another account's collection (default: the first admin)
mtg-scan identify photo1.jpg photo2.jpg --add
mtg-scan llm check                    # is the vision model ready (Ollama: model, vision, GPU)?
mtg-scan llm pull                     # Ollama: download the configured model
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
`%LOCALAPPDATA%\mtg-scan` (`~/.cache/mtg-scan` on Linux).

Folders can also be set with environment variables (handy for a Docker volume):

| Variable | Default |
|---|---|
| `MTG_SCAN_CONFIG` | the settings file: `./mtg-scan.toml`, then `config.toml` in `%APPDATA%\mtg-scan` (`~/.config/mtg-scan`). The Settings page creates it if it doesn't exist. |
| `MTG_SCAN_DATA_DIR` | the SQLite database's folder: `%LOCALAPPDATA%\mtg-scan` (`~/.local/share/mtg-scan`) |
| `MTG_SCAN_CACHE_DIR` | the caches: `%LOCALAPPDATA%\mtg-scan` (`~/.cache/mtg-scan`) |

## Usage statistics

The setup wizard asks whether the server may send anonymous usage statistics;
nothing is sent until an admin says yes. Then a running `mtg-scan ui` sends one
small ping a day to the author's collector:

| Sent | Example |
|---|---|
| A random id for this server (`telemetry.json` in the data folder; delete it for a new one) | `3f9c...` |
| Version, operating system, processor, how it was installed | `1.0.0-alpha.1`, `linux`, `x86_64`, `docker` |
| Storage, how cards are read, the vision model's provider, the OCR engine, Card Kingdom on/off, setup finished | `sqlite`, `auto`, `ollama`, `paddle` |
| Accounts, cards scanned and not recognised since the last ping, errors by kind, each rounded into a range | `2-5`, `100-999`, `1-9`, `camera: 1-9` |

Never sent: cards, collections, decks, images, names, e-mail or IP addresses,
error messages, settings values such as addresses or keys. The collector doesn't
keep IP addresses either.

* See exactly what the next ping sends: Settings → Usage statistics, or
  `mtg-scan telemetry show`.
* Change your mind: the same card, or `mtg-scan telemetry on` / `off`
  (`[telemetry] enabled` in `mtg-scan.toml`).
* `DO_NOT_TRACK=1` in the environment or `.env` turns it off whatever the file says.
* `MTG_SCAN_TELEMETRY_URL` sends to your own collector instead. The collector is
  in this repository: [`collector/`](collector/README.md).

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
mtg-scan db copy --to postgres        # move the accounts, collections and price history to PostgreSQL
mtg-scan --storage postgres db copy --to sqlite --file collection.db   # ...or back
```

Every account has its own collection; prices are shared. A collection row is a
printing in one finish, condition (Near Mint to Damaged) and language, so two
Lightning Bolts from the same set can be a Near Mint English one and a played
Japanese one. Scans record Near Mint copies in the card language from Settings;
the Collection page's edit dialog changes the condition and language. A copy
(`db copy`) brings the accounts with their passwords; people sign in again and
make new API tokens there.

Coming from 0.x with PostgreSQL? `db copy --to sqlite` reads the 0.x database
without upgrading it, so 0.x keeps working while you try 1.0 on the copy.
`mtg-scan db migrate` upgrades the PostgreSQL database itself for 1.0 (0.x
then refuses it).

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
