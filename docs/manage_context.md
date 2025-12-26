## Context management (`manage_context`)

Long Codex sessions can accumulate large tool outputs and reasoning. When you start approaching the model context
window, you have two options:

- Use `/compact` to summarize the session and drop older details.
- Use `/sanitize` to prune first-turn reasoning (manual fallback when the model can't make progress near the context limit).
- Use `manage_context` for targeted pruning: selectively include/exclude/delete history items, replace very large tool
  outputs/reasoning with a short distilled summary, and add pinned notes that should stay visible across turns.

For in-session guidance aimed at agents/models, see `docs/manage_context_model.md`.
For a short checklist, see `docs/manage_context_cheatsheet.md`.

> [!NOTE]
> The older offline script (`scripts/manage_context.py`) was removed; `manage_context` is now the supported workflow.

### In-session workflow (v2: retrieve → apply)

#### Retrieve a snapshot

Start cheap:

```json
{"mode":"retrieve","include_items":false}
```

If you need targets, ask for a bounded items list:

```json
{"mode":"retrieve","include_items":true,"max_items":200,"include_pairs":true}
```

#### Apply operations (dry-run recommended)

```json
{
  "mode":"apply",
  "snapshot_id":"<from retrieve>",
  "dry_run":true,
  "include_prompt_preview": true,
  "ops":[
    {"op":"replace","targets":{"call_ids":["call_123"]},"text":"Key results: ..."},
    {"op":"exclude","targets":{"ids":["r10","r11"]}},
    {"op":"add_note","notes":["Decision: ...","Constraint: ...","Next: ..."]}
  ]
}
```

The `apply` response includes `token_usage` (a best-effort estimate) so you can confirm the impact immediately. It also includes
`affected_ids`/`missing_ids` (debugging).

### Supported ops

- `include`, `exclude`, `include_all`
- `delete` (cascade is `tool_outputs`)
- `replace` (tool outputs + reasoning only), `clear_replace`, `clear_replace_all`
- `add_note`, `remove_note`, `clear_notes`
