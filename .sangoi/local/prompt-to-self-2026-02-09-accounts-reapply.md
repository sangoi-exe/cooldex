# Prompt to self — resume `/accounts` reapply on clean session

```text
You are working in the repo `codex`.
Continue from:
- CWD: /tmp/codex-accounts-v2
- Branch: reapply/accounts-20260209-v2
- Last commit: 54b401aa5fb2f2a7dec3ae13ac2a93a0cbc7bb9a
- Date (UTC-3): 2026-02-09T14:22:17-03:00

Objective (1 sentence)
- Reapply the local `/accounts` feature safely on top of `upstream/main` (manual reapply, no blind sync merge), then validate before moving to the next mod.

State
- Done:
  - Remote branch cleanup completed; only `origin/master`, `origin/main`, and `origin/backup/reapply-state-20260209-125806` remain.
  - Manual-reapply docs/plan were created and committed in backup commit `e0287cbb19d6bd24706487365eed74fbf6bd7ec1`.
  - A cleaner implementation lane was opened in worktree branch `reapply/accounts-20260209-v2`.
  - In `-v2`, core `/accounts`-related files were ported and formatted:
    - codex-rs/core/src/auth.rs
    - codex-rs/core/src/auth/storage.rs
    - codex-rs/core/tests/suite/auth_refresh.rs
    - codex-rs/login/src/server.rs
- In progress:
  - `/accounts` reapply on `reapply/accounts-20260209-v2` with targeted validation.
- Blocked / risks:
  - Disk exhaustion: build/test failed with `No space left on device (os error 28)`.
  - There is an older dirtier branch `reapply/accounts-20260209` in the main worktree; do not use it as source of truth.

Decisions / constraints (locked)
- Manual reapply per mod is mandatory (no rebase/sync shortcut for this recovery).
- `/accounts` is first priority mod.
- Use branch `reapply/accounts-20260209-v2` as the active lane.
- Fail loud: test commands must fail on non-zero exit and on `0 passed`.
- Do not do wholesale file overwrite across diverged upstream; port incrementally.

Follow-up (ordered)
1. Free disk space enough to compile/test reliably.
2. Re-run targeted `/accounts` validations on `-v2`.
3. If core checks are green, port the TUI `/accounts` surface incrementally (slash command + popups + status panel + snapshots).
4. Re-run TUI targeted tests/snapshots.
5. Prepare a focused checkpoint commit for `/accounts` before starting the next mod.

Next immediate step (do this first)
- Unblock disk and rerun the first targeted test in the active worktree.
Commands:
cd /tmp/codex-accounts-v2
df -h /
du -sh codex-rs/target /home/lucas/work/codex/codex-rs/target 2>/dev/null || true
rm -rf codex-rs/target
cd codex-rs
cargo test -p codex-core auth_refresh -- --quiet

Files
- Changed files (last relevant commit):
  - .sangoi/docs/guide-reapply-accounts.md
  - .sangoi/docs/guide-reapply-manage_context.md
  - .sangoi/docs/guide-reapply-mods-order.md
  - .sangoi/docs/guide-reapply-sanitize.md
  - .sangoi/docs/guide-reapply-subagents.md
  - .sangoi/docs/mods-reapply-inventory-2026-02-09.md
  - .sangoi/plans/plan-manual-reapply-mods-2026-02-09.md
- Focus files to open first:
  - codex-rs/core/src/auth.rs — core auth mode/store behavior for `/accounts`.
  - codex-rs/core/src/auth/storage.rs — persisted multi-account model and invariants.
  - codex-rs/core/tests/suite/auth_refresh.rs — canonical core behavior checks.
  - codex-rs/login/src/server.rs — auth store save path compatibility.

Validation (what “green” looks like)
- cargo test -p codex-core auth_refresh -- --quiet
  # expected: exits 0 and has non-zero passed tests
- cargo test -p codex-tui accounts_popup -- --quiet
  # expected: exits 0 and has non-zero passed tests
- cargo test -p codex-tui logout_popup -- --quiet
  # expected: exits 0 and has non-zero passed tests
- cargo test -p codex-tui slash_command -- --quiet
  # expected: exits 0 and has non-zero passed tests
- just fmt
  # expected: no further formatting changes

Known traps / gotchas
- If `df -h /` stays near 100%, test/build failures are environmental, not necessarily code regressions.
- Do not continue on `reapply/accounts-20260209` (old broad overwrite lane); continue on `reapply/accounts-20260209-v2`.
- Reapply docs are not present on `upstream/main`; read them from backup branch using `git show`.

References (read before coding)
- backup/reapply-state-20260209-125806:.sangoi/plans/plan-manual-reapply-mods-2026-02-09.md
- backup/reapply-state-20260209-125806:.sangoi/docs/mods-reapply-inventory-2026-02-09.md
- backup/reapply-state-20260209-125806:.sangoi/docs/guide-reapply-mods-order.md
- backup/reapply-state-20260209-125806:.sangoi/docs/guide-reapply-accounts.md
- backup/reapply-state-20260209-125806:.sangoi/howto/PROMPT_GUIDE.md
- backup/reapply-state-20260209-125806:.sangoi/templates/PROMPT_TO_SELF_TEMPLATE.md
```
