# Manage Context / Sanitize Hardening Restart (medium)

## Objective
Resume `manage_context` + `/sanitize` hardening on `reapply/accounts-20260209`, prove current HEAD behavior, and patch only root-cause contract drift.

## Phase 0 — Preconditions
- [x] Confirm branch/worktree/HEAD.
  - Done criteria: branch is `reapply/accounts-20260209`; HEAD recorded; worktree state captured.
  - Commands:
    - `git status --short --branch`
    - `git rev-parse --short=10 HEAD`
- [x] Apply HEAD gate.
  - Done criteria: if HEAD differs from `9f4fe487f`, drift magnitude and touched target files are recorded in handoff.
  - Commands:
    - `git rev-list --left-right --count 9f4fe487f...HEAD`
    - `git diff --name-only 9f4fe487f..HEAD -- codex-rs/core/src/tools/spec.rs codex-rs/core/src/tools/handlers/manage_context.rs codex-rs/core/src/tasks/sanitize.rs`

## Phase 1 — Evidence Rehydration (No code changes)
- [x] Re-open primary evidence and contract surfaces.
  - Done criteria: invariants extracted from forensics + `spec.rs` + implementation files.
  - Files:
    - `.sangoi/reports/2026-02-14-manage-context-sanitize-full-forensics.md`
    - `codex-rs/core/src/tools/spec.rs`
    - `codex-rs/core/src/tools/handlers/manage_context.rs`
    - `codex-rs/core/src/tasks/sanitize.rs`
- [x] Reconfirm key symbols/guards.
  - Done criteria: anti-drift/hash/plan/sanitize symbols are present where expected.
  - Command:
    - `rg -n "state_hash_for_context|collect_top_offenders|chunk_manifest|plan_id_invalid|state_hash mismatch|collect_recent_manage_context_items|user_instructions|environment_context" codex-rs/core/src/tools/handlers/manage_context.rs codex-rs/core/src/tasks/sanitize.rs codex-rs/core/src/tools/spec.rs`

## Phase 2 — Baseline Validation (Current HEAD)
- [x] Audit focused coverage inventory first.
  - Done criteria: known gaps are mapped to existing tests or marked missing before baseline pass/fail interpretation.
  - Commands:
    - `cd codex-rs && cargo test -p codex-core --lib manage_context -- --list`
    - `cd codex-rs && cargo test -p codex-core --lib sanitize -- --list`
- [x] Run focused tests before any patching.
  - Done criteria: both invocations complete with captured pass/fail output.
  - Commands:
    - `cd codex-rs && cargo test -p codex-core --lib manage_context -- --test-threads=1`
    - `cd codex-rs && cargo test -p codex-core --lib sanitize -- --test-threads=1`
- [x] Gate A (patch-or-no-patch decision).
  - Done criteria: if tests/invariants are clean and known gaps are already covered, skip code edits and move to final validation/handoff (including build).

## Phase 3 — Root-Cause Fixes (Only If Gate A Fails)
- [x] Add/adjust missing regression coverage first.
  - Done criteria: explicit coverage for:
    - replace rejecting user/assistant message targets.
    - protected categories (`environment_context`, `user_instructions`) blocked for include/exclude/delete.
    - delete preserving call/output pair integrity.
    - sanitize seed selection (`collect_recent_manage_context_items`) and toolset restriction.
    - sanitize reasoning effort fallback behavior.
- [x] Patch root cause in order.
  - Done criteria: fix starts in `manage_context.rs`; `sanitize.rs` only if still required; no compat shims, hidden retries, or silent fallback behavior.
  - Files:
    - `codex-rs/core/src/tools/handlers/manage_context.rs`
    - `codex-rs/core/src/tasks/sanitize.rs` (conditional)

## Phase 4 — Validation + Build
- [x] Run formatting and focused validation.
  - Done criteria: formatting complete; focused tests pass.
  - Commands:
    - `cd codex-rs && just fmt`
    - `cd codex-rs && cargo test -p codex-core --lib manage_context -- --test-threads=1`
    - `cd codex-rs && cargo test -p codex-core --lib sanitize -- --test-threads=1`
- [x] Build target crate.
  - Done criteria: `cargo build -p codex-core` succeeds.
  - Command:
    - `cd codex-rs && cargo build -p codex-core`

## Phase 5 — Docs + Final Gate
- [x] Sync docs when contract or behavior semantics changed.
  - Done criteria: runtime/tool-usage/error semantic changes are reflected in docs in same change set.
  - Candidate files:
    - `docs/manage_context.md`
    - `docs/manage_context_model.md`
    - `codex-rs/core/sanitize_prompt.md` (if tool usage contract text changed)
- [x] Run Senior Code Reviewer gate.
  - Done criteria: reviewer verdict captured and addressed before handoff.
- [x] Final handoff packet.
  - Done criteria: include exact changed files, commands run with outcomes, invariant-by-invariant status, and residual risks.
