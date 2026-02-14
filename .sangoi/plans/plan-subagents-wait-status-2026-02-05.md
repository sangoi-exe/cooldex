# Plan: sub-agents — `wait` status reporting

Plan level: **hard** (cross-cutting: `core` + `exec` + `tui` + `app-server`)

## Problem statement
Today, when the root agent calls the collab tool `wait`, the returned status information is often unhelpful — especially on timeout:

- Core tool result can be `{ "status": {}, "timed_out": true }`.
- `CollabWaitingEndEvent` is emitted with `statuses = {}` on timeout.
- Downstream consumers derive `receiverThreadIds` from `statuses.keys()`, so on timeout the UI/logs lose *which agents were waited on*.
  - App-server: completed `collabAgentToolCall.receiverThreadIds` becomes `[]`.
  - TUI: shows `Wait complete` + `agents: none` (no explicit “timed out”, no receiver ids).

## Desired behavior (needs confirmation)
Pick one:

### Option A — minimal, backwards-compatible
- Keep the tool contract (`status` only includes final statuses; empty on timeout).
- Fix presentation so the root can still see:
  - the list of `ids` that were waited on, and
  - an explicit “timed out” indicator.

### Option B — recommended, more informative
- On `wait` completion (success *or* timeout), report the **latest status for every requested id** (including `pending_init`/`running`).
- `timed_out` means: “no requested agent is in a final status yet”.
- UI/logging always shows receiver ids + per-agent statuses; no more `receiverThreadIds: []` for a real wait call.

Decision: ✅ **Option B** (confirmed 2026-02-05)

Notes:
- Pain point confirmed: **TUI + tool output**.

## Execution plan (after you confirm A vs B)
- [x] Reproduce the current behavior (parsed root rollout JSONL; confirmed `wait` omits running agents and returns empty `status` on timeout).
- [x] Implement Option B:
  - [x] `codex-rs/core/src/tools/handlers/collab.rs` (`wait` returns latest status for every requested id; `timed_out` when none are final)
  - [x] `codex-rs/core/src/tools/spec.rs` (updated `wait` tool description)
  - [x] Consumers:
    - [x] `codex-rs/exec/src/event_processor_with_human_output.rs` (no longer relies on `statuses.is_empty()`; shows “timed out” when none are final)
    - [x] `codex-rs/tui/src/collab.rs` (shows “Wait timed out” + per-agent statuses on timeout)
    - [x] `codex-rs/app-server/src/bespoke_event_handling.rs` (no code change needed; fixed indirectly because wait-end `statuses` now always includes all requested ids)
    - [x] `codex-rs/exec/src/event_processor_with_jsonl_output.rs` (no code change needed; will now include per-agent states for all requested ids)
- [x] Tests:
  - [x] Core: updated `wait_times_out_when_status_is_not_final` + added `wait_returns_latest_status_for_every_requested_agent`
  - [x] TUI: added focused unit tests for timeout/title behavior in `collab.rs`
- [x] Verification:
  - [x] `cd codex-rs && just fmt`
  - [x] `cd codex-rs && cargo test -p codex-protocol`
  - [x] `cd codex-rs && cargo test -p codex-core --lib` (note: `cargo test -p codex-core` integration suite fails in this environment)
  - [x] `cd codex-rs && cargo test -p codex-exec`
  - [x] `cd codex-rs && cargo test -p codex-tui collab::tests`

## Fan-out lanes (if we want speed)
- Lane A (core): `wait` semantics + tests (`codex-rs/core/**`)
- Lane B (UI): TUI + exec output behavior (`codex-rs/tui/**`, `codex-rs/exec/**`)
- Lane C (app-server): event → thread item mapping (`codex-rs/app-server/**`)

## DONE criteria
- On timeout, the root-visible output is unambiguous:
  - always identifies the waited-on agent ids, and
  - shows either (A) an explicit timeout indicator or (B) per-id statuses (including `running`).
- No regressions: `wait` still returns promptly when any requested agent reaches a final status.
- Formatting + targeted tests are green.
