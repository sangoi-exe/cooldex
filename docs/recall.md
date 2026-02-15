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
- `max_items` (optional): maximum number of matching pre-compaction items to return.
- `max_chars_per_item` (optional, default `1200`): per-item text truncation limit.

Unknown fields are rejected.

### Behavior

- Source is fixed to the current session rollout recorder path (no path argument).
- Uses the latest `RolloutItem::Compacted` marker as the upper boundary.
- Uses the latest pre-compaction `EventMsg::UserMessage` as the lower boundary (starts right after that event). If none exists, starts from the beginning of the rollout.
- Applies payload size cap from `config.toml` key `recall_kbytes_limit` (default `256` KiB).
- If `max_items` is provided, applies that cap too, prioritizing non-commentary items (reasoning + assistant non-commentary) when assistant phase metadata is available, and using assistant commentary as fallback when needed.
- If there is no compaction marker, the tool fails with `stop_reason = "no_compaction_marker"`.

### Example

```json
{}
```

Response shape (summary):
- `mode = "recall_pre_compact"`
- `boundary.latest_compacted_index`
- `counts`
- `items[]` with:
  - `kind = "assistant_message" | "reasoning"`
  - `rollout_index`
  - `text`
  - `phase` (assistant message only, when available)
