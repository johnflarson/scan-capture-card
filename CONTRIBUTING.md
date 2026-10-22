# Development process

How work on mtg-scan goes from an idea to a release. It's deliberately light
for a solo project, and grows as the project does.

```
Idea → Issue → Triage (labels, priority) → Board: Todo → Branch → Code + tests
  → Pull request → CI green + self-review → Squash-merge → Release tag → Feedback
```

## 1. Track: GitHub Issues and the project board

Every piece of work is an issue. Use the **Feature** or **Bug** template; use
a blank issue for chores (refactors, docs, tooling).

**Labels**

| Group | Labels | Meaning |
|---|---|---|
| Type | `type:feature`, `type:bug`, `type:chore` | What kind of work |
| Priority | `priority:P0` … `priority:P3` | P0 = broken or losing data, fix now. P1 = next up. P2 = soon. P3 = someday |
| Area | `area:scanning`, `area:recognition`, `area:pricing`, `area:storage`, `area:ci`, `area:dependencies` | Part of the app |
| Status | `status:triage` | New and not yet prioritized |

**Board** ("mtg-scan" GitHub Project), with columns `Todo → In Progress → Done`.
Issues move automatically when linked pull requests open and merge.

## 2. Prioritize: weekly triage (10 minutes)

1. Go through `status:triage` issues: add a priority and an area, and remove `status:triage`.
2. Score features roughly with value against effort:
   - **High value, low effort** first.
   - **High value, high effort** gets split into smaller issues.
   - **Low value, high effort** waits or gets closed.
3. Keep at most about 3 issues **In Progress**. Finishing beats starting.

## Two tracks: `main` (0.x) and `v1` (1.0)

- `main` is the personal 0.x line. Its last planned release is v0.3.0; it gets
  fixes only.
- `v1` is The Shuffling Tome 1.0 for friends and self-hosters
  ([plan](docs/v1-plan.md), milestone `1.0.0-alpha.1`, label `track:v1`).
  Branch 1.0 work from `v1` (`git switch v1; git pull; git switch -c ...`) and
  open PRs against it (`--base v1`). It has the same protection and CI as `main`.
- 1.0 versions are pre-releases until 1.0.0: tag `v1.0.0-alpha.N` on `v1`; the
  release workflow publishes them as GitHub pre-releases.

## 3. Build: branch, code, test

```powershell
git switch main; git pull
git switch -c feature/42-foil-detection      # feature/, fix/, chore/ + issue number
# ...code...
cargo fmt; cargo clippy --all-targets -- -D warnings; cargo test
```

- **Small changes.** One issue per branch, merged within a few days.
- **Tests.** Every bug fix gets a test that would have caught it, and every
  feature gets tests for its logic.
- **CHANGELOG.** Add a line under **Unreleased** in `CHANGELOG.md`.
- **Database changes** only go in a *new* numbered file in `migrations/`,
  e.g. `0002_add_condition.sql`. Never edit a migration that's already been
  applied. Ask first whether the data is already on Scryfall; only store what
  isn't.

### Web UI

The UI (`ui/`) is Leptos compiled to WebAssembly. It's built separately with
Trunk, and `cargo build` embeds the result. For live reload while editing it,
see [ui/README.md](ui/README.md):
- run `mtg-scan ui --no-open --dry-run` in one terminal
- run `trunk serve --open` in `ui/` in another

Server and UI share their API types through `shared/`. Change a type there
and both sides must be updated, or the build fails.

### Testing layers

| Layer | What | When |
|---|---|---|
| Unit | Parsing, heuristics, pure logic (`cargo test`) | Every PR (CI) |
| UI build | `cargo clippy -p mtg-scan-ui --target wasm32-unknown-unknown`, `trunk build` | Every PR (CI) |
| Integration | Real PostgreSQL round trip (`cargo test -- --ignored`) | Every PR (CI) |
| Manual / end-to-end | Live camera scan, the real database | Before each release |

To run the integration test locally, start a throwaway database:

```powershell
docker run -d --name mtg-scan-test-pg -e POSTGRES_USER=mtg -e POSTGRES_PASSWORD=mtgtest -e POSTGRES_DB=mtg -p 55432:5432 postgres:18
$env:MTG_SCAN_TEST_DATABASE_URL="postgres://mtg:mtgtest@localhost:55432/mtg"; cargo test -- --ignored
```

## 4. Review: pull request and CI

1. Push the branch and open a PR. Put `Closes #42` in the description so
   merging closes the issue.
2. CI runs automatically (`.github/workflows/ci.yml`):
   - `cargo fmt --check`
   - clippy, with warnings treated as errors
   - unit tests
   - the PostgreSQL integration test
3. Read your own diff top to bottom before merging, and fill in the PR checklist.
4. **Squash-merge** once CI is green, so the whole PR becomes one commit on `main`.
5. The branch deletes itself.

`main` and `v1` are protected: no direct pushes, and CI must pass. So both always build.

## 5. Release

Versions follow [semantic versioning](https://semver.org/):
- **PATCH** (0.2.1) for fixes.
- **MINOR** (0.3.0) for features.
- **MAJOR** (1.0.0) for breaking changes, such as a config or schema change
  that needs manual steps.

1. On a branch: rename `## [Unreleased]` in `CHANGELOG.md` to
   `## [0.3.0] - YYYY-MM-DD` and add a fresh `## [Unreleased]` above it. Set
   `version = "0.3.0"` in `Cargo.toml`, run `cargo build`, then PR and merge.
2. Tag the merged commit:
   ```powershell
   git switch main; git pull
   git tag v0.3.0; git push origin v0.3.0
   ```
3. `.github/workflows/release.yml` tests, builds and zips the Windows binary,
   then publishes a GitHub Release using that version's CHANGELOG section as notes.
4. **Deploy locally.** If the release has a migration, run `mtg-scan db migrate`,
   which uses the admin account. Then use the new build.

**Hotfix.** For a P0 bug: branch `fix/...` from `main`, fix it, PR, merge,
then release a PATCH version straight away.

## 6. Operate

- **Price refresh.** The app reprices cards not yet priced today in the
  background whenever the live scanner runs. It doesn't depend on any OS
  feature such as Task Scheduler or cron. `mtg-scan reprice` forces a full
  refresh.
- **Database accounts.** The app logs in as the least-privilege role
  (`MTG_SCAN_DB_USER`). Migrations and role changes use the schema owner
  (`MTG_SCAN_DB_ADMIN_USER`). Set this up with `mtg-scan db setup-app-user`.
- **Secrets** live only in `.env`, which is git-ignored. Never in code, issues
  or PRs.
- **Dependencies.** Dependabot opens weekly update PRs. Merge them when CI is
  green.

## Definition of done

An issue is done when:
- Its acceptance criteria are met.
- Tests cover the logic, and CI is green.
- Docs and `CHANGELOG.md` are updated.
- It's merged to `main`.
