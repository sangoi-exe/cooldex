# Prompt to self — clean-session resume (upstream sync → push `origin/master`)
Date: 2026-02-04
Owner: lucas
Status: Active

```text
You are working in the repo `codex`.
Continue from:
- CWD: /home/lucas/work/codex
- Branch: wip/upstream-sync-20260203-20260203-220801
- Last commit (code): 6860407639a7937e5ee8bfd8010fcaac43bdb42f
- Date (UTC-3): 2026-02-04T13:30:37-03:00

Objective (1 sentence)
- Finish syncing upstream into `master`, keep tests green, write a task-log, then push `origin/master` (keeping `main` identical to `upstream/main`).

State
- Done:
  - `main == upstream/main == origin/main == 33dc93e4d2913ba940213ede693b84ebaf80b3f6`.
  - `master` merged `main` via merge commit `1b523ecc750ad132378b273ed8333c68e2b483be` (not pushed; `origin/master` is still `39077a9e5f327a81c4846e0914f672af4659896c`).
  - Work captured on `wip/upstream-sync-20260203-20260203-220801` (working tree clean) with commits:
    - `50b039118` (snapshot + upstream-sync fixups + new `.sangoi` docs/templates/plan)
    - `8bcfe8728` (auth refresh: don’t switch cached account on reload mismatch)
    - `d2e074d8a` (tool parallelism runtime locking + unified_exec stabilization + test determinism)
    - `686040763` (TUI auth/account UI adjustments)
  - This resume capsule lives at `.sangoi/task-logs/prompt-to-self-2026-02-04-upstream-sync-resume.md` (HEAD includes a docs commit for it; run `git log -n 3`).
  - Tests/format/clippy already run on the WIP branch:
    - `cd codex-rs && just fmt`
    - `cd codex-rs && cargo test -p codex-core --all-features -- --quiet`
    - `cd codex-rs && cargo test -p codex-state -- --quiet`
    - `cd codex-rs && cargo test -p codex-tui -- --quiet`
    - `cd codex-rs && just fix -p codex-core`
    - `cd codex-rs && just fix -p codex-tui`
    - `cd codex-rs && just fix -p codex-login`
- In progress:
  - Write a short sync task-log in `.sangoi/task-logs/` (refs + commands + outcome).
  - Land the WIP branch commits onto `master` with a clean history, then push `origin/master`.
- Blocked / risks:
  - Repo rules: no stash, no `git clean`, no `git checkout -- <path>`, no `git restore`, no `git add -A`.
  - Tool policy note: `git reset ...` was previously rejected by policy in this environment; avoid relying on reset-based commit splitting.
  - Branch protection may block force-push to `origin/main` (but `origin/main` already matches `upstream/main` right now).

Decisions / constraints (locked)
- `main` must remain identical to `upstream/main` (mirror).
- `master` must be updated via merge from `main` (never rebase master).
- Keep `.sangoi/` as the canonical home for plans/runbooks/task-logs.
- Rust rules apply: don’t touch `CODEX_SANDBOX_*` env var logic; run `cd codex-rs && just fmt`; run targeted tests; run scoped `just fix -p ...`; avoid workspace-wide `cargo test --all-features` unless explicitly requested.
- Commit hygiene: avoid `git add -A`; prefer explicit file lists / `git add -p`.

Follow-up (ordered)
1. Create `.sangoi/task-logs/upstream-sync-2026-02-03.md` with refs + commands + outcome; commit it as a docs commit.
2. Land commits onto `master` (pick one strategy; prefer clean messages):
   - Recommended: cherry-pick the WIP commits onto `master` and edit the first commit message to remove “WIP”.
   - Fallback: `git merge --squash` the WIP branch into `master` and create a single clean commit message.
3. Re-run the key validations on `master` after landing (fmt + `codex-core` tests at minimum).
4. Push `origin/master` and confirm `origin/master` points to the new HEAD.

Next immediate step (do this first)
- Decide and execute the landing strategy onto `master` (recommended: cherry-pick with edited message for the first commit).
Commands:
git rev-parse --abbrev-ref HEAD
git rev-parse master origin/master
git log --oneline -n 8

# Option A (recommended): cherry-pick onto master (cleaner history than merging the WIP branch)
git switch master
git pull --ff-only origin master
git cherry-pick -x 50b039118
# Edit the message here if needed (remove “WIP”, summarize scope), then continue:
git cherry-pick -x 8bcfe8728 d2e074d8a 686040763

# Option B (fallback): squash merge (one big commit with a clean message)
# git switch master
# git pull --ff-only origin master
# git merge --squash wip/upstream-sync-20260203-20260203-220801
# git commit -m "chore(sync): upstream/main -> master (2026-02-03) + fixups"

Files
- Changed files (last relevant commit(s)):
  - From `50b039118`:
    - `.sangoi/howto/PROMPT_GUIDE.md`
    - `.sangoi/plans/plan-upstream-sync-2026-02-03.md`
    - `.sangoi/task-logs/prompt-to-self-2026-02-03-upstream-sync.md`
    - `.sangoi/templates/PROMPT_TO_SELF_TEMPLATE.md`
    - `AGENTS.md`
    - `codex-rs/core/src/auth.rs`
    - `codex-rs/core/src/auth/storage.rs`
    - `codex-rs/core/src/codex.rs`
    - `codex-rs/core/src/context_manager/history.rs`
    - `codex-rs/core/src/context_manager/history_tests.rs`
    - `codex-rs/core/src/context_manager/normalize.rs`
    - `codex-rs/core/src/shell_snapshot.rs`
    - `codex-rs/core/src/state/session.rs`
    - `codex-rs/core/src/tasks/mod.rs`
    - `codex-rs/core/src/tasks/sanitize.rs`
    - `codex-rs/core/src/tools/handlers/manage_context.rs`
    - `codex-rs/core/tests/suite/auth_refresh.rs`
    - `codex-rs/state/src/runtime.rs`
    - `codex-rs/tui/src/chatwidget/tests.rs`
  - From `d2e074d8a`:
    - `codex-rs/core/src/tools/context.rs`
    - `codex-rs/core/src/tools/parallel.rs`
    - `codex-rs/core/src/tools/router.rs`
    - `codex-rs/core/src/unified_exec/process_manager.rs`
    - `codex-rs/core/tests/suite/unified_exec.rs`
  - From `686040763`:
    - `codex-rs/login/src/server.rs`
    - `codex-rs/tui/src/app.rs`
    - `codex-rs/tui/src/chatwidget.rs`
    - `codex-rs/tui/src/slash_command.rs`
    - `codex-rs/tui/src/status/helpers.rs`
- Focus files to open first:
  - `codex-rs/core/src/auth.rs` — refresh/reload mismatch handling + cached token patching.
  - `codex-rs/core/src/tools/parallel.rs` + `codex-rs/core/src/tools/router.rs` — runtime lock decision using `is_mutating()`.
  - `codex-rs/core/src/unified_exec/process_manager.rs` + `codex-rs/core/tests/suite/unified_exec.rs` — exit metadata flake + deterministic test args.
  - `codex-rs/tui/src/chatwidget.rs` + `codex-rs/tui/src/app.rs` + `codex-rs/tui/src/status/helpers.rs` — auth mode checks + account label/email display.
  - `.sangoi/plans/plan-upstream-sync-2026-02-03.md` — execution checklist / DONE criteria.

Validation (what “green” looks like)
- `cd codex-rs && just fmt`  # expected: exit 0
- `cd codex-rs && cargo test -p codex-core --all-features -- --quiet`  # expected: all pass
- `cd codex-rs && cargo test -p codex-state -- --quiet`  # expected: all pass
- `cd codex-rs && cargo test -p codex-tui -- --quiet`  # expected: all pass (no pending insta snapshots)
- `cd codex-rs && just fix -p codex-core`  # expected: exit 0, no clippy warnings
- `git status --porcelain=v1`  # expected: empty before pushing
- `git rev-parse master origin/master`  # expected: different before push, equal after push

References (read before coding)
- `.sangoi/docs/guia-sync-upstream-main-master.md`
- `.sangoi/plans/plan-upstream-sync-2026-02-03.md`
- `.sangoi/plans/plan-guia-sync-upstream-main-master.md`
- `.sangoi/task-logs/prompt-to-self-2026-02-03-upstream-sync.md`
- `.sangoi/howto/PROMPT_GUIDE.md`
- `.sangoi/templates/PROMPT_TO_SELF_TEMPLATE.md`
```
