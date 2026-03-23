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
  - `RolloutItem::Compacted` with `replacement_history: Some(...)` from standard compaction, or from prompt-gc apply only when that marker is immediately followed by its persisted `TurnContext` item.
- If that lower boundary is `replacement_history_compacted`, `recall` starts from the sanitized `replacement_history` persisted on that boundary, hydrates assistant/reasoning items plus prompt-gc context notes stored as tagged note messages, and then appends newer rollout items after the marker until the latest compaction marker.
- Otherwise, returned scan starts at `lower_boundary_index + 1`.
- If no lower boundary marker exists, scan starts at rollout index `0`.
- If no upper boundary marker exists, the tool fails with `stop_reason = "no_compaction_marker"`.
- Merge-safety note: this boundary behavior must stay aligned with `codex-rs/core/src/tools/handlers/recall.rs` and compaction paths that persist `replacement_history`.

### Filtering and Size Budget

- Includes assistant messages, reasoning text, and prompt-gc tool/reasoning context notes hydrated from `replacement_history` boundaries.
- Excludes ordinary user messages, tool calls, and tool outputs.
- Reasoning uses summary text first and falls back to reasoning content when needed.
- Uses the latest non-observational `RolloutItem::Compacted` as the upper boundary.
- For standard compactions, the matching `EventMsg::ContextCompacted(_)` is a legacy event emitted after the `ContextCompaction` item completes, so the current compaction's own legacy event is not part of the pre-`Compacted` scan.
- Uses the most recent earlier boundary marker before that upper boundary as the lower boundary, where a boundary marker is either:
  - `EventMsg::ContextCompacted(_)` from a previous compaction, or
  - `RolloutItem::Compacted` with `replacement_history: Some(...)` from a previous standard compaction, or from a previous prompt-gc apply only when the persisted prompt-gc marker is immediately followed by its `TurnContext` item.
- When that lower boundary is `replacement_history_compacted`, the persisted sanitized history at that boundary becomes the authoritative recall base for the returned reasoning/assistant/context-note window.
- Applies `recall_kbytes_limit` (default `256` KiB) as a byte cap from the tail of matching items.

### Output Modes

- Compact mode (default: `recall_debug` unset or `false`):
  - `mode = "recall_pre_compact_compact"`
  - `source = "current_session_rollout"`
  - `legend["[r]"] = "reasoning"`
  - `legend["[am]"] = "assistant message"`
  - `legend["[tc]"] = "tool context note"`
  - `legend["[rc]"] = "reasoning context note"`
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
  - `filters.include_reasoning`
  - `filters.include_assistant_messages`
  - `filters.include_context_notes`
  - `filters.exclude_tool_output`
  - `counts.matching_pre_compact_items`
  - `counts.returned_items`
  - `counts.returned_bytes`
  - `counts.bytes_limit`
  - `items[]` objects with `kind` (`"reasoning" | "assistant_message" | "tool_context_note" | "reasoning_context_note"`), `source` (`"rollout"` | `"replacement_history"`), `rollout_index` (nullable for items hydrated from sanitized `replacement_history`), `text`, and optional `phase`

For hydrated prompt-gc context notes, `text` preserves the tagged note body, including the leading `chunk_id=...` line from the stored note.

When rollout parsing encounters malformed lines, debug mode returns valid parsed items and reports degraded integrity instead of hard-failing.
