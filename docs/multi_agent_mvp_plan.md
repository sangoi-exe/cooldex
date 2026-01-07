# Multi-agent MVP follow-ups (`agent_run`)

This document is a task plan to evolve the current experimental `agent_run` tool into a more robust, reusable, and “background-friendly” multi-agent feature, without relying on `base_instructions`/`developer_instructions` overrides (rubrics must be passed via `UserInput::Text` + `final_output_json_schema`).

## Status

As of 2026-01-07:

- M1 is done (shared approval routing + shutdown helper extracted; call sites refactored).
- M2 is partially done (`agent_run` inherits the effective turn settings; still needs generalization + explicit overrides).
- M3 is done (agent_spawn/wait/status/cancel implemented + registry + lifecycle cleanup).
- M4 is in progress (shared workspace lock wired into tool dispatch; policy refinement pending).

## Goals

- Reuse shared sub-agent wiring across `agent_run`, `/sanitize`, review, and compact.
- Make sub-agents inherit the *effective* session/turn configuration (model/cwd/policies), not just the original config.
- Support true background execution (spawn → poll/wait) so the main agent can continue while sub-agents work.
- Enforce clear concurrency semantics to avoid multiple agents mutating the workspace simultaneously.
- Fail fast on invalid output schemas and over-large results.
- Improve observability (start/finish events, timing, debug handles).

## Non-goals (for this iteration)

- A full TUI agent dashboard (nice-to-have; can come later).
- Persisting agent state across process restarts (would require durable registry).

## Milestones

### M1 — Extract a shared “sub-agent runner”

- [x] Create a reusable sub-agent runner module (`core/src/subagent_runner.rs`) that encapsulates:
  - [x] Routing `ExecApprovalRequest` / `ApplyPatchApprovalRequest` to the parent session
  - [x] Shutdown + drain logic (interrupt → shutdown → wait)
  - [x] Cancellation-aware approval routing (abort on cancellation)
  - [ ] Spawning a sub-agent conversation (still owned by call sites for now)
  - [ ] Shared “run until completion” loop (still owned by call sites for now)
- [x] Refactor existing call sites to use it:
  - [x] Review/compact path (currently in `codex_delegate.rs`)
  - [x] `agent_run` tool handler
- [x] Add unit tests for the runner:
  - [x] Approval routing is wired to the parent (no deadlocks)
  - [x] Cancelled parent turn aborts sub-agent approvals promptly
  - [ ] Timeout always leads to a clean shutdown (nice-to-have: add a deterministic test)

### M2 — Correct config inheritance (“effective turn”)

- [ ] Define exactly what should be inherited by default:
  - [x] `cwd`
  - [x] `approval_policy`
  - [x] `sandbox_policy`
  - [x] `model` / provider selection
  - [ ] enabled features (with explicit exclusions for recursion; currently we only hard-disable `multi_agent` in sub-agents)
- [ ] Implement “effective config” derivation (prefer cloning `SessionConfiguration` + any active overrides) and pass it into sub-agent spawn:
  - [x] `agent_run`: inherit effective turn settings (cwd/model/provider/policies) with fail-fast validation
  - [ ] Generalize inheritance for all sub-agents (sanitize/review/compact) via a shared helper
- [ ] Add an explicit `agent_run` argument surface for overrides (optional, but explicit):
  - [ ] `cwd` override
  - [ ] `model` override
  - [ ] `approval_policy` / `sandbox_policy` override (if we want it)
- [ ] Verify no provider “instructions validation” regressions:
  - [ ] Rubric only via `UserInput::Text`
  - [ ] Output constrained only via `final_output_json_schema`

### M3 — True background mode (spawn → poll/wait)

- [x] Add new tools (still model-only; experimental flag):
  - [x] `agent_spawn`: returns `{ agent_id }`
  - [x] `agent_wait`: waits for completion with timeout, returns `{ status, snapshot }`
  - [x] `agent_status`: returns `{ snapshot }`
  - [x] `agent_cancel`: aborts a running agent
- [x] Maintain a parent-scoped registry of spawned agents (in memory) so the model can reference them by id.
- [ ] Ensure non-leaky lifecycle:
  - [x] Agents are removed after `shutdown` unless explicitly retained
  - [x] Headless event draining is always active for background agents

### M4 — Concurrency and workspace safety

- [ ] Define a concurrency policy:
  - [ ] “Research” agents may run concurrently.
  - [ ] “Execute/mutate” agents require exclusive access to the workspace.
- [x] Implement a cross-conversation lock shared across sessions (e.g. an `Arc<RwLock<()>>` in the conversation manager / services layer):
  - [x] Mutating tool calls acquire the write lock.
  - [x] Non-mutating tool calls acquire the read lock.
- [x] Update tool handlers’ `is_mutating()` classifications (or add a new “workspace mutating” trait) to ensure correctness.
- [ ] Add fail-fast errors when policy is violated (no silent fallback).

### M5 — Schema validation + result size discipline

- [ ] Validate/sanitize `result_schema` before sending it to the provider:
  - [ ] Reject empty or invalid schemas with a clear error
  - [ ] Run the existing schema sanitizer where appropriate
- [ ] Make `max_result_bytes` enforceable in a predictable way:
  - [ ] Reject oversized results with guidance to re-run with a tighter schema
  - [ ] Optionally support truncation only if it remains valid JSON (prefer fail-fast)

### M6 — Observability + UX for the parent session

- [ ] Emit parent-visible events (non-spammy):
  - [ ] `BackgroundEvent`: “agent started” (id + label)
  - [ ] `BackgroundEvent`: “agent finished” (status + duration)
  - [ ] Optionally: periodic heartbeats for very long runs
- [ ] Include timing + metadata in tool outputs:
  - [ ] `elapsed_ms`
  - [ ] `rollout_path` (debug)
  - [ ] `model` used (if override is supported)

## Acceptance criteria (provisional)

- Sub-agent orchestration code is not duplicated across features.
- A sub-agent can be spawned, run tools (including approvals), and return a JSON result without polluting the parent context.
- Background agents do not accumulate unbounded event queues.
- Concurrency rules prevent two agents from mutating the same workspace at the same time.
