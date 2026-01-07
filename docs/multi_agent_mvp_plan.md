# Multi-agent MVP follow-ups (`agent_run`)

This document is a task plan to evolve the current experimental `agent_run` tool into a more robust, reusable, and “background-friendly” multi-agent feature, without relying on `base_instructions`/`developer_instructions` overrides (rubrics must be passed via `UserInput::Text` + `final_output_json_schema`).

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

- [ ] Create a reusable sub-agent runner module (e.g. `core/src/subagent_runner.rs`) that encapsulates:
  - [ ] Spawning a sub-agent conversation with a `SessionSource::SubAgent(SubAgentSource::Other(...))`
  - [ ] Routing `ExecApprovalRequest` / `ApplyPatchApprovalRequest` to the parent session
  - [ ] Shutdown + drain logic (interrupt → shutdown → wait)
  - [ ] Timeout + cancellation behavior
- [ ] Refactor existing call sites to use it:
  - [ ] Review/compact path (currently in `codex_delegate.rs`)
  - [ ] `agent_run` tool handler
- [ ] Add unit tests for the runner:
  - [ ] Approval routing is wired to the parent (no deadlocks)
  - [ ] Timeout always leads to a clean shutdown
  - [ ] Cancelled parent turn aborts the sub-agent promptly

### M2 — Correct config inheritance (“effective turn”)

- [ ] Define exactly what should be inherited by default:
  - [ ] `cwd`
  - [ ] `approval_policy`
  - [ ] `sandbox_policy`
  - [ ] `model` / provider selection
  - [ ] enabled features (with explicit exclusions for recursion)
- [ ] Implement “effective config” derivation (prefer cloning `SessionConfiguration` + any active overrides) and pass it into sub-agent spawn.
- [ ] Add an explicit `agent_run` argument surface for overrides (optional, but explicit):
  - [ ] `cwd` override
  - [ ] `model` override
  - [ ] `approval_policy` / `sandbox_policy` override (if we want it)
- [ ] Verify no provider “instructions validation” regressions:
  - [ ] Rubric only via `UserInput::Text`
  - [ ] Output constrained only via `final_output_json_schema`

### M3 — True background mode (spawn → poll/wait)

- [ ] Add new tools (still model-only; experimental flag):
  - [ ] `agent_spawn`: returns `{ agent_id, rollout_path? }`
  - [ ] `agent_wait`: waits for completion with timeout, returns `{ status, result }`
  - [ ] `agent_status`: returns `{ status, last_message? }`
  - [ ] `agent_cancel`: aborts a running agent
- [ ] Maintain a parent-scoped registry of spawned agents (in memory) so the model can reference them by id.
- [ ] Ensure non-leaky lifecycle:
  - [ ] Agents are removed after `shutdown` unless explicitly retained
  - [ ] Headless event draining is always active for background agents

### M4 — Concurrency and workspace safety

- [ ] Define a concurrency policy:
  - [ ] “Research” agents may run concurrently.
  - [ ] “Execute/mutate” agents require exclusive access to the workspace.
- [ ] Implement a cross-conversation lock shared across sessions (e.g. an `Arc<RwLock<()>>` in the conversation manager / services layer):
  - [ ] Mutating tool calls acquire the write lock.
  - [ ] Non-mutating tool calls acquire the read lock.
- [ ] Update tool handlers’ `is_mutating()` classifications (or add a new “workspace mutating” trait) to ensure correctness.
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

