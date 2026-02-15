# Plan (medium) — Lane B GOOD runtime flow audit

Goal: Rebuild end-to-end `/sanitize` runtime flow with file:line evidence for slash parsing, dispatch, `Op::Sanitize`, `spawn_sanitize_task`, `SanitizeTask` loop, and `run_sampling_request` + `manage_context` tool invocation path. Baseline GOOD commit: `ee8f56eb09f1e0783c63407b078155e5c2e3bf35` (current HEAD detached).

## Checklist
- [ ] Confirm baseline SHA and inventory target files/symbols
- [ ] Trace slash parsing and command dispatch to `Op::Sanitize`
- [ ] Trace `Op::Sanitize` to `spawn_sanitize_task` and `SanitizeTask` loop
- [ ] Trace `run_sampling_request` and `manage_context` tool invocation path inside sanitize flow
- [ ] Produce numbered flow with file:line for each hop and note mismatches vs current forensic report

## Evidence commands
- `git rev-parse HEAD`
- `rg -n "sanitize|Sanitize|/sanitize|spawn_sanitize_task|run_sampling_request|manage_context" codex-rs/core/src -S`
- `rg -n "slash|command|dispatch|Op::Sanitize|Op::" codex-rs/core/src -S`
- `nl -ba <file> | sed -n '<start>,<end>p'`

## Done criteria
- A numbered flow list with exact file:line for every hop requested.
- Explicit mismatch list vs existing forensic report, if any.
