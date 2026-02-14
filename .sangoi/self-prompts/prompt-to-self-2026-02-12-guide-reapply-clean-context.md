# Prompt to self — clean-session resume (guide-reapply)
Date: 2026-02-12
Status: Active

```text
You are working in the repo `codex`.
Continue from:
- CWD: /home/lucas/work/codex
- Branch: reapply/accounts-20260209
- Last commit: 44b92f9a854b4af68cc4a1c5f87d4a9daa6dc23e
- Date (UTC-3): 2026-02-12T13:54:36-03:00
- Nested docs repo: .sangoi (branch master, head f4326540697df7432a512c45446014e780f48b79)

Objective (1 sentence)
- Implement the remaining `guide-reapply` items (`manage_context`, `subagents`, `/sanitize`) with fail-loud behavior, senior gates, and guide sync before any commit/push.

State
- Done:
  - `/accounts` reapply is implemented and validated.
  - Completed sub-agent status message is already untruncated (trim-only).
  - Main branch base sync done: local `main` == `upstream/main` at `44b92f9a8`; work branch updated to this base.
  - Focused battery for accounts/login/core/tui/exec is green with non-zero passed counts.
- In progress:
  - `guide-reapply` package implementation is open: `manage_context` -> `subagents` -> `/sanitize`.
  - Recon already confirmed `manage_context` and `/sanitize` are currently absent in source wiring.
- Blocked / risks:
  - No local commit yet for this work; large staged working set already exists.
  - `subagent_instructions_file` does not exist in current config model; must be added explicitly (no hidden fallback shim).
  - Temporary worktree `/tmp/codex-sync-master-20260212-113506` has an unfinished merge for `master`; do not disturb it while implementing these features.

Decisions / constraints (locked)
- Do NOT commit or push yet.
- `/accounts` is already done; do not rework unless needed for contract integrity.
- Execute in order: `manage_context` -> `subagents` -> `/sanitize`.
- Do recon from code first; do not trust guides blindly.
- Run Senior Plan Advisor after drafting the implementation plan and Senior Code Reviewer after all plan items complete.
- If seniors require changes, update code AND the relevant `guide-reapply` docs.
- Fail loud: root-cause fixes only; no compatibility shims/workarounds.
- Keep `SlashCommand::DebugConfig` and `Clean` available during active task.

Follow-up (ordered)
1. Rebuild and validate implementation plan for `manage_context` (state+tools+tests+docs).
2. Implement `manage_context` v2 end-to-end (handler/spec/state wiring + tests).
3. Implement subagent instruction plumbing (`subagent_instructions_file`) and status/wait refinements as needed.
4. Implement `/sanitize` task flow (`tasks/sanitize.rs`, `sanitize_prompt.md`, Op + slash command + docs + tests).
5. Run focused validations after each lane, then final combined battery.
6. Run Senior Code Reviewer gate; fix findings.
7. Sync `guide-reapply` docs to match final behavior.

Next immediate step (do this first)
- Draft the exact `manage_context` port plan from current code + backup anchor and run Advisor on that draft.
Commands:
cd /home/lucas/work/codex
git show backup/reapply-state-20260209-125806:codex-rs/core/src/tools/handlers/manage_context.rs | sed -n '1,260p'
git show backup/reapply-state-20260209-125806:codex-rs/core/src/state/context.rs | sed -n '1,260p'

Files
- Changed files (current working set; no local commit yet for this body of work):
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
  - codex-rs/tui/src/chatwidget/tests.rs
  - codex-rs/tui/src/slash_command.rs
  - codex-rs/tui/src/status/account.rs
  - codex-rs/tui/src/status/card.rs
  - codex-rs/tui/src/status/helpers.rs
  - docs/authentication.md
  - docs/multi-account-auth-plan.md
  - docs/slash_commands.md
- Focus files to open first:
  - codex-rs/core/src/tools/spec.rs — add `manage_context` tool schema and registry wiring.
  - codex-rs/core/src/tools/handlers/mod.rs — register new handler(s).
  - codex-rs/core/src/tools/handlers/collab.rs — subagent config/status/wait behavior and tests.
  - codex-rs/core/src/config/mod.rs — add `subagent_instructions_file` and load semantics.
  - codex-rs/protocol/src/protocol.rs — add sanitize op if needed by core dispatch.
  - codex-rs/core/src/codex.rs — wire new Op/task handlers (`manage_context`/`sanitize`).
  - codex-rs/core/src/tasks/mod.rs — add sanitize task entry.
  - codex-rs/core/src/tasks/review.rs — template for sanitize task behavior.
  - codex-rs/tui/src/slash_command.rs — add `/sanitize` enum/help/availability.
  - codex-rs/tui/src/chatwidget.rs — slash dispatch and `Op::Sanitize` submission flow.
  - .sangoi/docs/guide-reapply-manage_context.md — contract anchor; update if implementation changes.
  - .sangoi/docs/guide-reapply-subagents.md — contract anchor; update if implementation changes.
  - .sangoi/docs/guide-reapply-sanitize.md — contract anchor; update if implementation changes.

Validation (what “green” looks like)
- cd /home/lucas/work/codex/codex-rs && just fmt
  # expected: exits 0
- cargo test -p codex-login --lib persist_tokens_async_ -- --quiet
  # expected: test result ok with passed > 0
- cargo test -p codex-core auth_refresh -- --quiet
  # expected: test result ok with passed > 0
- cargo test -p codex-tui accounts_popup -- --quiet
  # expected: test result ok with passed > 0
- cargo test -p codex-tui logout_popup -- --quiet
  # expected: test result ok with passed > 0
- cargo test -p codex-tui slash_command -- --quiet
  # expected: test result ok with passed > 0
- cargo test -p codex-exec completed_status_message -- --quiet
  # expected: test result ok with passed > 0
- Add and run new focused filters for `manage_context` and `sanitize` once those tests exist.

Known traps / gotchas
- In this environment, some test builds may require system `libcap` presence; fail loudly and document workaround instead of masking failures.
- Do not let filtered tests pass with `0 passed`; enforce `passed > 0` checks.
- Keep `.sangoi/local/**` and ad-hoc scratch artifacts out of staged scope.

References (read before coding)
- .sangoi/local/plan-2026-02-12-guide-reapply-implementation.md
- .sangoi/docs/guide-reapply-mods-order.md
- .sangoi/docs/guide-reapply-manage_context.md
- .sangoi/docs/guide-reapply-subagents.md
- .sangoi/docs/guide-reapply-sanitize.md
- .sangoi/plans/plan-manage_context-rollout.md
- .sangoi/plans/plan-manage_context-replace-vs-exclude.md
- .sangoi/docs/mods-reapply-inventory-2026-02-09.md
- docs/multi-account-auth-plan.md
```
