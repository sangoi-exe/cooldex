# Self Prompt — Session Restart (Manage Context / Sanitize)

## Mission
Resume from a clean session and finish `manage_context`/`/sanitize` hardening on branch `reapply/accounts-20260209`.

## Current Ground Truth
- Repo: `/home/lucas/work/codex`
- Branch: `reapply/accounts-20260209`
- HEAD when this prompt was generated: `9f4fe487f`
- Worktree: clean for code; only untracked forensic artifacts under `.sangoi/**`.

Untracked artifacts currently present:
- `.sangoi/reports/2026-02-14-manage-context-sanitize-full-forensics.md`
- `.sangoi/plans/2026-02-14-manage-context-sanitize-forensics.md`
- `.sangoi/plans/2026-02-14-lane-b-good-runtime-flow-audit.md`
- `.sangoi/plans/plan-report-qa-manage-context-sanitize.md`
- `.sangoi/self-prompts/2026-02-14-resume-manage-context-fix.md`

Backup copy (safe path outside detached-risk context):
- `/home/lucas/.codex/2026-02-14-manage-context-sanitize-full-forensics.md`

## What Is Already Established
1. GOOD vs BAD forensic mapping is done and documented.
2. Rollout evidence confirmed:
- BAD case (`2026-02-14T18:06:30` → `18:07:10`): `retrieve ok` then `apply` fails with `state_hash_mismatch`, then sanitize stalls.
- Legacy loop case (`2026-02-14T01:49` → `01:51`): many `apply ok:true` happened before final `snapshot mismatch` loop stop.
3. Report was corrected to avoid over-claiming certainty where evidence is partial.

## Critical Constraints (Do Not Violate)
- Root-cause fixes only; no compatibility shims/fallback glue for renamed/removed contract.
- Fail-loud behavior only (no silent parameters, no hidden retries that mask defects).
- Keep `retrieve/apply` contract internally coherent (same context boundary for anti-drift/hash/plan inputs).
- If behavior changes, update docs in the same change set.

## Immediate Execution Plan
1. Recon quickly:
- `git status --short --branch`
- verify target files still match expected state.

2. Re-validate current implementation on this HEAD:
- `cd codex-rs && cargo test -p codex-core --lib manage_context -- --test-threads=1`
- `cd codex-rs && cargo test -p codex-core --lib sanitize -- --test-threads=1`

3. If failing or contract-drift found:
- patch `codex-rs/core/src/tools/handlers/manage_context.rs` first (anti-drift/hash/plan consistency),
- then patch `codex-rs/core/src/tasks/sanitize.rs` only if needed for error/stall clarity,
- add/adjust regression tests in same files.

4. Validate after patch:
- `cd codex-rs && just fmt`
- rerun focused tests above
- `cd codex-rs && cargo build -p codex-core`

5. Update docs if runtime contract changed:
- `docs/manage_context.md`
- `docs/manage_context_model.md`
- `codex-rs/core/sanitize_prompt.md` (only if tool usage contract text changed)

## Files To Open First
- `codex-rs/core/src/tools/handlers/manage_context.rs`
- `codex-rs/core/src/tasks/sanitize.rs`
- `codex-rs/core/src/tools/spec.rs`
- `codex-rs/core/sanitize_prompt.md`
- `.sangoi/reports/2026-02-14-manage-context-sanitize-full-forensics.md`

## Known Gaps To Close (If Not Already)
- Explicit tests for:
- replace rejecting user/assistant message targets.
- protected categories (`environment_context`, `user_instructions`) blocked for include/exclude/delete.
- delete preserving call/output pair integrity.
- sanitize seed selection (`collect_recent_manage_context_items`) and toolset restriction.
- sanitize reasoning effort fallback behavior.

## Done Criteria (Strict)
- Focused tests pass (`manage_context`, `sanitize`) + `cargo build -p codex-core`.
- No ambiguous claim in final report/summary.
- Any behavior/contract change reflected in docs.
- Final handoff includes exact changed files and residual risks.

## Quick Command Block
```bash
git status --short --branch
rg -n "state_hash_for_context|collect_top_offenders|chunk_manifest|plan_id_invalid|state_hash mismatch" codex-rs/core/src/tools/handlers/manage_context.rs codex-rs/core/src/tasks/sanitize.rs
cd codex-rs && cargo test -p codex-core --lib manage_context -- --test-threads=1
cd codex-rs && cargo test -p codex-core --lib sanitize -- --test-threads=1
```
