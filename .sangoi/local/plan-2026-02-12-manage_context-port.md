# Plan — `manage_context` v2 reapply (current upstream)

Label: **hard**

## Objective
- Reapply `manage_context` v2 with fail-loud behavior on current `upstream/main`, preserving `retrieve/apply`, anti-drift `snapshot_id`, atomic ops, and `consolidate_reasoning`.

## Locked constraints
- No commit/push in this phase.
- Root-cause implementation only (no compatibility shims/workarounds).
- Keep `/accounts` lane untouched unless contract integrity requires it.
- Keep `/debug-config` and `/clean` available while task runs.
- Validate with non-zero pass counts (`passed > 0`).

## Recon summary
- `manage_context` tool wiring is absent in `core/src/tools/spec.rs` and handler registry.
- Backup handler depends on removed state primitives (`ContextItemSummary`, `ContextOverlay`, RID helpers).
- Current `SessionState` lacks inclusion/replacement overlay/rid tracking.
- Sampling path currently uses raw `sess.clone_history().for_prompt(...)`.
- Protocol rollout no longer carries context overlay/inclusion variants.

## Contract gate (must resolve before coding)
- Decide one target contract and document it:
  - **Option A — strict parity** with backup behavior (including resume persistence).
  - **Option B — runtime/session-only parity** (no resume persistence for mask/overlay; explicit docs delta).
- Recommendation: **Option B** (lower blast radius, aligned with removed rollout variants).

## Decision (implementation shape)
- Use **SessionState-scoped RID/inclusion/overlay plumbing** plus state-aware prompt snapshots.
- Keep ContextManager changes minimal.
- Enforce invariants:
  - snapshot mismatch is explicit and fail-loud.
  - self-generated `manage_context` call/output must be ignored in snapshot hashing.
  - protected/recent target guards block invalid prune ops unless explicitly overridden.

## Inventory
- **Create**
  - `codex-rs/core/src/state/context.rs`
  - `codex-rs/core/src/tools/handlers/manage_context.rs`
  - `codex-rs/core/src/rid.rs`
  - `docs/manage_context.md`
  - `docs/manage_context_cheatsheet.md`
  - `docs/manage_context_model.md`
- **Modify**
  - `codex-rs/core/src/state/mod.rs`
  - `codex-rs/core/src/state/session.rs`
  - `codex-rs/core/src/codex.rs`
  - `codex-rs/core/src/lib.rs`
  - `codex-rs/core/src/tools/spec.rs`
  - `codex-rs/core/src/tools/handlers/mod.rs`
  - `.sangoi/docs/guide-reapply-manage_context.md`
  - `docs/manage_context.md` (if restored from backup, then align with final behavior)
  - `docs/manage_context_cheatsheet.md` (if restored from backup, then align with final behavior)
  - `docs/manage_context_model.md` (if restored from backup, then align with final behavior)
- **Delete**
  - none expected.

## Execution order (sequential dependencies)
- [x] 0) Lock contract and doc delta
  - Done when:
    - Option A/B is explicitly chosen and recorded in `.sangoi` guide.
  - Verify:
    - `rg -n "Option A|Option B|runtime/session-only|strict parity|resume" /home/lucas/work/codex/.sangoi/docs/guide-reapply-manage_context.md`

- [x] 1) Build op/field matrix from backup contract
  - Done when:
    - Matrix includes all args/ops/error cases: `mode`, `max_top_items`, `snapshot_id`, `ops`, `include_prompt_preview`, `allow_recent`.
    - Matrix is written into `.sangoi/docs/guide-reapply-manage_context.md`.
  - Verify:
    - `git show backup/reapply-state-20260209-125806:codex-rs/core/src/tools/handlers/manage_context.rs | nl -ba | sed -n '1,340p'`

- [x] 2) Add state primitives + RID utilities
  - Done when:
    - `SessionState` exposes snapshot/inclusion/overlay APIs required by handler.
    - RID parse/format helpers exist with tests.
  - Verify:
    - `cd /home/lucas/work/codex/codex-rs && run_checked mc_state cargo test -p codex-core state::session -- --quiet`

- [x] 3) Route sampling through state-aware prompt snapshot
  - Done when:
    - Sampling path no longer bypasses state inclusion/overlay.
    - Prompt estimation uses equivalent context view.
  - Verify:
    - `cd /home/lucas/work/codex/codex-rs && run_checked mc_prompt cargo test -p codex-core prompt_caching -- --quiet`
    - `rg -n "for_prompt\\(|get_estimated_token_count" /home/lucas/work/codex/codex-rs/core/src/codex.rs`

- [x] 4) Add tool schema + registry wiring
  - Done when:
    - Tool schema includes full args (including `max_top_items`).
    - Spec + handler registry are wired and tests updated for new tool presence.
  - Verify:
    - `cd /home/lucas/work/codex/codex-rs && run_checked mc_model_tools cargo test -p codex-core model_tools -- --quiet`

- [x] 5) Port handler + focused invariants tests
  - Done when:
    - `retrieve` returns snapshot/breakdown/token usage/header hints.
    - `apply` enforces atomic ops + anti-drift + missing target accounting.
    - `consolidate_reasoning` works and fails loud when empty.
    - snapshot hash ignores manage_context self-noise.
  - Verify:
    - `cd /home/lucas/work/codex/codex-rs && run_checked mc_handler cargo test -p codex-core manage_context -- --quiet`

- [ ] 6) Resume behavior gate
  - Done when:
    - Resume semantics for chosen Option A/B are tested and documented explicitly with dedicated `manage_context_resume*` tests.
  - Verify:
    - `cd /home/lucas/work/codex/codex-rs && run_checked mc_resume cargo test -p codex-core manage_context_resume -- --quiet`
  - Blocked currently:
    - Local test execution requires Linux `libcap` development headers (`sys/capability.h`) unavailable in this environment.

- [x] 7) Sync docs (repo docs + guide docs)
  - Done when:
    - `docs/manage_context*.md` and `.sangoi` guide match implemented behavior and deltas.
  - Verify:
    - `test -f /home/lucas/work/codex/docs/manage_context.md`
    - `test -f /home/lucas/work/codex/docs/manage_context_cheatsheet.md`
    - `test -f /home/lucas/work/codex/docs/manage_context_model.md`
    - `rg -n "snapshot_id|consolidate_reasoning|allow_recent|max_top_items|fail-loud" /home/lucas/work/codex/docs/manage_context*.md /home/lucas/work/codex/.sangoi/docs/guide-reapply-manage_context.md`

- [ ] 8) Formatting + focused validation gate
  - Done when:
    - formatting is clean and all focused filters pass with non-zero counts.
  - Verify:
    - `cd /home/lucas/work/codex/codex-rs && just fmt`
    - `cd /home/lucas/work/codex/codex-rs && run_checked mc_state cargo test -p codex-core state::session -- --quiet`
    - `cd /home/lucas/work/codex/codex-rs && run_checked mc_prompt cargo test -p codex-core prompt_caching -- --quiet`
    - `cd /home/lucas/work/codex/codex-rs && run_checked mc_model_tools cargo test -p codex-core model_tools -- --quiet`
    - `cd /home/lucas/work/codex/codex-rs && run_checked mc_handler cargo test -p codex-core manage_context -- --quiet`
    - `cd /home/lucas/work/codex/codex-rs && run_checked mc_resume cargo test -p codex-core manage_context_resume -- --quiet`
    - `cd /home/lucas/work/codex/codex-rs && run_checked mc_slash_guard cargo test -p codex-tui task_availability_preserves_existing_commands -- --quiet`
  - Current status:
    - `just fmt` completed.
    - `cargo check -p codex-core` and `cargo check -p codex-protocol` completed.
    - test binaries remain blocked by missing Linux `libcap` development headers.

## Validation helper
```bash
run_checked() {
  name="$1"; shift
  "$@" 2>&1 | tee "/tmp/${name}.log"
  status=${PIPESTATUS[0]}
  test "$status" -eq 0
  rg -q "test result: ok\\. [1-9][0-9]* passed" "/tmp/${name}.log"
}
```

## Risks and fail-loud checks
- Snapshot mismatch must return explicit mismatch details and abort apply.
- Missing targets must surface in `missing_ids`/`skipped_missing_targets`.
- Protected/recent guards must reject unsafe exclude/delete unless override set.
- If contract differs from backup due architecture, docs must be updated in the same turn.
