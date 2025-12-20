Sprint 2 – 2025-12-20
====================

Context
- Scope: Make `manage_context` behave like `apply_patch`: model-facing, user shouldn’t need to learn it.
- Goals: Inject built-in manage_context usage instructions into the system prompt when the tool is available; add a bounded “what’s taking space” breakdown to guide autonomous pruning.
- Constraints/flags: Do not modify the core base prompt text directly (prompt validation); keep outputs bounded and deterministic.

Completed
- Changes:
  - Added `codex-rs/core/manage_context_tool_instructions.md` and appended it to system instructions when `manage_context` is present in the toolset.
  - Added `action=help` (v1 fallback) and enriched `status` + `retrieve` with:
    - per-category counts and approximate bytes (raw vs effective with replacements)
    - top included items by effective bytes (bounded)
    - per-item approx bytes in `retrieve`
- Files touched:
  - `codex-rs/core/manage_context_tool_instructions.md`
  - `codex-rs/core/src/client_common.rs`
  - `codex-rs/core/src/tools/handlers/manage_context.rs`
  - `codex-rs/core/src/tools/spec.rs`
  - `.sangoi/reference/manage_context.md`
  - `.sangoi/CHANGELOG.md`
- Env/config changes: none

Validation
- Steps run:
- `cd codex-rs && RUSTUP_HOME=$HOME/.codextools/rustup CARGO_HOME=$HOME/.codextools/cargo cargo check -p codex-core`
- `cd codex-rs && RUSTUP_HOME=$HOME/.codextools/rustup CARGO_HOME=$HOME/.codextools/cargo cargo test -p codex-core client_common::tests::get_full_instructions_includes_manage_context_when_tool_present`
- `cd codex-rs && RUSTUP_HOME=$HOME/.codextools/rustup CARGO_HOME=$HOME/.codextools/cargo just fmt`
- Results:
- ✅ `cargo check` passed.
- ✅ Targeted test for instruction injection passed.
- Console/Network errors:

Risks / Follow-ups
- “Approx bytes” is a heuristic (not token-accurate). It is intended for relative ranking (“what’s huge”), not accounting.
- If the injected instructions prove too verbose, tighten them further (keep them short so they don't contribute materially to context pressure).

Next Sprint
- Consider adding minimal tests for `manage_context.status` breakdown shape (boundedness + stable ordering).
