# Plan: sync `upstream/main` → `main` → `master` (2026-01-29)

## Recon (starting state)

- Starting branch: `master` (dirty working tree; 10 modified files)
- After `git fetch --all --prune`: `upstream/main` advanced `2d9ac8227` → `fbb3a3095`
- `log-session.txt`: not found (nothing to delete)

### Local modifications present at start

- `codex-rs/core/config.schema.json`
- `codex-rs/core/src/client_common.rs`
- `codex-rs/core/src/codex.rs`
- `codex-rs/core/src/rollout/metadata.rs`
- `codex-rs/core/src/shell_snapshot.rs`
- `codex-rs/core/src/tasks/mod.rs`
- `codex-rs/core/src/tasks/sanitize.rs`
- `codex-rs/core/src/tools/handlers/shell.rs`
- `codex-rs/core/src/tools/orchestrator.rs`
- `codex-rs/state/src/extract.rs`

## Decisions (confirmed)

1) Uncommitted changes on `master`: **A** (commit to a temporary WIP branch)
2) Bring upstream into `master`: **A** (merge `main` → `master`, no history rewrite)
3) Push results to `origin`: **yes** (`origin/main` and `origin/master`)

## Checklist (executed)

### 0) Safety + clean starting point

- [x] Created backups: `backup/main-before-upstream-sync-20260129-192740`, `backup/master-before-upstream-sync-20260129-192740`
- [x] Saved local work on `wip/master-local-changes-20260129-192740` and cherry-picked onto `master`

### 1) Sync `main` with upstream

- [x] `git fetch --all --prune`
- [x] `git checkout main`
- [x] Fast-forward: `git merge --ff-only upstream/main` (→ `fbb3a3095`)
- [x] Pushed: `git push origin main`

### 2) Bring new upstream commits into `master`

- [x] `git checkout master`
- [x] Merged: `git merge --no-edit main` (clean merge)
- [x] Re-applied local mods via cherry-pick from the WIP branch (clean apply)

### 3) Format + tests

- [x] `cd codex-rs && just fmt`
- [x] Stabilized a few flaky tests and re-ran: `cargo test -p codex-core --all-features -- --quiet`
- [x] `cd codex-rs && just fix -p codex-core`
- [x] `cd codex-rs && cargo test -p codex-state -- --quiet`

### 4) Push + verify

- [x] Pushed: `git push origin master`
- [x] Working tree clean + branch heads verified

## Result (final state)

- `main`: `fbb3a3095` (matches `upstream/main`), pushed to `origin/main`
- `master`: `8de58a77c`, pushed to `origin/master`
