# Reapply `/accounts` + sub-agent completed message (2026-02-11)

Label: **hard**

## Scope + target artifacts
- Keep and finish the existing multi-account auth port without sync/rebase drift.
- Keep only `/accounts` + account popup/logout UI behavior in TUI on top of current `HEAD` behavior.
- Keep sub-agent completed status formatting fix (no truncation).
- Target files:
  - `codex-rs/core/src/auth.rs`
  - `codex-rs/core/src/auth/storage.rs`
  - `codex-rs/core/tests/suite/auth_refresh.rs`
  - `codex-rs/login/src/server.rs`
  - `codex-rs/tui/src/slash_command.rs`
  - `codex-rs/tui/src/chatwidget.rs`
  - `codex-rs/tui/src/status/account.rs`
  - `codex-rs/tui/src/bottom_pane/chatgpt_add_account_view.rs`
  - `codex-rs/tui/src/chatwidget/snapshots/codex_tui__chatwidget__tests__accounts_popup.snap`
  - `codex-rs/tui/src/chatwidget/snapshots/codex_tui__chatwidget__tests__logout_popup.snap`
  - `codex-rs/exec/src/event_processor_with_human_output.rs`
  - `docs/authentication.md`
  - `docs/slash_commands.md`

## Checklist
- [x] 1) Evidence/repro: audit staged/unstaged + focus diffs.
  - Done criteria: drift outside `/accounts` identified with concrete files/symbols.
  - Commands run: `git status --short`; `git diff --cached --name-only`; focused diffs.
- [x] 2) Rebuild TUI port surgically on `HEAD` behavior.
  - Done criteria: `slash_command.rs` keeps existing upstream commands and adds `/accounts`; `chatwidget.rs` includes only account popup/logout additions with no unrelated feature removals.
  - Verification commands:
    - `git diff --cached -- codex-rs/tui/src/slash_command.rs codex-rs/tui/src/chatwidget.rs`
    - `rg -n "DebugConfig|Statusline|Clean|Accounts" codex-rs/tui/src/slash_command.rs`
- [x] 3) Close TUI cross-file contracts.
  - Done criteria: `AppEvent` variants and handler signatures needed by `/accounts` compile cleanly (`StartOpenAccountsPopup`, `SetActiveAccount`, `StartChatGptAddAccount`, `RemoveAccount`, `ConnectorsLoaded`) and mention bindings API matches current `HEAD`.
  - Verification commands:
    - `rg -n "StartOpenAccountsPopup|SetActiveAccount|StartChatGptAddAccount|RemoveAccount|ConnectorsLoaded" codex-rs/tui/src/app_event.rs codex-rs/tui/src/app.rs codex-rs/tui/src/chatwidget.rs`
    - `rg -n "mention_paths|mention_bindings" codex-rs/tui/src/chatwidget.rs codex-rs/tui/src/chat_composer.rs codex-rs/tui/src/app.rs`
- [x] 4) Close core/login compatibility.
  - Done criteria: `AuthStore` call sites compile across core/login/tui paths without legacy regressions and legacy helpers still satisfy dependents (`save_auth`, `load_auth_dot_json`).
  - Verification commands:
    - `rg -n "save_auth\(|AuthStore::from_legacy|list_accounts|set_active_account" codex-rs/core/src/auth.rs codex-rs/login/src/server.rs`
- [x] 5) Keep sub-agent completed message untruncated + tests.
  - Done criteria: formatter prints full trimmed message; regression tests present.
  - Verification commands:
    - `cargo test -p codex-exec completed_status_message -- --quiet`
- [x] 6) Focused validation + formatting.
  - Done criteria: all target commands pass and each target command reports test count > 0.
  - Verification commands:
    - `just fmt`
    - `cargo test -p codex-core auth_refresh -- --quiet`
    - `cargo test -p codex-tui accounts_popup -- --quiet`
    - `cargo test -p codex-tui logout_popup -- --quiet`
    - `cargo test -p codex-tui slash_command -- --quiet`
    - `cargo test -p codex-exec completed_status_message -- --quiet`
- [x] 7) Fan-in/review/handoff.
  - Done criteria: Senior Code Reviewer verdict is READY/READY_WITH_NITS; final summary includes residual risks + follow-ups.

## Fan-out lanes
- Senior Plan Advisor (read-only): critique intent alignment and execution/verification gaps before edits.
- Root lane (mutating): all file edits and test runs (single workspace writer).
- Senior Code Reviewer (read-only): full diff review after all plan steps complete.
