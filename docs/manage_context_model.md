# manage_context: model playbook (v2)

Use `manage_context` to sanitize heavy context with deterministic `retrieve -> apply` cycles.

If the goal is to inspect recent pre-compaction history (reasoning + assistant messages only), use `recall` instead of `manage_context`.

## Hard rules

- Use only v2 fields.
- Always send `policy_id`.
- `retrieve` payload must include only `mode` and `policy_id`.
- For `apply`, always send `plan_id + state_hash` from the latest `retrieve`.
- Only use `chunk_id`s returned in the latest `chunk_manifest`.
- `chunk_summaries` must be non-empty and `len <= max_chunks_per_apply` from runtime policy.
- Each `chunk_summaries[]` entry must include non-empty:
  - `chunk_id`
  - `tool_context`
  - `reasoning_context`
- Do not repeat `chunk_id` in the same `apply` payload.

Do not send legacy fields (`snapshot_id`, `new_snapshot_id`, `ops`, `max_top_items`, `include_prompt_preview`, `allow_recent`).

## Loop

1) Retrieve:

```json
{"mode":"retrieve","policy_id":"<runtime policy_id>"}
```

2) Summarize selected chunks from `chunk_manifest` (up to runtime `max_chunks_per_apply`).

3) Apply:

```json
{
  "mode":"apply",
  "policy_id":"<runtime policy_id>",
  "plan_id":"<from retrieve>",
  "state_hash":"<from retrieve>",
  "chunk_summaries":[
    {
      "chunk_id":"chunk_001",
      "tool_context":"...",
      "reasoning_context":"..."
    }
  ]
}
```

4) Retrieve again and continue until fixed point.

## Quality

- `tool_context`: factual, compact, execution-relevant.
- `reasoning_context`: concise rationale, constraints, next implications.
- Avoid fluff and repetition.
