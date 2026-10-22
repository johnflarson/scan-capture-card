# The Shuffling Tome 1.0: plan

1.0 turns mtg-scan, a personal Windows tool, into an app that friends and other
self-hosters can run on their own hardware. The work happens on the `v1` branch.
`main` stays the personal 0.x line; v0.3.0 is its last planned release.

Progress is tracked in the GitHub milestone **1.0.0-alpha.1** (issues labelled
`track:v1`). Each phase ends with a milestone test by the owner.

## Decisions

| Topic | Decision |
|---|---|
| Audience | Friends and self-hosters (alpha). A public release stays possible; no hosted service. |
| License | AGPL-3.0-or-later, plus a section 7(b) term that keeps the author credit visible in the UI (see `LICENSE-ADDITIONAL-TERMS.md`). 0.x releases stay MIT. |
| Prices | Scryfall only: TCGplayer USD, Cardmarket EUR, MTGO tix from Scryfall's daily bulk data. The direct TCGplayer lookup is removed. Card Kingdom is an optional add-on, off by default. |
| Moxfield | Kept as is (opt-in, rate-limited) while the owner asks Moxfield for permission. Pasted lists and CSV are the deck import that works for everyone. |
| Platforms | Docker / Linux first, then Windows, then macOS. |
| Camera | Overhead cameras only (no hand-held phone scanning). |
| Recognition | Works on any hardware: OCR first; local vision LLM when the hardware can run it; cloud LLMs with the user's own key. |
| Accounts | Real accounts: the first user is the admin, others join by invite link; the admin resets passwords. No public sign-up. |
| Settings | Currency and card language are user settings. |
| Telemetry | Opt-in at setup, anonymous, sent to a collector on the owner's VPS. Never card lists, images or keys. |

## Phases

### 0. Groundwork
- `v1` branch with the same protection as `main`; version `1.0.0-alpha.0`; pre-release tags publish as GitHub pre-releases.
- AGPL-3.0 license, the attribution term, an About line in the UI (credit, version, license, source link), the Wizards Fan Content disclaimer and Scryfall attribution.

### 1. Data foundation
- Scryfall bulk data (`default_cards`) cached in the database: card details and daily prices come from it, so listings no longer call the Scryfall API.
- Prices from Scryfall only; Card Kingdom as an optional source.
- SQLite as the default store, PostgreSQL as an option; one test suite runs against both.
- Schema v2: card cache, price history v2 (USD, EUR, tix, optional CK); import from 0.x (PostgreSQL migrates in place with `db migrate`; Markdown with `db import`); copy between SQLite and PostgreSQL (`db copy`). Condition and language per collection row moved to Phase 4, where the collection's key changes for accounts anyway.
- Settings: currency and card language.

### 2. Runs on Linux and in Docker
- Windows-only code behind build switches; the desktop window becomes optional; rustls; data, config and cache folders configurable; the UI starts without a camera.
- OCR behind an interface: Windows OCR on Windows, PaddleOCR (ONNX) on Linux and in Docker.
- Linux CI, a Docker image published to GHCR on `v1` tags, and a compose file with optional PostgreSQL and Ollama.

### 3. Overhead camera everywhere
- Frame sources: a camera attached to the server (native, or Docker on Linux with `--device /dev/video0`) and a camera attached to the browser's computer (covers Docker Desktop on Windows and macOS; `http://localhost` needs no certificate).
- Guide and detected-card boxes drawn in the page.

### 4. Accounts
- Users, sessions (argon2id passwords), admin, invite links, API tokens.
- Collections, settings, hidden decks, Moxfield user and LLM keys per user (keys encrypted at rest); condition and language per collection row.
- A scan session per user, sharing one recognition queue; no "same computer skips sign-in" behind a reverse proxy.

### 5. Setup wizard and providers
- First-run wizard: admin account, storage, recognition (OCR only / local Ollama with hardware check, benchmark and model download / cloud key), camera test, currency and language, telemetry opt-in.
- Native Anthropic and Gemini adapters next to the OpenAI-compatible one.

### 6. Telemetry and metrics
- Opt-in anonymous pings: version, platform, install type, setup choices, rough scan counts, error categories.
- Collector service on the owner's VPS; a metrics view combining telemetry with GitHub stars and traffic, image and release downloads, and sponsors.

### 7. Alpha release
- CSV import and export (ManaBox, Moxfield, Deckbox, Archidekt).
- Install guide, security review (`cargo audit`, CSRF), `1.0.0-alpha.1`.
- Backup and restore for other users: designed with the owner, not before.

### Later
- Image matching against Scryfall card images, so the LLM becomes optional.
- Signed Windows installer; macOS.
