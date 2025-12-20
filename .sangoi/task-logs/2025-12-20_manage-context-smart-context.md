Title: Smart context overlay + `manage_context` tool (codex-core)

Context
- Problem: Long sessions accumulate tool outputs and reasoning; `/compact` helps but is disruptive and lossy.
- Goal: Provide model-controlled “smart context” management so the agent can keep only the useful parts (and drop/replace the rest) without restarting or compacting.
- Constraint: `replace` must be restricted to ToolOutput + Reasoning (no rewriting user/assistant messages).

Completed
- Added `manage_context` tool (schema + handler) to list/status current context and apply include/exclude/delete, plus `replace` for tool outputs/reasoning and pinned notes.
- Introduced `RolloutItem::ContextOverlay` (replacements + notes) and replayed it on resume/fork, so smart context survives restarts.
- Wired prompt construction to use an “effective history” (mask + replacements + notes) instead of raw history.
- Fixed prune invariant: deleting a tool call now also deletes its corresponding output(s), preventing orphan outputs from breaking sessions.
- Added a final normalization pass during prompt build to drop orphan outputs and insert placeholder outputs for calls missing outputs (safety net).
- Refactored RID helpers into `codex-rs/core/src/rid.rs`.

Files Touched
- codex-rs/protocol/src/protocol.rs
- codex-rs/core/src/tools/handlers/manage_context.rs
- codex-rs/core/src/tools/spec.rs
- codex-rs/core/src/state/session.rs
- codex-rs/core/src/codex.rs
- codex-rs/core/src/model_family.rs
- codex-rs/core/src/rid.rs
- codex-rs/core/src/rollout/{policy.rs,recorder.rs,list.rs}
- .sangoi/reference/manage_context.md
- .sangoi/CHANGELOG.md

Validation
- `cargo check -p codex-core`

Risks / Follow-ups
- `delete` is destructive for the session (and persisted); consider a future “undo last delete” UX if needed.
- Notes are injected as a user message block; if needed, we can switch to a dedicated role/tagging scheme later.
- `manage_context` is gated by `experimental_supported_tools`; confirm which production model families should expose it by default.

Next Steps
- Add a small TUI affordance to show “edited/overlaid” markers (replaced items + note count) using existing ContextItems data.
- Consider a helper action like `compress_pair(call_id)` (distill + replace + exclude original) to standardize the common pattern.

