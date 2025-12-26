# manage_context - cheat sheet (agent/model)

Use this when you see context pressure (low "context left") and want targeted cleanup instead of `/compact`.

## Golden rules

- Prefer `replace` over `delete`.
- `replace` is allowed ONLY for: tool outputs + reasoning (never user/assistant messages).
- Tool outputs may be prefixed with `Context left: NN%` (matches the footer; based on the last known token usage); treat it as an early-warning signal.
- `include_items=true` can itself add a lot of text; start with `include_items=false` unless you need to target items.
- Avoid touching protected items:
  - `<environment_context>...` (environment context)
  - user instructions (AGENTS.md block)
- Prefer targeting by `call_id` to keep call/output pairs consistent.

## 1) Fast diagnosis (cheap)

Call `manage_context`:

```json
{"mode":"retrieve","include_items":false}
```

If context is tight, do a bounded inspect:

```json
{"mode":"retrieve","include_items":true,"max_items":200,"include_pairs":true}
```

Look at:
- `breakdown.top_included_items` (largest offenders)
- `breakdown.top_calls` (largest tool invocations grouped by `call_id`, includes `tool_args_preview`)
- per-item `approx_bytes.effective` (what matters after replacements)
- `breakdown.by_category.reasoning` (reasoning often dominates total size)

## 2) Preferred cleanup plan (order)

1. `replace` the biggest tool outputs / reasoning with a short distilled summary
2. `exclude` older low-value noise (use `call_ids` if possible)
3. `add_note` with the few decisions/constraints that must stay visible
4. `delete` only if necessary (prefer by `call_ids`)

## 2a) Emergency: context_left ~0% (fastest win)

If you're basically out of context, a high-leverage, reversible move is to **exclude all included `reasoning` items** (they can be hundreds of items and dwarf tool output).

- Reversible: `include_all` brings them back.
- Always follow up by setting a small `notes` block with the current state (repo, constraints, what's done, what's next).
- This can recover a lot of space quickly (e.g. ~0% → ~70% in a long session).

Pseudo-apply (agent/model computes the reasoning indices from `retrieve`):

```json
{
  "mode":"apply",
  "snapshot_id":"<from retrieve>",
  "ops":[
    {"op":"exclude","targets":{"indices":[/* all included reasoning indices */]}},
    {"op":"add_note","notes":[
      "Repo: ...",
      "Decision: ...",
      "Constraint: ...",
      "Next: ..."
    ]}
  ]
}
```

## 3) Apply changes (v2, atomic + anti-drift)

Dry-run first:

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

Then apply (same ops, without `dry_run`):

```json
{
  "mode":"apply",
  "snapshot_id":"<from retrieve>",
  "ops":[
    {"op":"replace","targets":{"call_ids":["call_123"]},"text":"Key results: ..."},
    {"op":"exclude","targets":{"ids":["r10","r11"]}},
    {"op":"add_note","notes":["Decision: ...","Constraint: ...","Next: ..."]}
  ]
}
```

Verify:

```json
{"mode":"retrieve","include_items":false}
```

Tip: `apply` responses also include `token_usage` and `affected_ids`/`missing_ids` (use `retrieve` when you need the breakdown/items list).

If you get `snapshot mismatch`, re-run `retrieve` and retry.

If you repeatedly get `snapshot mismatch` even right after `retrieve`, use a fallback:

- Omit `snapshot_id` (still supports `dry_run`), or
- Retry after a fresh `retrieve`.

Example (v2 apply without `snapshot_id`):

```json
{
  "mode":"apply",
  "dry_run":true,
  "ops":[
    {"op":"replace","targets":{"call_ids":["call_123"]},"text":"Key results: ..."},
    {"op":"add_note","notes":["Decision: ...","Constraint: ...","Next: ..."]}
  ]
}
```
