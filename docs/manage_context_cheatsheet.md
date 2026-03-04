# manage_context - cheat sheet (v2)

## Flow

1) Retrieve

```json
{"mode":"retrieve","policy_id":"<runtime policy_id>"}
```

2) Apply (only when `chunk_manifest` is non-empty)

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

3) Retrieve again

If `retrieve.chunk_manifest` is empty, skip `apply`.

## Required invariants

- one `<tool_context>` + one `<reasoning_context>` per applied chunk
- send only fields from the current v2 contract
- `retrieve` payload is only `mode` + `policy_id`
- `chunk_id` must exist in current `chunk_manifest`
- `chunk_id` cannot repeat in the same `apply`
- `chunk_summaries` must be non-empty and `<= max_chunks_per_apply`
- `policy_id` must match runtime policy

## stop_reason

`target_reached | fixed_point_reached | invalid_summary_schema | state_hash_mismatch | plan_id_invalid | invalid_contract | rollout_persist_error`
