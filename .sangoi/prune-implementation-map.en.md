Prune Implementation Map (English)

Context
- Date: 2025-10-17
- Area: Prune flow (TUI) and native rollout persistence
- Key files: `codex-rs/tui/src/chatwidget.rs`, `codex-rs/core/src/{codex.rs,rollout/{policy.rs,recorder.rs},tools/spec.rs}`, `codex-rs/protocol/src/protocol.rs`

Problem
- Opening the prune menu and aborting (Esc/back) could persist incorrect state and leave the rollout desynchronized; the legacy `.bak` flow could also generate a 0-byte backup when failures occurred.

Final Design (current state)
- Native persistence (core): rollout now records `RolloutItem::ContextInclusion { included_indices, deleted_indices }` for prune actions.
- Resume: core applies ContextInclusion snapshots while reconstructing history—no file rewrite required.
- TUI: removed `.bak` handling and any rollout rewrite. Prune confirmation closes the menu and shows a summary toast; cancel truly cancels.

User-visible behavior
- Manual prune: after confirmation, shows “Pruned {category}: ~{pct}% freed.”
- Advanced prune: after confirmation, shows “Applied advanced prune: include +N, exclude -M, delete ×K (freed ~P%).”
- Esc/back from any prune view does not persist or reopen menus unexpectedly.
- No `.bak` file is created; “Restore full context” entry removed from the prune menu.

Implementation highlights
- Protocol: added `ContextInclusionItem` plus `RolloutItem::ContextInclusion`.
- Persist policy/recorder: ContextInclusion is persisted and parsed the same way as Compacted entries.
- Core ops emission: `Op::SetContextInclusion` and `Op::PruneContextByIndices` append ContextInclusion snapshots.
- Resume path: `reconstruct_history_from_rollout()` applies ContextInclusion to rebuild effective history.
- TUI interaction:
  - Advanced list → Confirm: confirm popup uses `on_complete_event: None` to avoid reopening menus; Apply triggers ops and closes.
  - Manual list → Confirm: confirm popup uses `on_complete_event: None`; Apply triggers the prune op and closes.
  - Menu/root: no `.bak` restore entry.

Tests
- `codex-rs/tui/src/chatwidget/tests.rs: prune_cancel_clears_toggles`
- `codex-rs/tui/src/chatwidget/tests.rs: prune_root_open_close_without_changes_leaves_no_toggles`

Notes
- All prune persistence is native in the rollout; no shutdown work is required.

Crash Fix – UTF-8 boundary in previews (2025-10-18)
- Symptom: any attempt to open or use `/prune` crashed with `assertion failed: self.is_char_boundary(new_len)` in `core/src/state/session.rs:265`.

- Root cause
  - The preview builder for context items called `String::truncate(MAX)` on potentially non-ASCII text.
  - When `MAX` cut through the middle of a UTF-8 code point (accents/emoji), `truncate` asserted on the boundary and the binary panicked.

- Fix (core)
  - Implemented a grapheme/UTF-8 safe helper:
    - `truncate_grapheme_head(&str, max_bytes) -> String` in `core/src/truncate.rs`.
    - Kept `truncate_middle` for mid-string trimming (unchanged).
  - `preview_for(..)` now:
    - Formats the full preview string (including prefixes like `role:` / `tool output:`).
    - Applies a uniform cap via `truncate_grapheme_head` so we never cut through a code point or cluster.
  - Hardened extras:
    - `prune_by_indices` now performs `sort_unstable` + `dedup` + reverse (desc) to avoid incorrect removals when duplicate indices arrive.

- Fix (TUI – advanced flow)
  - After confirming “Yes, apply prune”: clears local toggles, issues `Op::GetContextItems` to resync, and returns to the composer (does not reopen Advanced).
  - Added a `[!]` marker for potentially risky deletions (when the next included item is an assistant message).

- Files touched
  - `codex-rs/core/src/state/session.rs` (safe preview + dedup/sort in `prune_by_indices`).
  - `codex-rs/core/src/truncate.rs` (new `truncate_grapheme_head`, tests, docstring).
  - `codex-rs/core/Cargo.toml` (workspace `unicode-segmentation`).
  - `codex-rs/tui/src/chatwidget.rs` (post-apply returns to composer; refresh/clear; `[!]` marker).

- Validation
  - `cargo check -p codex-core` and `-p codex-tui` pass.
  - Unit tests: `truncate_head_honors_char_boundaries` and `truncate_grapheme_head_preserves_clusters`.
  - Manual: history containing emoji/accents → opening `/prune` no longer panics; previews remain stable.

- Residual risks / follow-ups
  - Grapheme-safe truncation adds a minor cost (irrelevant for `MAX=80`).
  - For more intelligent risky-deletion detection, consider heuristics that look for non-adjacent dependencies.

Rollout Replay Hardening – Stable RIDs (2025-10-20)
- Problem: `ContextInclusion` stored only post-prune indices. During resume, those indices diverged from the originals and deleted items reappeared.
- Structural solution:
  - Every `ResponseItem` now receives a RID (`r{u64}`) monotonic in `SessionState`.
  - `ContextItemSummary` exposes `id` to the UI/tests; `ContextInclusionItem` persists `included_ids`/`deleted_ids` alongside raw indices.
    - `id` follows the `r{u64}` format and is assigned by the core whenever an item enters the conversation.
- Replay (`reconstruct_history_from_rollout`) reapplies prune/set-inclusion using the RIDs, with an index fallback for legacy rollouts.
  - During replay, each `RolloutItem::ResponseItem` passes through `ReplayRidTracker::next()` to guarantee deterministic ordering.
  - `Compacted` events call `assign_compacted_rids` to generate a stable sequence with the same runtime logic.
- Emission:
  - `Op::SetContextInclusion` and `Op::PruneContextByIndices` collect `included_ids` from the current items and map `deleted_ids` before mutation.
    - Collection uses `session.history_rids` to avoid races against partial rebuilds.
  - `PruneContextByIndices` writes the deleted RIDs into the snapshot for deterministic reconstruction.
  - Snapshots also include the original `included_indices`/`deleted_indices` for compatibility with older rollouts.
- State:
  - `SessionState` tracks `history_rids` and redistributes IDs in `replace_history`/`prune_by_indices`.
  - Helper `apply_include_mask_from_ids` rebuilds the post-replay mask by mapping RID → current index.
- Tests:
  - New unit coverage in `codex.rs` locks replay with IDs and index-only fallbacks.
  - Hybrid manual + advanced case ensures inclusion masks accumulate correctly.
- Manual prune:
  - `Op::PruneContext` persists `ContextInclusion` snapshots (with `included_ids`) immediately after category application, guaranteeing manual prunes survive resume.
  - The TUI continues to emit categories only; the core expands them to RIDs.
- Risks:
  - RIDs are monotonic globals; extremely long sessions could approach `u64::MAX`, but that risk is theoretical.

Prune UX Refinements – Esc Close & Manual Snapshot (2025-10-20)
- Problem: `Esc` inside prune submenus returned to the root menu instead of exiting the flow; the manual menu opened without metrics until someone visited Advanced (the only path that triggered `GetContextItems`).
- New behavior:
  - `Esc` in any submenu (manual or advanced) exits `/prune`; no automatic reopen of the root menu.
  - Manual menu reuses the Advanced snapshot path. On open, it sends `Op::GetContextItems`, marks the list active, and refreshes descriptions as soon as the event lands.
- Implementation:
  - `ChatWidget` now tracks `manual_menu_active` plus `manual_menu_entries` and adds helper `refresh_manual_menu_view()` to recompute descriptions via freshly persisted RIDs.
  - Manual and advanced flows share the close routine (`clear_manual_menu_tracking`, `reset_advanced_prune_state`) to avoid resume debris.
  - `ListSelectionView` now exposes `update_description_at_index` so descriptions refresh in place without recreating the view.
- Tests:
  - `manual_prune_esc_closes_flow` ensures `Esc` exits the flow.
  - `manual_prune_menu_requests_snapshot_and_updates_counts` covers dynamic recomputation of percentages when `ContextItems` arrives.

Smart Context Overlay – `manage_context` (2025-12-20)
- Motivation: long-running sessions accumulate tool outputs and reasoning; `/compact` helps but is disruptive/losing detail.
- Solution: add a persisted overlay layer (replacements + pinned notes) applied when building the prompt, so the model can distill large outputs into short text and keep durable notes.
- Safety: deleting tool calls also deletes their corresponding outputs; prompt construction additionally normalizes call/output pairs so orphan outputs never break the session.
- Reference: `.sangoi/reference/manage_context.md`.
