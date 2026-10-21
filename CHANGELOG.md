# Changelog

All notable changes to mtg-scan. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/): MAJOR for breaking changes, MINOR
for new features, PATCH for fixes.

Every pull request adds a line under **Unreleased**. A release renames that
section to the new version (see CONTRIBUTING.md, "Releasing").

## [Unreleased]

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
- `scripts/nightly.ps1` + `scripts/install-nightly-task.ps1`: a nightly `reprice` via
  Windows Task Scheduler, so price history builds itself.
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
