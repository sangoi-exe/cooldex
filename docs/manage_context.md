## Context management (`manage_context`)

Long Codex sessions can accumulate large tool outputs and reasoning. When you start approaching the model context
window, you have two options:

- Use `/compact` to summarize the session and drop older details.
- Use `/sanitize` to sanitize context (uses `manage_context` to reclaim space).
- Use `/hygiene` to toggle automatic post-turn hygiene (deletes last-turn tool call/output history, updates `<tool_context>`, and consolidates reasoning into `<reasoning_context>`).
- Use `manage_context` for targeted pruning: selectively include/exclude/delete history items, replace very large tool
  outputs/reasoning with a short distilled summary, and add pinned notes that should stay visible across turns.

For in-session guidance aimed at agents/models, see `docs/manage_context_model.md`.
For a short checklist, see `docs/manage_context_cheatsheet.md`.

### In-session workflow (v2: retrieve → apply)

#### Retrieve a snapshot

Start cheap:

```json
{"mode":"retrieve"}
```

Prefer targeting from the summary (`breakdown.top_calls` / `breakdown.top_included_items`). If you can't identify what to prune without pulling full item lists, prefer `/compact` over expanding context.

#### Apply operations

```json
{
  "mode":"apply",
  "snapshot_id":"<from retrieve>",
  "ops":[
    {"op":"replace","targets":{"ids":["call_123"]},"text":"Key results: ..."},
    {"op":"exclude","targets":{"ids":["r10","r11"]}},
    {"op":"add_note","notes":["Decision: ...","Constraint: ...","Next: ..."]}
  ]
}
```

The `apply` response includes `token_usage` (a best-effort estimate) so you can confirm the impact immediately. It also includes
`affected_ids`/`missing_ids` (debugging).

### Supported ops

- `include`, `exclude`, `include_all`
- `consolidate_reasoning` (extract included reasoning summaries under `extracted.reasoning.items` and exclude the original reasoning items)
- `delete` (deletes targeted items; tool call/output pairs are removed together)
- `replace` (tool outputs + reasoning only), `clear_replace`, `clear_replace_all`
- `add_note`, `remove_note`, `clear_notes`
