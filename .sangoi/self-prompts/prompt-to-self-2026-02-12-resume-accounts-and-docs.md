# Prompt to self — resume `/accounts` reapply + docs handoff
Date: 2026-02-12
Status: Active

```text
You are working in the repo `codex`.

Context header
- Repo: codex
- CWD: /home/lucas/work/codex
- Branch: reapply/accounts-20260209
- Last commit: 54b401aa5fb2f2a7dec3ae13ac2a93a0cbc7bb9a
- Date (UTC-3): 2026-02-12T11:16:22-03:00
- Nested docs repo: .sangoi (branch master, head f4326540697df7432a512c45446014e780f48b79)

Objective (1 sentence)
- Finalize and hand off the `/accounts` reapply and docs-guide updates with fail-loud validation and clean commit strategy.

State
- Done:
  - `/accounts` multi-account flow reapplied across core/login/tui.
  - Sub-agent `completed` message formatting fix applied (no truncation).
  - Focused validation battery previously green (core/tui/exec/login targeted filters).
  - `.sangoi` standalone docs repo initialized and pushed to `origin/master` (`f432654`).
  - Guides updated under `.sangoi/docs` + `docs/multi-account-auth-plan.md` with implementation lessons.
- In progress:
  - Main repo has a large staged working set (code + docs) not yet committed.
- Blocked / risks:
  - Mixed scope in staged set can cause noisy commit if not split intentionally.
  - Avoid staging `.sangoi/local/*` scratch artifacts.
  - Preserve fail-loud constraints (no “green” with filtered tests = 0).

Decisions / constraints (locked)
- No sync/rebase to recover mods; manual reaplicação only.
- No full-file overwrite from backup; port by behavior/contracts.
- Fail loud always: root-cause fixes only; no compat shims/workarounds.
- `/accounts` contracts must stay cross-file aligned (`app_event`/`app`/`chatwidget`/`status`/`bottom_pane`).
- `login::persist_tokens_async` must keep upsert semantics (`update_auth_store`) and clear stale API key.
- `SlashCommand::DebugConfig` and `Clean` remain available during task; `Accounts`/`Logout` unavailable during task.
- `AgentStatus::Completed(Some(...))` must stay untruncated (trim-only).

Follow-up (ordered)
1. Reconfirm current working tree state and staged scope.
2. Re-run focused validation battery to re-prove green on current tree.
3. Decide commit split (recommended: code commit + docs commit) and ensure each is coherent.
4. Audit final diffs for drift/noise and remove any accidental files.
5. Prepare final handoff with commands/results and residual risks.

Next immediate step (do this first)
- Reconcile real state before any new edit/commit.
Commands:
cd /home/lucas/work/codex
git status --short
git diff --cached --name-only
git -C .sangoi status -sb

Files
- Changed files (current staged working set):
  - .sangoi/docs/guide-reapply-accounts.md
  - .sangoi/docs/guide-reapply-manage_context.md
  - .sangoi/docs/guide-reapply-mods-order.md
  - .sangoi/docs/guide-reapply-sanitize.md
  - .sangoi/docs/guide-reapply-subagents.md
  - .sangoi/docs/mods-reapply-inventory-2026-02-09.md
  - .sangoi/plans/plan-manual-reapply-mods-2026-02-09.md
  - .sangoi/plans/plan-subagents-wait-status-2026-02-05.md
  - .sangoi/sangoi_base_instructions.md
  - .sangoi/sangoi_subagent_instructions.md
  - codex-rs/core/src/auth.rs
  - codex-rs/core/src/auth/storage.rs
  - codex-rs/core/tests/suite/auth_refresh.rs
  - codex-rs/exec/src/event_processor_with_human_output.rs
  - codex-rs/login/src/server.rs
  - codex-rs/tui/src/app.rs
  - codex-rs/tui/src/app_event.rs
  - codex-rs/tui/src/bottom_pane/chatgpt_add_account_view.rs
  - codex-rs/tui/src/bottom_pane/mod.rs
  - codex-rs/tui/src/chatwidget.rs
  - codex-rs/tui/src/chatwidget/snapshots/codex_tui__chatwidget__tests__accounts_popup.snap
  - codex-rs/tui/src/chatwidget/snapshots/codex_tui__chatwidget__tests__logout_popup.snap
  - codex-rs/tui/src/chatwidget/tests.rs
  - codex-rs/tui/src/slash_command.rs
  - codex-rs/tui/src/status/account.rs
  - codex-rs/tui/src/status/card.rs
  - codex-rs/tui/src/status/helpers.rs
  - docs/authentication.md
  - docs/multi-account-auth-plan.md
  - docs/slash_commands.md
- Focus files to open first:
  - codex-rs/login/src/server.rs — multi-account upsert + stale API key semantics.
  - codex-rs/tui/src/app_event.rs — `/accounts` event contract.
  - codex-rs/tui/src/app.rs — handlers (`StartOpenAccountsPopup`, `LogoutAllAccounts`, add-account flow).
  - codex-rs/tui/src/chatwidget.rs — popup UX + slash dispatch + fetch helper.
  - codex-rs/tui/src/slash_command.rs — availability matrix and regression guard.
  - codex-rs/exec/src/event_processor_with_human_output.rs — `completed` no truncation behavior.
  - docs/multi-account-auth-plan.md — operational lessons section alignment.

Validation (what “green” looks like)
- cd /home/lucas/work/codex/codex-rs && just fmt
  # expected: exits 0
- cargo test -p codex-login --lib persist_tokens_async_ -- --quiet
  # expected: test result ok with passed > 0
- cargo test -p codex-core auth_refresh -- --quiet
  # expected: test result ok with passed > 0 (filtered suite)
- cargo test -p codex-tui accounts_popup -- --quiet
  # expected: test result ok with passed > 0
- cargo test -p codex-tui logout_popup -- --quiet
  # expected: test result ok with passed > 0
- cargo test -p codex-tui slash_command -- --quiet
  # expected: test result ok with passed > 0
- cargo test -p codex-exec completed_status_message -- --quiet
  # expected: test result ok with passed > 0

References (read before coding)
- .sangoi/local/plan-2026-02-11-accounts-reapply.md
- .sangoi/local/plan-2026-02-11-guides-feature-application.md
- .sangoi/docs/guide-reapply-accounts.md
- .sangoi/docs/guide-reapply-mods-order.md
- .sangoi/docs/guide-reapply-subagents.md
- .sangoi/docs/mods-reapply-inventory-2026-02-09.md
- docs/multi-account-auth-plan.md
```
