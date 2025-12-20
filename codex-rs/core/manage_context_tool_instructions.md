## `manage_context` (Smart Context)

The user should not need to know about this tool. Use it yourself to keep long sessions healthy.

When to use
- If you are low on context (roughly `context_left_percent <= 20`) or you just produced/received large tool outputs.

Preferred flow (non-interactive, 2 calls)
1) `mode="retrieve"` (read-only) to get a bounded snapshot (`snapshot_id`, token usage, items).
2) `mode="apply"` with `snapshot_id` + an ordered batch of `ops` applied atomically.
   - If `snapshot_id` mismatches, re-run `retrieve` and retry with the new snapshot.

What to do
- Prefer `exclude` when unsure (non-destructive).
- Use `replace` to distill large ToolOutput/Reasoning into short, high-signal text.
- Use `delete` only for safe-to-drop history (it is destructive; deleting a tool call also deletes its outputs).
- Use notes for pinned facts; keep them short.

Hard safety rules
- `replace` is allowed ONLY for ToolOutput and Reasoning. Never replace user/assistant messages.

Example (retrieve)
```json
{"mode":"retrieve","max_items":120,"include_token_usage":true,"include_pairs":true}
```

Example (apply)
```json
{
  "mode":"apply",
  "snapshot_id":"<from retrieve>",
  "ops":[
    {"op":"replace","targets":{"call_ids":["call_123"]},"text":"Key results: ..."},
    {"op":"exclude","targets":{"indices":[0,1,2]}},
    {"op":"add_note","notes":["Decision: ...","Constraint: ..."]}
  ]
}
```
