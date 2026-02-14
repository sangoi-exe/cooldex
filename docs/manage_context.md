## Context management (`manage_context`)

`manage_context` now uses a strict v2 contract with only two modes: `retrieve` and `apply`.

Use it when large `tool_output` / `reasoning` chunks are pressuring context.

### Contract (v2 only)

Request fields:
- `mode`: `"retrieve" | "apply"` (required)
- `policy_id`: required in both modes (must match runtime `manage_context_policy.quality_rubric_id`)
- `plan_id`: required in `apply`
- `state_hash`: required in `apply`
- `chunk_summaries`: required in `apply` (non-empty and bounded by runtime `max_chunks_per_apply`)

`retrieve` accepts only `mode` + `policy_id`; sending `plan_id`, `state_hash`, or `chunk_summaries` in `retrieve` is invalid.

`chunk_summaries[]` item:
- `chunk_id` (required, unique in payload, and must exist in current `chunk_manifest`)
- `tool_context` (required)
- `reasoning_context` (required)

Legacy fields are intentionally invalid: `snapshot_id`, `new_snapshot_id`, `ops`, `max_top_items`, `include_prompt_preview`, `allow_recent`.

### `retrieve`

Example:

```json
{"mode":"retrieve","policy_id":"<runtime policy_id>"}
```

Returns:
- `plan_id`
- `state_hash`
- `policy_id`
- `chunk_manifest`
- `top_offenders`
- `convergence_policy`
- `progress_report`

`chunk_manifest` is the source of truth for `chunk_id`s to summarize.

### `apply`

Example:

```json
{
  "mode":"apply",
  "policy_id":"<runtime policy_id>",
  "plan_id":"<from retrieve>",
  "state_hash":"<from retrieve>",
  "chunk_summaries":[
    {
      "chunk_id":"chunk_001",
      "tool_context":"Key tool result in one concise paragraph.",
      "reasoning_context":"Decision rationale and constraints."
    }
  ]
}
```

Behavior per chunk:
- validates `chunk_id` against current `chunk_manifest`
- always emits exactly one `<tool_context>` and one `<reasoning_context>` tied to that chunk
- applies either replacement (when summary is compact enough) or exclusion for the chunk source

Response includes:
- `applied_events`
- `new_state_hash`
- `progress_report`
- `stop_reason`

### `stop_reason` values

- `target_reached`
- `fixed_point_reached`
- `invalid_summary_schema`
- `state_hash_mismatch`
- `plan_id_invalid`
- `invalid_contract`

For model-facing guidance, see `docs/manage_context_model.md`.
