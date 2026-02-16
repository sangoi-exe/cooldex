## Pre-Compaction Recall (`recall`)

`recall` is a read-only tool for recovering recent context from the **current session rollout JSON** before the latest compaction marker.

It intentionally returns only:
- assistant messages
- reasoning text (summary first; falls back to reasoning content when summary is absent)

It intentionally excludes:
- tool calls
- tool outputs
- user messages

### Contract

Request fields:
- none (`{}` only)

Unknown fields are rejected (including removed legacy fields like `max_items` and `max_chars_per_item`).

### Behavior

- Source is fixed to the current session rollout recorder path (no path argument).
- Uses the latest `RolloutItem::Compacted` marker as the upper boundary.
- Uses the previous `EventMsg::ContextCompacted` (before the latest compact marker) as the lower boundary and starts right after it.
- If no previous `context_compacted` event exists (first compaction cycle), starts from the beginning of the rollout.
- Applies payload size cap from `config.toml` key `recall_kbytes_limit` (default `256` KiB).
- If the rollout has malformed lines, `recall` returns the valid parsed subset and reports integrity as degraded (`integrity.status = "degraded"`, `integrity.rollout_parse_errors > 0`).
- If there is no compaction marker, the tool fails with `stop_reason = "no_compaction_marker"`; when parse errors exist, the error message includes the parse-error count.

### Example

```json
{}
```

Response shape (summary):
- `mode = "recall_pre_compact"`
- `integrity.status` and `integrity.rollout_parse_errors`
- `boundary.start_index`
- `boundary.last_context_compacted_event_index` (nullable)
- `boundary.latest_compacted_index`
- `counts`
- `items[]` with:
  - `kind = "assistant_message" | "reasoning"`
  - `rollout_index`
  - `text`
  - `phase` (assistant message only, when available)
