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
  - a legacy/standard non-prompt-gc `RolloutItem::Compacted` with `replacement_history: None` (`boundary.last_boundary_kind = "compacted_marker"`), or
  - `RolloutItem::Compacted` with `replacement_history: Some(...)` from standard compaction, or from prompt-gc apply only when that marker is immediately followed by its persisted `TurnContext` item (`boundary.last_boundary_kind = "replacement_history_compacted"`).
- Legacy message-only prompt-gc markers (`message == "[internal] prompt_gc"` with `prompt_gc: null`) are treated as incompatible private artifacts: they do not count as upper/lower boundaries, they do not hydrate `replacement_history`, and surviving copies disable future prompt-gc for that session instead of being replayed as standard compaction.
- If that lower boundary is `replacement_history_compacted`, `recall` starts from the sanitized `replacement_history` persisted on that boundary, hydrates assistant/reasoning items plus prompt-gc context notes stored as tagged note messages, and then appends newer rollout items after the marker until the latest compaction marker.
- Otherwise, returned scan starts at `lower_boundary_index + 1`.
- If no lower boundary marker exists, scan starts at rollout index `0`.
- If rollout parsing encounters malformed JSONL lines, `recall` skips those lines and continues from the remaining valid rollout items.
- If no upper boundary marker exists after malformed lines are skipped, the tool fails with `stop_reason = "no_compaction_marker"`.
- Merge-safety anchor: this boundary behavior must stay aligned with `codex-rs/core/src/tools/handlers/recall.rs` and compaction paths that persist `replacement_history`.

### Filtering and Size Budget

- Includes assistant messages, reasoning text, and prompt-gc tool/reasoning context notes hydrated from `replacement_history` boundaries.
- Excludes ordinary user messages, tool calls, and tool outputs from `items[]`.
- Reports minimal user-anchor coverage separately under `user_anchors`; default policy is `missing_latest_only`, which includes at most the latest real user message only when the latest compacted `replacement_history` is present and missing that anchor.
- Synthetic post-compact recovery warnings and prompt-gc context notes are never treated as real user anchors.
- Reasoning uses summary text first and falls back to reasoning content when needed; opaque encrypted-only reasoning is intentionally skipped because it has no semantic payload to hydrate.
- Applies `recall_kbytes_limit` (default `256` KiB) as a byte cap from the tail of matching items.
- If the newest semantic item alone exceeds the byte cap, `recall` returns a suffix-truncated version of that item with explicit truncation metadata instead of returning an empty result solely because the newest item was too large.

### Output Modes

- Compact mode (default: `recall_debug` unset or `false`):
  - `mode = "recall_pre_compact_compact"`
  - `source = "current_session_rollout"`
  - `legend["[r]"] = "reasoning"`
  - `legend["[am]"] = "assistant message"`
  - `legend["[tc]"] = "tool context note"`
  - `legend["[rc]"] = "reasoning context note"`
  - `integrity.status = "ok"` when no malformed rollout lines were skipped, or `"degraded"` when `recall` skipped malformed JSONL lines
  - `integrity.rollout_parse_errors = <real skipped-line count>` on every successful response
  - `integrity.truncated`
  - `boundary.start_index`
  - `boundary.last_boundary_index` (nullable)
  - `boundary.last_boundary_kind` (`"context_compacted_event" | "compacted_marker" | "replacement_history_compacted" | null`)
  - `boundary.latest_compacted_index`
  - `boundary.compacted_markers_seen`
  - `counts.matching_pre_compact_items`
  - `counts.returned_items`
  - `counts.dropped_items`
  - `counts.returned_bytes`
  - `counts.bytes_limit`
  - `user_anchors.policy = "missing_latest_only"`
  - `user_anchors.coverage_status` (`"not_applicable" | "covered" | "missing" | "unknown"`)
  - `user_anchors.latest_real_user_seen_in_rollout`
  - `user_anchors.latest_real_user_rollout_index` (nullable)
  - `user_anchors.latest_real_user_present_in_replacement_history` (nullable)
  - `user_anchors.older_user_messages_may_be_truncated`
  - optional `user_anchors.missing_latest_user_anchor` with `rollout_index` and `text`
  - `items[]` as numbered strings
  - truncated compact entries include `[truncated_from_start][bytes=<returned>/<original>]`

- Debug mode (`recall_debug = true`):
  - `mode = "recall_pre_compact"`
  - `source = "current_session_rollout"`
  - `integrity.status = "ok"` when no malformed rollout lines were skipped, or `"degraded"` when `recall` skipped malformed JSONL lines
  - `integrity.rollout_parse_errors = <real skipped-line count>` on every successful response
  - `integrity.truncated`
  - `boundary.start_index`
  - `boundary.last_boundary_index` (nullable)
  - `boundary.last_boundary_kind` (`"context_compacted_event" | "compacted_marker" | "replacement_history_compacted" | null`)
  - `boundary.latest_compacted_index`
  - `boundary.compacted_markers_seen`
  - `filters.include_reasoning`
  - `filters.include_assistant_messages`
  - `filters.include_context_notes`
  - `filters.exclude_tool_output`
  - `counts.matching_pre_compact_items`
  - `counts.returned_items`
  - `counts.dropped_items`
  - `counts.returned_bytes`
  - `counts.bytes_limit`
  - `user_anchors` with the same fields as compact mode
  - `items[]` objects with `kind` (`"reasoning" | "assistant_message" | "tool_context_note" | "reasoning_context_note"`), `source` (`"rollout"` | `"replacement_history"`), `rollout_index` (nullable for items hydrated from sanitized `replacement_history`), `text`, optional `phase`, and optional truncation fields (`truncated`, `truncation.side`, `truncation.original_bytes`, `truncation.returned_bytes`)

For hydrated prompt-gc context notes, `text` preserves the tagged note body, including the leading `chunk_id=...` line from the stored note.
