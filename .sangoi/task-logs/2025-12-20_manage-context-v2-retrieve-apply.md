Sprint 1 – 2025-12-20
====================

Context
- Scope: Evolve `manage_context` into a non-interactive contract that the agent can use in 2 calls (`retrieve` + `apply`) to cut context pressure without `/compact`.
- Goals: Add `snapshot_id` anti-drift, atomic batched ops, and a real `dry_run` path; preserve v1 behavior for compatibility.
- Constraints/flags: Do not modify core prompt text (prompt validation). Keep `replace` restricted to ToolOutput/Reasoning and keep tool call/output invariants safe.

Completed
- Changes:
  - Added `mode=retrieve|apply` to `manage_context`.
  - Implemented `retrieve` returning a single bounded JSON snapshot with `snapshot_id`, optional token usage, and optional pairing metadata.
  - Implemented `apply` with upfront validation, ordered batched ops, `snapshot_id` mismatch rejection, and `dry_run` simulation on a temporary `SessionState`.
- Files touched:
  - `codex-rs/core/src/tools/handlers/manage_context.rs`
  - `codex-rs/core/src/tools/spec.rs`
  - `.sangoi/reference/manage_context.md`
  - `.sangoi/CHANGELOG.md`
- Env/config changes: none

Validation
- Steps run:
- `cd codex-rs && RUSTUP_HOME=$HOME/.codextools/rustup CARGO_HOME=$HOME/.codextools/cargo cargo check -p codex-core`
- `cd codex-rs && RUSTUP_HOME=$HOME/.codextools/rustup CARGO_HOME=$HOME/.codextools/cargo cargo test -p codex-core truncate::tests::truncate_grapheme_head_preserves_clusters`
- `cd codex-rs && RUSTUP_HOME=$HOME/.codextools/rustup CARGO_HOME=$HOME/.codextools/cargo cargo test -p codex-core truncate::tests::truncate_head_honors_char_boundaries`
- Results:
- ✅ `cargo check` passed.
- ✅ Targeted truncate tests passed (filters used to avoid sandbox/PTY-only tests).
- ⚠️ Full `cargo test -p codex-core` fails in this environment due to `openpty` permission denied in `unified_exec` tests (sandbox limitation).
- Console/Network errors:
- `unified_exec` test failures: `failed to openpty: Permission denied`

Risks / Follow-ups
- Snapshot drift strictness: `snapshot_id` changes on any new history items, inclusion toggles, or overlay edits; if this proves too strict in practice, consider an opt-in “allow append-only drift” mode.
- Schema expressiveness: our tool JSON-schema subset can’t express “oneOf(action|mode)” strictly; rely on handler validation and tool description guidance.

Next Sprint
- Add focused tests for v2 (snapshot stability + replace restriction + dry_run no-mutation).
