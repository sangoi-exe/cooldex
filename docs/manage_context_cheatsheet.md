# manage_context - cheat sheet (agent/model)

Use this when the session accumulates large tool outputs/reasoning and you want a mechanical sanitization pass instead of `/compact`.

## Golden rules

- Prefer `replace` over `delete`.
- `replace` is allowed ONLY for: tool outputs + reasoning (never user/assistant messages).
- Tool outputs may be prefixed with `Context left: NN%` (matches the footer; based on the last known token usage); treat it as a rough signal, not a goal.
- Don’t chase a target `context_left_percent`; sanitize by keeping only decision-focused summaries and small essential excerpts.
- When sanitizing, be mechanical: `retrieve` → `apply` → `retrieve`.
- Avoid touching protected items:
  - `<environment_context>...` (environment context)
  - user instructions (AGENTS.md block)
- Prefer targeting by tool `call_id` (pass it in `targets.ids`) to keep call/output pairs consistent.

## 1) Fast diagnosis (cheap)

Call `manage_context`:

```json
{"mode":"retrieve"}
```

Look at:
- `breakdown.top_included_items` (largest offenders)
- `breakdown.top_calls` (largest tool invocations grouped by `call_id`, includes `tool_args_preview`)
- per-item `approx_bytes.effective` (what matters after replacements)
- `breakdown.by_category.reasoning` (reasoning often dominates total size)

## 2) Preferred cleanup plan (order)

1. `consolidate_reasoning` when reasoning dominates (extract included reasoning summaries under `extracted.reasoning.items` and exclude the original reasoning items)
2. `replace` the biggest tool outputs / reasoning with a short distilled summary
3. `exclude` older low-value noise (target by tool `call_id` via `targets.ids` when possible)
4. `add_note` with the few decisions/constraints that must stay visible
5. `delete` only if necessary (prefer by tool `call_id` via `targets.ids`)

## 2a) Emergency: blocked by context (fastest win)

If you can’t proceed due to context pressure, do a single, high-leverage `apply` (replace the biggest tool outputs/reasoning from `breakdown.top_calls` / `breakdown.top_included_items`). If that doesn’t recover enough space, prefer `/compact` over repeated `manage_context` cycles.

```json
{
  "mode":"apply",
  "snapshot_id":"<from retrieve>",
  "ops":[
    {"op":"consolidate_reasoning"},
    {"op":"replace","targets":{"ids":["call_123"]},"text":"Key results: ..."},
    {"op":"replace","targets":{"ids":["r42"]},"text":"Conclusion: ..."},
    {"op":"add_note","notes":["Decision: ...","State: ...","Next: ..."]}
  ]
}
```

## 3) Apply changes (v2, atomic)

Apply directly:

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

Verify:

```json
{"mode":"retrieve"}
```

Tip: `apply` responses also include `token_usage` and `affected_ids`/`missing_ids` (use `retrieve` when you need the breakdown).

If you get `snapshot mismatch`, re-run `retrieve` and retry.

If you repeatedly get `snapshot mismatch` even right after `retrieve`, use a fallback:

- Omit `snapshot_id`, or
- Retry after a fresh `retrieve`.
