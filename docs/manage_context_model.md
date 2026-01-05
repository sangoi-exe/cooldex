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
  - Prefer targeting by tool `call_id` (pass it in `targets.ids`) when you want a call + its output(s) treated together.
- Keep outputs small:
  - A "cleanup" operation that emits huge JSON or a full transcript defeats the purpose.

## Preferred workflow: manage_context (v2)

### Step 0: check pressure (cheap)

Call `retrieve` to get a bounded summary:

```json
{
  "mode": "retrieve"
}
```

Use this to decide whether cleanup is needed. A common trigger is low remaining context (for example, <20% left).

### Step 1: pick targets from the summary

Use the cheap `retrieve` breakdown to pick a *small* set of targets:

- `breakdown.top_calls` for the biggest tool call/output pairs (target by tool `call_id` via `targets.ids`).
- `breakdown.top_included_items` for oversized reasoning/tool outputs (target with `ids`).

Avoid pulling full item lists just to find targets. If the summary isn't enough to target what you need, prefer `/compact` over repeatedly expanding context.

### Emergency: recover from ~0% context left

When the context is effectively full, do one high-leverage, reversible `apply`:

- Replace the largest tool outputs / reasoning shown in the `retrieve` breakdown.
- If needed, exclude a few old tool `call_id`s (noise) and add a tiny pinned note with the current state.

If you still can't recover enough space, prefer `/compact` over repeated `manage_context` cycles.

### Step 2: plan operations (prefer non-destructive first)

Recommended order:

1. `consolidate_reasoning` when reasoning dominates (extracts included reasoning summaries under `extracted.reasoning.items` and excludes the original reasoning items).
2. `replace` the largest tool outputs/reasoning with short distilled summaries (target by tool `call_id` via `targets.ids`, or by RID via `targets.ids`).
3. `exclude` older, low-value items (prefer targeting by tool `call_id` via `targets.ids` if you are excluding tool noise).
4. `add_note` for the few facts that must remain visible (decisions, constraints, TODOs).
5. `delete` only when necessary (and prefer deleting by tool `call_id` via `targets.ids` so pairs are removed cleanly).

### Step 3: apply (atomic)

```json
{
  "mode": "apply",
  "snapshot_id": "<from retrieve>",
  "ops": [
    { "op": "replace", "targets": { "ids": ["call_123"] }, "text": "Key results: ..." },
    { "op": "exclude", "targets": { "ids": ["r10", "r11"] } },
    { "op": "add_note", "notes": ["Decision: ...", "Constraint: ...", "Next: ..."] }
  ]
}
```

If you get a `snapshot mismatch`, re-run `retrieve` and retry once.

### Step 4: verify

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
  - If it repeats even immediately after `retrieve`, omit `snapshot_id`.
- `skipped_missing_targets`:
  - Target RID/index/call_id was not present; re-retrieve and retarget.
- `replace only supports tool outputs and reasoning`:
  - You targeted a call or a message; target the output via a tool `call_id` in `targets.ids`, or pick the output RID.
- `targets recent message id(s)`:
  - By default, `exclude`/`delete` refuse to target the most recent user/assistant messages.
  - Set `allow_recent=true` to override.
