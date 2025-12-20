Manage Context v2 — Non-interactive retrieve/apply contract
===========================================================

Goal
- Make `manage_context` usable by the agent without an interactive/iterative flow.
- Reduce context pressure by allowing the agent to distill large tool outputs/reasoning into small kept facts.
- Keep safety invariants: tool call/output pairs must never be left inconsistent.

Constraints / Decisions
- `replace` is allowed only for ToolOutput + Reasoning (never user/assistant messages).
- Avoid multi-step “menus”. Target: **2 calls max** in the common path:
  1) `manage_context.retrieve` (read-only) -> returns a single JSON snapshot
  2) `manage_context.apply` (mutating) -> applies a batch of ops atomically
- The `apply` call must be non-interactive: no “ask again”, no incremental prompting; validate all ops up-front.

Why change the current API
- The current `manage_context` supports `status`/`list` and individual actions, but complex cleanups can require many calls (list -> decide -> multiple include/exclude/delete/replace calls).
- In long sessions, this becomes expensive in both time and context.

Proposed tool schema (v2)
- Keep the existing `manage_context` tool name, but add a new top-level field:
  - `mode`: `"retrieve"` | `"apply"`

`retrieve` request
- Args:
  - `mode`: `"retrieve"`
  - `include_items`: bool (default true)
  - `include_notes`: bool (default true)
  - `include_token_usage`: bool (default true)
  - `include_pairs`: bool (default true; include best-effort tool call/output pairing metadata)
  - `max_items`: number (optional; truncate output deterministically, e.g. most recent N)
- Response:
  - `snapshot_id`: string (opaque; can be `sha256` over a stable representation of item ids + overlay counts)
  - `token_usage`: { `model_context_window`, `tokens_in_context`, `context_left_percent` }
  - `items`: array of:
    - `index`, `id` (RID), `category`, `included`, `preview`
    - `call_id` (if any), `tool_name` (if any)
    - `pair`: { `kind`: `"call"`|`"output"`|`"none"`, `pair_call_id`: string? }
    - `replaced`: bool
  - `notes`: array of strings

`apply` request
- Args:
  - `mode`: `"apply"`
  - `snapshot_id`: string (optional but strongly recommended; if provided and mismatched, reject with a clear error)
  - `ops`: array of operations; execute in order within one transaction:
    - `{ "op": "include", "targets": { "ids": [...], "indices": [...], "call_ids": [...] } }`
    - `{ "op": "exclude", "targets": { ... } }`
    - `{ "op": "delete", "targets": { ... }, "cascade": "tool_outputs" }`
      - `cascade` defaults to `"tool_outputs"` so deleting a tool call also deletes outputs.
    - `{ "op": "replace", "targets": { "ids"/"indices"/"call_ids" }, "text": "..." }`
      - Validate target item types are ToolOutput/Reasoning only.
    - `{ "op": "clear_replace", "targets": { ... } }` or `{ "op": "clear_replace_all" }`
    - `{ "op": "add_note", "notes": ["...", "..."] }`
    - `{ "op": "remove_note", "note_indices": [0, 2] }`
    - `{ "op": "clear_notes" }`
    - `{ "op": "include_all" }`
  - `dry_run`: bool (optional; if true, return computed changes without mutating)
- Response:
  - `ok`: bool
  - `applied`: summary counts (included/excluded/deleted/replaced/notes)
  - `new_snapshot_id`: string (if mutations happened)

Implementation sketch (codex-core)
1) Schema: extend `create_manage_context_tool()` to accept `mode` + `ops`.
2) Handler:
   - Add a `retrieve` code path that returns one JSON snapshot (no side effects).
   - Add `apply` that:
     - resolves all targets -> concrete indices/RIDs
     - expands deletes to include paired outputs (tool call -> outputs) before mutating
     - validates all replace targets types (ToolOutput/Reasoning only)
     - applies ops to `SessionState` in-memory
     - persists rollout snapshots at the end:
       - `RolloutItem::ContextInclusion` (for inclusion mask + deletions)
       - `RolloutItem::ContextOverlay` (for replacements + notes)
3) Keep backward compatibility:
   - Either:
     - keep old `action`-based API as a compatibility layer; or
     - interpret `action` as shorthand mapping to a single-op `apply`.
4) Add a small safety net:
   - keep the prompt normalization pass (`normalize_tool_call_pairs`) to avoid orphan outputs even if something slips.

Acceptance criteria
- Agent can do: `retrieve` -> decide -> `apply` (one call) to clean context.
- Deleting tool calls never leaves orphan outputs (no session errors).
- Resume/fork preserves overlays and inclusion/deletions deterministically.

Notes / gotchas
- Prompt validation: do not modify core prompt text for tool usage guidance; rely on tool schema/description or user instructions.
- Keep `manage_context` output JSON bounded; add `max_items` and deterministic truncation when needed.

