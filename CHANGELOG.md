# Changelog

All notable changes to mtg-scan. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/): MAJOR for breaking changes, MINOR
for new features, PATCH for fixes.

Every pull request adds a line under **Unreleased**. A release renames that
section to the new version (see CONTRIBUTING.md, "Releasing").

## [Unreleased]

### Added
- Group-by pie on the Collection page (#55): while the table is grouped by type, rarity or color,
  a small pie in the filter box shows each group's share of the cards, following the search and
  filter chips. Its key lists every group with its percentage; hovering a slice or key entry
  highlights it and shows the card count.
- Table mode finds cards on a dark playmat too (#53). On a black mat the card's black border
  disappears into the mat, so the border detector found nothing and every pass fell back to the
  vision LLM (~13 s). A second pass now looks for the light frame inside the border and grows it
  to the whole card: 6 of 7 cards on a black mat in 0.2 s, all identified. Black-framed cards
  on a black mat can still be missed: their frame is as dark as the border. Light tables are
  unchanged (one desk photo gained a card the border pass missed).
- Top 5 on the Scan page (#51): beside the session list (under it on narrower screens), this
  session's five most expensive cards by TCGplayer price and by Card Kingdom price, as card
  images with that store's price under each (each printing once; hover for the full-size card,
  click for Scryfall). The × on a card hides it from both lists, e.g. a mis-scan, without
  changing the collection or the session list; **Show hidden** brings them back, and **Clear**
  starts the lists over.
- Scan without saving, and start fresh (#49). The Scan page has a **Save to collection** switch:
  off, cards are identified and priced but not added, and the background price refresh pauses,
  so nothing is written to the database (a "Not saving scans" chip shows in the top bar). It
  lasts until switched back or the app restarts. **Clear** empties the session list and totals
  and forgets the last scan (table memory, Undo); cards already saved stay in the collection.
  Cards waiting for review are dropped, so then it asks for a second click.
- Table mode finds cards without an LLM (#5): by their black borders, at any position and up to
  ~25° off upright or sideways, in under a second (the LLM took ~13 s). On the 10-card desk photo
  it finds all 10 with no false boxes, including cards greyed by glare and a tilted one, on a
  busy playmat. The vision LLM is used only when it finds nothing; `[recognition] detection`
  (`auto` / `classical` / `llm`) chooses. `mtg-scan detect-dump <photo> --out boxes.png` (hidden)
  shows what it found.
- Settings page in the web UI (#40), opened with the cog at the top right:
  - **Startup:** auto-scan on or off, single-card or table mode, and the finish when
    `mtg-scan ui` starts. `--manual`, `--table` and `--finish` override them for one run,
    and the page says so.
  - **Moxfield account.** It wins over `MTG_SCAN_MOXFIELD_USER` in `.env`.
  - **Decks:** Show / Hide for each deck. Hidden decks are left off the Decks page and out
    of the deck statistics.
  - **Database:** where the collection is stored, with live status. Never passwords.
  - **Connect:** the addresses for this computer and the network, whether an access token
    is required, and (on this computer only) the sign-in link.

  Changes are saved in `[ui]` and `[decks]` in `mtg-scan.toml`, and comments and other
  settings in that file are kept. Changes are refused in `--dry-run`.
- Web UI (`mtg-scan ui`, #2): the live camera with the guide or detected-card boxes; mode, finish
  and auto-scan controls; each card shown the moment it is saved (image, set, prices, match
  confidence) with session totals; a Collection page with totals and a filter. Dark Material
  design in the luminaldata palette. Leptos + Axum, reachable from other computers on the network.
- Collection editing in the web UI (#19): each row's Edit button opens a dialog to change the
  quantity, switch finish (only finishes the printing exists in), move the row to another printing
  by pasting a Scryfall link (priced afresh, merged into a row you already own), or remove it.
- Home page (#22), the web UI's start page: large Scan / Collection / Decks buttons beside
  collection statistics (cards, printings, value, 7-day value change from PostgreSQL price
  history, breakdowns by color, type, rarity, finish and set, most valuable and recently added
  cards). Decks is a placeholder until Moxfield support (#20).
- Collection rows carry each card's colors and type line from Scryfall (not stored).
- `mtg-scan movers [--days N] [--top N]` (#6, PostgreSQL): the collection's TCGplayer and Card
  Kingdom value now versus N days ago (default 7), and the cards that gained or lost the most for
  the copies owned.
- Access token for the web UI (#17): with `MTG_SCAN_UI_TOKEN` in `.env` (`mtg-scan ui-token` creates
  one), other computers get a sign-in page (or use a `?token=` link once; a cookie remembers it);
  this computer never needs it. `--bind 127.0.0.1` keeps the UI on this computer only.
- Review uncertain identifications (#8): in the web UI, a card whose printing is uncertain (amber)
  is no longer saved straight away; it waits under **To review** on the Scan page with Accept,
  Other printing (every printing as thumbnails, plus the finish) and Discard.
  `[recognition] review_uncertain = false` saves them directly as before; the desktop window and
  `scan-all --once` always do.
- Collection page type, rarity and color (#23): three new columns (full type line and color
  letters on hover); filter chips for each, combinable (any value within a column, every column
  with picks); group by type, rarity or color with per-group count and value; the search also
  matches type line, rarity and color; totals follow the filter. Filters and grouping are kept
  when switching tabs.
- Phone layout for the web UI (#16): at 360-430 px wide every page fits the screen (two-row top
  bar, one column, thumb-sized controls under the camera; wide tables scroll inside their card).
- Card image on hover (#29): hovering a card name on the Collection, Deck and Home pages shows the
  card at a readable size next to the pointer (kept inside the window) for as long as the pointer
  stays on the name; images load only on hover, and touch taps just open the link.
- Collapsible groups on the Collection page (#28): click a group header to hide or show its rows
  (count and value stay in the header); Expand all / Collapse all; each grouping remembers its
  collapsed groups while switching tabs.
- Add a card by hand on the Collection page (#26): **Add card** opens a dialog for a Scryfall link
  (card page, localized link or ID) with quantity and finish (only finishes the printing exists
  in). The card is priced and added, merging into the row you already own.
- Moxfield decks (#20): set `MTG_SCAN_MOXFIELD_USER` in `.env` to a Moxfield user name or profile
  link. The Decks page lists that user's public decks with how much of each is already in the
  collection (any printing counts) and the TCGplayer cost to finish it; open a deck for each card's
  status (owned, other printing, partly owned, missing), deck value and cost to finish (TCGplayer
  and Card Kingdom, Moxfield's prices). The Home page shows deck count, color identities, formats
  and the decks cheapest to finish. Decks are cached and refetched only when changed on Moxfield.
- Single-card / table mode switch while running (`M` key in the desktop window, toggle in the UI).
- `[camera] table_width` / `table_height` settings for table mode (default 3840x2160).

### Changed
- The color breakdown on the Home page (cards and decks) shows each color's symbol, count and
  percentage right under its part of the bar instead of in a separate legend (#47). Colors with
  none are left out.
- Colors are shown as MTG mana symbols (Scryfall's symbol SVGs) instead of a colored dot and the
  color's name (#39): the Collection color column, filter chips and group headers, the Home color
  breakdown and deck colors. Multicolor is a gold coin; each symbol is named on hover.
- The web UI is now "The Shuffling Tome" (#43): a new icon (an open book with three cards fanning
  out of it) and wordmark replace the "MTG scan" text logo, and the icon is the browser-tab
  favicon. The CLI keeps the name `mtg-scan`.
- The camera loop, auto-scan and scan worker are now a shared scanning engine that the desktop
  window and the web UI both drive.
- Table detection is more robust on messy scenes: capped model output, recovery of boxes from a
  truncated reply, a card-shape filter, and skipping a second overlapping detection of the same card.
- The Markdown store makes each change under an exclusive lock (`collection.md.lock`) and
  re-reads the file first, so the scanner, the web UI and other mtg-scan commands never
  overwrite each other's changes.

### Fixed
- The Decks page missed decks Moxfield marks as not legal in their format (#45), such as 101-card
  Commander decks, often a third of an account's decks. Private and unlisted decks still aren't
  listed: Moxfield shows them only to the signed-in owner. Deck lists saved by older builds are
  ignored, so the missing decks show up straight after updating.
- Empty session totals showed "$-0.00" (an empty sum is negative zero); they now show "$0.00".
- "Retry with LLM" now replaces the card the previous single-card scan added instead of adding a
  second copy. The original is removed only once the retry has identified the card, and a retry
  never removes cards from a table pass.
- The startup header showed `collection.md` even when saving to PostgreSQL; it now shows the real location.
- An unknown `/api/...` request now gets a "not found" message saying the UI and server may come
  from different builds, instead of the web page (which the UI reported as "expected value at
  line 1 column 1").

## [0.2.0] - 2026-10-20

Iteration 2: PostgreSQL storage, plus the development foundation.

### Added
- PostgreSQL backend: `collection` (one row per Scryfall printing + finish) and daily
  `price_history` (`price_tcg`, `price_ck`, `price_ck_buylist`); card details stay on Scryfall.
- `collection_value` view with each row's latest prices and value.
- Commands: `history <card>`, `db migrate | status | import | export | setup-app-user`,
  and the `--storage` flag.
- Least-privilege database login (`db setup-app-user`): the app reads and writes rows only;
  schema changes use the owner account (`MTG_SCAN_DB_ADMIN_*`).
- Background price refresh: `scan` / `scan-all` reprice cards not yet priced today while
  idle, so price history builds itself without any OS scheduler.
- `.env` file for the database connection and API keys (`.env.example` template).
- CI (format, lint, unit and PostgreSQL integration tests), tag-driven releases,
  Dependabot, issue and PR templates, CONTRIBUTING.md.

### Changed
- Migrations are no longer applied implicitly on connect; run `mtg-scan db migrate`.
- `db status` shows the server version and the logged-in account.

### Fixed
- `.env` values are read literally; a `$` in a password no longer gets mangled.

## [0.1.0] - 2026-10-20

Iteration 1: scanning, recognition, pricing and a Markdown collection.

### Added
- Live webcam scanning (Insta360 Link 2 Pro, 4K MJPEG) with hands-free auto-scan:
  `scan` (one card in a guide) and `scan-all` (every card on the table).
- Recognition: Windows OCR first, vision-LLM fallback (Ollama, LM Studio, OpenRouter,
  OpenAI-compatible), Scryfall name correction and visual printing match.
- Prices from TCGplayer (market) and Card Kingdom (retail + buylist).
- `collection.md` Markdown collection with totals; `list`, `add`, `remove`, `price`, `reprice`.

[Unreleased]: https://github.com/johnflarson/scan-capture-card/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/johnflarson/scan-capture-card/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/johnflarson/scan-capture-card/releases/tag/v0.1.0
