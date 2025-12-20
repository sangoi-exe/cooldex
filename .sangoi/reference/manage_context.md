Manage Context (Smart Context Overlay)
=====================================

Purpose
- Reduce context growth without forcing `/compact`.
- Let the model keep only the useful parts of large tool outputs and reasoning.
- Keep context edits reversible when possible (exclude/include, replace), and safe by default.

High-level design
- Keep the raw transcript (rollout `ResponseItem`s) intact.
- Build an *effective* prompt history at send-time:
  - apply inclusion mask (include/exclude)
  - apply replacements (RID -> distilled text)
  - inject pinned notes (small XML-wrapped block)
  - normalize tool call/output pairs (drop orphan outputs; add placeholder outputs for missing outputs)

Model tool: `manage_context`
- Location: `codex-rs/core/src/tools/handlers/manage_context.rs`
- Schema: `codex-rs/core/src/tools/spec.rs`
- Enabled for model families that include `"manage_context"` in `experimental_supported_tools`

v2: non-interactive retrieve/apply
- Motivation: avoid chatty/iterative “menus” under context pressure.
- Contract (common path = 2 calls max):
  1) `mode="retrieve"`: returns a single bounded JSON snapshot with `snapshot_id`.
  2) `mode="apply"`: applies a batch of `ops` atomically; if `snapshot_id` mismatches, rejects (anti-drift).
- Notes:
  - `snapshot_id` is an opaque SHA-1 over a stable representation of items + overlay (not security-sensitive; only for drift detection).
  - `dry_run=true` on apply runs the batch on a simulated state and returns the computed summary + `new_snapshot_id` without mutating.
  - `replace` remains restricted to ToolOutput + Reasoning only (never user/assistant messages).
  - `delete` cascades tool call -> tool outputs (same invariant as v1).

Actions
- `status`
  - Returns current token usage info and overlay counts (replacements, notes).
- `list`
  - Returns context item summaries including `index`, `id` (RID), `category`, `included`, `preview`, and best-effort tool `call_id`.
- `include` / `exclude`
  - Stages items into/out of the next prompt (non-destructive).
  - Targets can be provided via `indices`, `ids` (RID strings like `r42`), or `call_ids`.
- `include_all`
  - Clears the include mask (everything included again).
  - Note: does not restore deleted items.
- `delete`
  - Destructively removes items from the session history (and persists deletion on resume).
  - If a tool call is deleted, its corresponding output(s) are deleted as well to preserve invariants.
- `replace`
  - Overlays the *effective* text for a target item with distilled content.
  - Hard restriction: only allowed for ToolOutput and Reasoning items.
  - Targeting: `id` / `index` / `call_id` (call_id targets tool outputs).
- `clear_replace`
  - Clears all replacements, or clears replacements for specific targets.
- `add_note` / `remove_note` / `clear_notes`
  - Adds/removes pinned notes that are injected into the prompt as a small context block.

Persistence & replay
- Inclusion/deletion snapshots: `RolloutItem::ContextInclusion`
- Overlay snapshots: `RolloutItem::ContextOverlay` (replacements + notes)
- Replay path: `codex-rs/core/src/codex.rs` applies `ContextOverlay` on resume/fork.

Safety: tool call/output invariants
- Deleting tool calls can otherwise leave orphan outputs (and vice versa).
- Fixes:
  - `SessionState::prune_by_indices` expands deletions so deleting a call also deletes its outputs.
  - Prompt build does a final `normalize_tool_call_pairs` pass (drop orphan outputs; insert placeholder outputs for missing outputs).
  - This prevents the “tries to find the call for an output and fails” session error.

Key files
- `codex-rs/core/src/state/session.rs`
  - `history_for_prompt`, `prune_by_indices`, `normalize_tool_call_pairs`
- `codex-rs/core/src/tools/handlers/manage_context.rs`
  - Tool implementation and persistence
- `codex-rs/protocol/src/protocol.rs`
  - `RolloutItem::ContextOverlay`, `ContextOverlayItem`
