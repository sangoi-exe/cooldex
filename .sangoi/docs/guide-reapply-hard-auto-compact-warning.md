# Reapply Guide — Post-Remote-Auto-Compact Warning (Actual Implementation)

## Problem this patch solves

After remote `auto-compact`, the agent could continue without an explicit operational checkpoint and lose practical continuity (plan state, unstaged changes, learning log awareness).

The chosen fix is to write a warning **into conversation history** (as a `user message`) immediately after every successful remote compaction.

## Why this design

- Do not use one-shot warning injection in prompt assembly:
  - one-shot text is ephemeral and easier to lose in later cycles;
  - it does not leave an explicit trace in history.
- Use `record_model_warning(...)`:
  - reuses existing warning persistence behavior;
  - keeps message format consistent (`Warning: ...`).
- Emit inside `run_auto_compact(...)`:
  - single convergence point for both auto-compact entry paths (normal loop + pre-sampling);
  - avoids duplicated logic in callers.

## Code locations

File: `codex-rs/core/src/codex.rs`

- Warning text constant:
  - `AUTO_COMPACT_RECON_WARNING_BODY` at `codex-rs/core/src/codex.rs:273`
- Warning emission:
  - `run_auto_compact(...)` at `codex-rs/core/src/codex.rs:4355`
  - remote branch:
    - runs `run_inline_remote_auto_compact_task(...)`;
    - then calls `sess.record_model_warning(AUTO_COMPACT_RECON_WARNING_BODY, turn_context)`;
    - reference: `codex-rs/core/src/codex.rs:4358`
- Call paths converging to `run_auto_compact(...)`:
  - token-limit auto-compact in main loop: `codex-rs/core/src/codex.rs:4239`
  - pre-sampling compact: `codex-rs/core/src/codex.rs:4299`
  - model-switch pre-sampling compact: `codex-rs/core/src/codex.rs:4322`, `codex-rs/core/src/codex.rs:4350`

## Functional contract (must not regress)

- Warning is emitted after **every** successful remote auto-compact.
- Warning is persisted as `user message` (via `record_model_warning`).
- Warning must not appear in:
  - manual `/compact`
  - local auto-compact
  - remote compact failure flow

## Reapply steps (code-level)

1. Add warning-body constant in `codex.rs` near session constants.
2. Enter `run_auto_compact(...)`.
3. In remote branch (`should_use_remote_compact_task(...) == true`):
   - keep `run_inline_remote_auto_compact_task(...).await?`;
   - immediately after it, call `record_model_warning(...)`.
4. Do not change local branch behavior.
5. Do not move warning emission into callers; keep it centralized in `run_auto_compact`.

## Test map and proof each test gives

File: `codex-rs/core/tests/suite/compact_remote.rs`

- `remote_compact_runs_automatically` (`:181`)
  - proves warning appears in follow-up after basic remote auto-compact.
- `remote_auto_compact_warning_is_emitted_after_each_compaction` (`:257`)
  - proves warning is emitted repeatedly across consecutive remote compactions.
- `remote_pre_sampling_auto_compact_emits_warning_after_model_switch` (`:351`)
  - proves coverage in pre-sampling/model-switch path.
- `auto_remote_compact_failure_stops_agent_loop` (`:740`)
  - proves warning is absent on remote compact failure and does not leak into subsequent manual `/compact`.

File: `codex-rs/core/tests/suite/compact.rs`

- `manual_compact_uses_custom_prompt` (`:399`)
  - proves manual `/compact` does not receive this warning.
- `local_auto_compact_does_not_emit_remote_warning` (`:1424`)
  - proves local auto-compact does not receive this warning.

## Validation checklist

```bash
cd codex-rs
just fmt
cargo test -p codex-core remote_compact_runs_automatically -- --nocapture
cargo test -p codex-core remote_auto_compact_warning_is_emitted_after_each_compaction -- --nocapture
cargo test -p codex-core remote_pre_sampling_auto_compact_emits_warning_after_model_switch -- --nocapture
cargo test -p codex-core auto_remote_compact_failure_stops_agent_loop -- --nocapture
cargo test -p codex-core manual_compact_uses_custom_prompt -- --nocapture
cargo test -p codex-core local_auto_compact_does_not_emit_remote_warning -- --nocapture
cargo build -p codex-core
```

## Regression signals

- warning missing after a successful remote auto-compact;
- warning appears in manual `/compact`;
- warning appears in local auto-compact path;
- warning appears even when `run_inline_remote_auto_compact_task(...)` fails.
