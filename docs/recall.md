<!-- Merge-safety anchor: recall docs must stay aligned with prompt_gc-aware compaction boundary rules and the runtime/tests that enforce them. -->
## Pre-Compaction Recall (`recall`)

`recall` is a read-only tool that retrieves model-relevant context from the current session rollout before the latest real compaction marker.

### Request Contract

- Request payload must be `{}`.
- Unknown fields are rejected with `invalid_contract`.

### Boundary Semantics

- Upper boundary: latest non-observational `RolloutItem::Compacted`.
- Lower boundary: most recent boundary marker before that upper boundary where a marker is either:
  - `EventMsg::ContextCompacted(_)`, or
  - `RolloutItem::Compacted` with `replacement_history: Some(...)` and no prompt-GC marker metadata.
- If that lower boundary is `replacement_history_compacted`, `recall` starts from the sanitized `replacement_history` persisted on that boundary and then appends newer assistant/reasoning rollout items after the marker until the latest compaction marker.
- Otherwise, returned scan starts at `lower_boundary_index + 1`.
- If no lower boundary marker exists, scan starts at rollout index `0`.
- If no upper boundary marker exists, the tool fails with `stop_reason = "no_compaction_marker"`.
- Merge-safety note: this boundary behavior must stay aligned with `codex-rs/core/src/tools/handlers/recall.rs` and compaction paths that persist `replacement_history`.

### Filtering and Size Budget

- Includes only assistant messages and reasoning text.
- Excludes user messages, tool calls, and tool outputs.
- Reasoning uses summary text first and falls back to reasoning content when needed.
- Uses the latest non-observational `RolloutItem::Compacted` as the upper boundary.
- For standard compactions, the matching `EventMsg::ContextCompacted(_)` is a legacy event emitted after the `ContextCompaction` item completes, so the current compaction's own legacy event is not part of the pre-`Compacted` scan.
- Uses the most recent earlier boundary marker before that upper boundary as the lower boundary, where a boundary marker is either:
  - `EventMsg::ContextCompacted(_)` from a previous compaction, or
  - `RolloutItem::Compacted` with `replacement_history: Some(...)` and no prompt-GC marker metadata.
- When that lower boundary is `replacement_history_compacted`, the persisted sanitized history at that boundary becomes the authoritative recall base for the returned reasoning/assistant window.
- Applies `recall_kbytes_limit` (default `256` KiB) as a byte cap from the tail of matching items.

### Output Modes

- Compact mode (default: `recall_debug` unset or `false`):
  - `mode = "recall_pre_compact_compact"`
  - `source = "current_session_rollout"`
  - `items[]` as numbered strings

- Debug mode (`recall_debug = true`):
  - `mode = "recall_pre_compact"`
  - `source = "current_session_rollout"`
  - `integrity.status`
  - `integrity.rollout_parse_errors`
  - `boundary.start_index`
  - `boundary.last_boundary_index` (nullable)
  - `boundary.last_boundary_kind` (`"context_compacted_event" | "replacement_history_compacted" | null`)
  - `boundary.latest_compacted_index`
  - `boundary.compacted_markers_seen`
  - `counts.matching_pre_compact_items`
  - `counts.returned_items`
  - `counts.returned_bytes`
  - `counts.bytes_limit`
  - `items[]` objects with `kind`, `source` (`"rollout"` | `"replacement_history"`), `rollout_index` (nullable for items hydrated from sanitized `replacement_history`), `text`, and optional `phase`

When rollout parsing encounters malformed lines, debug mode returns valid parsed items and reports degraded integrity instead of hard-failing.
