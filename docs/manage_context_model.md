# manage_context: model playbook (in-session workflow)

This document is a practical, step-by-step guide for an agent/model to keep long Codex sessions healthy by shrinking prompt context pressure using the `manage_context` tool.

For a short checklist, see `docs/manage_context_cheatsheet.md`. For an overview, see `docs/manage_context.md`.

## Goals

- Prevent context-window blowups by proactively reducing what stays in the prompt.
- Preserve correctness by keeping key decisions/constraints visible as short pinned notes.
- Avoid destructive history loss unless absolutely necessary.

## Signals to watch

- Tool outputs may be prefixed with `Context left: NN%` (matches the footer; based on the last known token usage); use it as an early-warning signal.
- The most reliable view is `manage_context` `mode=retrieve` (token_usage is included).

## Glossary

- **History item**: one `ResponseItem` in the session transcript (message, tool call/output, reasoning, etc.).
- **Index**: 0-based history position.
- **RID**: stable per-item id, formatted as `r<integer>` (e.g. `r42`). Used to target items reliably across steps.
- **call_id**: tool call identifier; useful to target a call plus its output(s) as a unit.
- **include mask**: optional set of indices that are included in the prompt; items outside are excluded (non-destructive).
- **overlay**: prompt-only transformations:
  - replacements: short text keyed by RID to replace large tool outputs/reasoning
  - notes: pinned notes inserted near the start of the prompt
- **snapshot_id**: hash of (items + inclusion + overlay). Used to detect drift between `retrieve` and `apply`.

## When to use what

- Use **`manage_context`** when the model is available and you want a safe/atomic workflow.
- Use **`/compact`** when you want a full reset/summarization of the conversation.
  - Note: a compaction rebuild resets history shape; do compaction first, then re-run `manage_context` if needed.

## Hard rules (safety + invariants)

- Prefer **replace** over delete:
  - `replace` is non-destructive and keeps a distilled summary visible to the model.
- **Replace is allowed only for ToolOutput and Reasoning** (never for user/assistant messages).
- Avoid excluding or deleting **protected items**:
  - environment context (`<environment_context>...`) and user instructions (AGENTS.md block) are treated as protected.
- Tool calls and outputs have pairing invariants:
  - If you keep a call but drop its output (or vice-versa), the prompt may be normalized to avoid orphaned items.
  - Prefer targeting by `call_id` when you want a tool call + its output(s) treated together.
- Keep outputs small:
  - A "cleanup" operation that emits huge JSON or a full transcript defeats the purpose.

## Preferred workflow: manage_context (v2)

### Step 0: check pressure (cheap)

Call `retrieve` with `include_items=false` to get a bounded summary:

```json
{
  "mode": "retrieve",
  "include_items": false
}
```

Use this to decide whether cleanup is needed. A common trigger is low remaining context (for example, <20% left).

### Step 1: inspect a bounded window (optional)

If you need to identify targets, ask for a bounded item list:

```json
{
  "mode": "retrieve",
  "include_items": true,
  "max_items": 200,
  "include_pairs": true
}
```

Use:

- `breakdown.top_calls` to quickly spot the biggest tool invocations grouped by `call_id` (includes `tool_name` and `tool_args_preview`).
- `breakdown.top_included_items` and per-item `approx_bytes.effective` to find the biggest offenders.
- `breakdown.by_category` to see which category is dominating (commonly: `reasoning`).

If you need to operate across the full transcript (not just the most recent window), raise `max_items` (for example to `5000`).

### Emergency: recover from ~0% context left (reasoning purge)

When the context is effectively full, a fast, reversible win is often to **exclude all included `reasoning` items** (reasoning can dwarf tool output).

1. `retrieve` with items included (and a large `max_items`).
2. Collect indices of items where `category=="reasoning"` and `included==true`.
3. `apply` an `exclude` op for those indices.
4. Add short pinned notes with the state you must retain.
5. Verify `context_left_percent` increased.

This is reversible via `include_all`. In one long session, excluding ~200 reasoning items raised context left from ~0% to ~70%.

### Step 2: plan operations (prefer non-destructive first)

Recommended order:

1. `replace` the largest tool outputs/reasoning with short distilled summaries (target by `call_ids` or `ids`).
2. `exclude` older, low-value items (prefer targeting by `call_ids` if you are excluding tool noise).
3. `add_note` for the few facts that must remain visible (decisions, constraints, TODOs).
4. `delete` only when necessary (and prefer deleting by `call_ids` so pairs are removed cleanly).

### Step 3: dry-run (anti-drift)

Always dry-run first when possible:

```json
{
  "mode": "apply",
  "snapshot_id": "<from retrieve>",
  "dry_run": true,
  "ops": [
    { "op": "replace", "targets": { "call_ids": ["call_123"] }, "text": "Key results: ..." },
    { "op": "exclude", "targets": { "ids": ["r10", "r11"] } },
    { "op": "add_note", "notes": ["Decision: ...", "Constraint: ...", "Next: ..."] }
  ]
}
```

Optional: set `include_prompt_preview=true` to get a truncated preview of the prompt after applying the ops.

If you get a `snapshot mismatch`, re-run `retrieve` and rebuild the plan.

### Step 4: apply for real

Repeat the same payload with `dry_run=false` (or omit it):

```json
{
  "mode": "apply",
  "snapshot_id": "<from retrieve>",
  "ops": [
    { "op": "replace", "targets": { "call_ids": ["call_123"] }, "text": "Key results: ..." },
    { "op": "exclude", "targets": { "ids": ["r10", "r11"] } },
    { "op": "add_note", "notes": ["Decision: ...", "Constraint: ...", "Next: ..."] }
  ]
}
```

### Step 5: verify

Check `token_usage` in the `apply` response (it should improve).

If you see `skipped_missing_targets > 0`, check `missing_ids` to see which RIDs didn't resolve.

Then run `retrieve` again and confirm:

- the new `snapshot_id` changed
- `tokens_in_context` dropped (or `context_left_percent` increased)
- the notes look correct and minimal

## Writing good replacement text

Replacement text should be short and "decision-focused". A good template:

- What was executed (command/tool + high-level intent)
- Key outputs (paths changed, results, conclusions)
- Any critical numbers/IDs needed later
- Follow-ups (next steps / TODOs)

Avoid:

- raw logs, stack traces, or multi-page output
- sensitive tokens/credentials
- repeating the entire conversation

## Troubleshooting

- `snapshot mismatch` (manage_context v2):
  - The transcript changed between `retrieve` and `apply`; run `retrieve` again.
  - If it repeats even immediately after `retrieve`, omit `snapshot_id` (use `dry_run`).
- `skipped_missing_targets`:
  - Target RID/index/call_id was not present; re-retrieve and retarget.
- `replace only supports tool outputs and reasoning`:
  - You targeted a call or a message; target the output via `call_ids` or pick the output RID.
- `targets recent message id(s)`:
  - By default, `exclude`/`delete` refuse to target the most recent user/assistant messages.
  - Set `allow_recent=true` to override.
