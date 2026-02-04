# Task log: upstream sync (`upstream/main` → `main` → `master`) — 2026-02-03

Sync date: 2026-02-03
Log written (UTC-3): 2026-02-04T14:27:17-03:00

## Goal

- Finalize the `upstream/main` → `master` sync (keep `main` mirrored), keep tests green, and push `origin/master`.

## Refs (starting point)

- `main == upstream/main == origin/main`: `33dc93e4d291` (already mirrored; no-op in this run)
- `origin/master`: `39077a9e5f32` (behind local `master`)
- `master` base merge commit: `1b523ecc7` (main -> master (upstream sync 2026-02-03))

## Landing strategy

- Kept `master` merge-based (no rebase): the `main -> master` merge was already present (`1b523ecc7`).
- Landed the prepared WIP fixups onto `master` via `git cherry-pick -x` (edited the first message to remove “WIP”).

## New commits on `master` (this run)

- `a1eb7317e` chore(sync): upstream sync snapshot (2026-02-03)
- `4e0947af7` fix(core): prevent auth reload switching accounts
- `df6ae6ec1` fix(core): stabilize parallel tools and unified_exec
- `3d6d15f43` fix(tui): adapt auth/account UI after sync
- `af681c065` docs(.sangoi): add prompt-to-self resume capsule
- `18ed23ea7` chore: fix AGENTS.md newline
- `f732725b9` fix(state): persist dynamic tools atomically (fixes a cross-test race in `persist_dynamic_tools`)

## Commands (high-signal)

- Land WIP onto `master`:
  - `git switch master`
  - `git pull --ff-only origin master`
  - `git cherry-pick -x 50b039118` (then `git commit --amend ...` to drop “WIP”)
  - `git cherry-pick -x 8bcfe8728 d2e074d8a 686040763 bd51e9795`
- Follow-up hygiene / fixes:
  - `git add AGENTS.md && git commit -m "chore: fix AGENTS.md newline"`
  - (after a flaky `sqlite_state` backfill under load) make dynamic tools persistence atomic:
    - edit `codex-rs/state/src/runtime.rs` (`persist_dynamic_tools` uses a transaction)
    - `git add codex-rs/state/src/runtime.rs && git commit -m "fix(state): persist dynamic tools atomically"`
- Format + tests:
  - `cd codex-rs && just fmt`
  - `cd codex-rs && cargo test -p codex-core --all-features -- --quiet`
  - `cd codex-rs && cargo test -p codex-state -- --quiet`
  - `cd codex-rs && cargo test -p codex-tui -- --quiet`
  - `cd codex-rs && cargo test -p codex-login -- --quiet`
  - `cd codex-rs && just fix -p codex-state`

## Outcome

- `master` is clean (`git status --porcelain=v1` empty).
- `main` remains a mirror of `upstream/main`.
- TUI snapshots: no updates required (no `*.snap.new` files generated).
- Pushed + verified:
  - `git push origin master`
  - `git ls-remote origin refs/heads/master` == `git rev-parse HEAD`
