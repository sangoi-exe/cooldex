# Plan — `/accounts` reapply finalization + upstream reconciliation (2026-02-12)

Label: **hard**

## Intent check
- Finalize current `/accounts` reapply + guide updates with fail-loud validation and coherent commit split.
- Include the new sync request (`main` then `master`) without rebase/overwrite recovery and without losing local mods.

## Chosen execution path (after evaluating alternatives)
- Manual, contract-preserving reconciliation only (merge/cherry-pick/manual resolution as explicitly approved).
- Preserve current work first via safety snapshots, then sync/reconcile, then split commits intentionally.
- Enforce fail-loud gates (`passed > 0`) and finish with reviewer audit before handoff.

## Fan-out lanes (fan-out → fan-in)
- **Current decision:** keep single lane until sync scope is explicit.
- **Reason:** branch/repo ambiguity (`main` → `master`) and shared git index make early fan-out unsafe.
- **Fan-in gate:** final diff audit + reviewer gate + command/result handoff.

## Checklist
- [ ] 0) Scope lock (blocker)
  - Done when: user confirms repo/branch scope for `main` → `master`, integration method (merge/cherry-pick/ff-only), commit target per repo, and conflict precedence if upstream diverges from locked `/accounts` contracts.
  - Verification: explicit user confirmation before any `fetch/pull/merge/cherry-pick`.

- [ ] 1) Recon + topology proof in both repos
  - Done when: staged/unstaged/untracked scope, tracking, and divergence are explicit for `codex` and `.sangoi`.
  - Verification:
    - `git status --short`
    - `git diff --cached --name-only`
    - `git branch -vv`
    - `git remote -v`
    - `git rev-list --left-right --count HEAD...origin/main || true`
    - `git rev-list --left-right --count HEAD...origin/master || true`
    - `git -C .sangoi rev-parse --show-toplevel`
    - `git -C .sangoi status -sb`
    - `git -C .sangoi diff --cached --name-only`
    - `git -C .sangoi branch -vv`
    - `git -C .sangoi remote -v`
    - `git -C .sangoi rev-list --left-right --count HEAD...origin/master || true`

- [ ] 2) Lock reconciliation ordering and commit buckets
  - Done when: ordering is explicit (`commit-first` vs `sync-first`) and every changed file is mapped to exactly one commit bucket (`codex code`, `codex docs`, `.sangoi docs/plans` as approved).
  - Verification: written mapping list with zero overlap and explicit inclusion/exclusion for `.sangoi/local/*` + `.sangoi/self-prompts/*`.

- [ ] 3) Capture safety snapshots before sync/reconcile
  - Done when: staged and unstaged patches are saved for both repos to a timestamped path.
  - Verification:
    - `ts=$(date +%Y%m%d-%H%M%S); d=/tmp/reapply-safety-$ts; mkdir -p "$d"`
    - `git diff > "$d/codex.unstaged.patch"`
    - `git diff --cached > "$d/codex.staged.patch"`
    - `git -C .sangoi diff > "$d/sangoi.unstaged.patch" || true`
    - `git -C .sangoi diff --cached > "$d/sangoi.staged.patch" || true`
    - `ls -lh "$d"`

- [ ] 4) Run baseline validation battery (fail-loud)
  - Done when: every focused command exits 0 and filtered tests report `passed >= 1` (never `0 passed`).
  - Verification:
    - `cd codex-rs && just fmt`
    - `CARGO_TERM_COLOR=never cargo test -p codex-login --lib persist_tokens_async_ -- --quiet | tee /tmp/validate-login.log && rg -q 'test result: ok\\..*[1-9][0-9]* passed' /tmp/validate-login.log`
    - `CARGO_TERM_COLOR=never cargo test -p codex-core auth_refresh -- --quiet | tee /tmp/validate-core.log && rg -q 'test result: ok\\..*[1-9][0-9]* passed' /tmp/validate-core.log`
    - `CARGO_TERM_COLOR=never cargo test -p codex-tui accounts_popup -- --quiet | tee /tmp/validate-tui-accounts.log && rg -q 'test result: ok\\..*[1-9][0-9]* passed' /tmp/validate-tui-accounts.log`
    - `CARGO_TERM_COLOR=never cargo test -p codex-tui logout_popup -- --quiet | tee /tmp/validate-tui-logout.log && rg -q 'test result: ok\\..*[1-9][0-9]* passed' /tmp/validate-tui-logout.log`
    - `CARGO_TERM_COLOR=never cargo test -p codex-tui slash_command -- --quiet | tee /tmp/validate-tui-slash.log && rg -q 'test result: ok\\..*[1-9][0-9]* passed' /tmp/validate-tui-slash.log`
    - `CARGO_TERM_COLOR=never cargo test -p codex-exec completed_status_message -- --quiet | tee /tmp/validate-exec.log && rg -q 'test result: ok\\..*[1-9][0-9]* passed' /tmp/validate-exec.log`

- [ ] 5) Reconcile upstream updates with local mods (fail-loud)
  - Done when: requested upstream SHAs are imported per confirmed strategy and conflicts are resolved by contract (no compat shims/workarounds).
  - Verification:
    - `git fetch --all --prune`
    - `git -C .sangoi fetch --all --prune` (if `.sangoi` is in confirmed sync scope)
    - divergence checks with explicit refs agreed in scope lock (no placeholders)
    - targeted file contract audit:
      - `codex-rs/login/src/server.rs`
      - `codex-rs/tui/src/app_event.rs`
      - `codex-rs/tui/src/app.rs`
      - `codex-rs/tui/src/chatwidget.rs`
      - `codex-rs/tui/src/slash_command.rs`
      - `codex-rs/exec/src/event_processor_with_human_output.rs`

- [ ] 6) Clean and split staged scope coherently
  - Done when: commit groups are atomic/non-overlapping, `.sangoi/local/*` and `.sangoi/self-prompts/*` are excluded, and no accidental files remain.
  - Verification:
    - `git diff --cached --name-only`
    - `git -C .sangoi diff --cached --name-only`
    - `git status --short`
    - `git -C .sangoi status --short`

- [ ] 7) Final validation, reviewer gate, and handoff
  - Done when: post-reconcile validations are green, reviewer returns `READY` or `READY_WITH_NITS` (no blocker/high unresolved), and handoff includes local SHAs + imported upstream SHAs + commands + residual risks.
  - Verification:
    - re-run focused validation battery
    - `git show --stat --name-only <sha>`
    - `git diff --check`

- [ ] 8) Rollback procedure captured
  - Done when: exact restore commands for both repos are documented alongside the saved patch directory.
  - Verification:
    - `git apply --check /tmp/reapply-safety-<ts>/codex.staged.patch`
    - `git apply --check /tmp/reapply-safety-<ts>/codex.unstaged.patch`
    - `git -C .sangoi apply --check /tmp/reapply-safety-<ts>/sangoi.staged.patch`
    - `git -C .sangoi apply --check /tmp/reapply-safety-<ts>/sangoi.unstaged.patch`
