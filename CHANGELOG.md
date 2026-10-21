# Changelog

All notable changes to mtg-scan. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/): MAJOR for breaking changes, MINOR
for new features, PATCH for fixes.

Every pull request adds a line under **Unreleased**. A release renames that
section to the new version (see CONTRIBUTING.md, "Releasing").

## [Unreleased]

### Added
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
- Single-card / table mode switch while running (`M` key in the desktop window, toggle in the UI).
- `[camera] table_width` / `table_height` settings for table mode (default 3840x2160).

### Changed
- The camera loop, auto-scan and scan worker are now a shared scanning engine that the desktop
  window and the web UI both drive.
- Table detection is more robust on messy scenes: capped model output, recovery of boxes from a
  truncated reply, a card-shape filter, and skipping a second overlapping detection of the same card.
- The Markdown store makes each change under an exclusive lock (`collection.md.lock`) and
  re-reads the file first, so the scanner, the web UI and other mtg-scan commands never
  overwrite each other's changes.

### Fixed
- "Retry with LLM" now replaces the card the previous single-card scan added instead of adding a
  second copy. The original is removed only once the retry has identified the card, and a retry
  never removes cards from a table pass.
- The startup header showed `collection.md` even when saving to PostgreSQL; it now shows the real location.

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
