## What and why

<!-- One or two sentences. Link the issue so it closes on merge: -->
Closes #

## How it was tested

<!-- Commands run, manual checks (camera, real database), screenshots. -->

## Checklist

- [ ] `cargo fmt`, `cargo clippy --all-targets -- -D warnings` and `cargo test` pass locally
- [ ] New behaviour has tests (or a note on why it can't be unit-tested)
- [ ] `CHANGELOG.md` has a line under **Unreleased**
- [ ] Docs updated where behaviour changed (README / CONTRIBUTING / CLAUDE.md)
- [ ] Schema change? New numbered file in `migrations/` (applied ones are never edited)
- [ ] No secrets committed (`.env`, passwords, API keys)
