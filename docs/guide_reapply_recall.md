# Recall Maintenance Guide

This document describes the current `recall` contract and the required maintenance checks when `recall` behavior changes.

## Current Contract

- `recall` accepts only an empty object payload: `{}`.
- Any unknown argument fails loud with `stop_reason = "invalid_contract"`.
- The rollout source is always the current session rollout recorder.
- The upper boundary is the latest `RolloutItem::Compacted`.
- For standard compactions, the matching `EventMsg::ContextCompacted(_)` is a legacy event emitted after the `ContextCompaction` item completes, so the current compaction's own legacy event is not part of the pre-`Compacted` scan.
- The lower boundary is the most recent earlier marker before that upper boundary where the marker is either:
  - `EventMsg::ContextCompacted(_)` from a previous compaction, or
  - `RolloutItem::Compacted` with `replacement_history: Some(...)`.
- If that lower boundary is `replacement_history_compacted`, recall hydrates the sanitized `replacement_history` stored on that marker as the base of the returned reasoning/assistant window, then appends newer matching rollout items after the marker.
- Otherwise, returned rollout scan starts immediately after the lower boundary marker; when no lower boundary exists, scan starts at index `0`.
- Returned items include only assistant messages and reasoning text.
- Returned items exclude user messages, tool calls, and tool outputs.
- `recall_kbytes_limit` applies a KiB byte cap from the tail of matching items.
- `recall_debug` controls output shape:
  - unset/`false` (default): compact payload (`mode = "recall_pre_compact_compact"`)
  - `true`: debug payload (`mode = "recall_pre_compact"`)
- In debug mode, replacement-history-derived items must report `source = "replacement_history"` and may use `rollout_index = null`; raw rollout-derived items use `source = "rollout"` with a concrete rollout index.
- Rollout parse errors do not hard-fail recall; debug mode reports degraded integrity with `integrity.rollout_parse_errors`.
- If no compaction marker exists, recall fails loud with `stop_reason = "no_compaction_marker"`.

## Required Touchpoints for Contract Changes

When changing recall behavior, update these in the same change:

- `codex-rs/core/src/tools/handlers/recall.rs`
- `docs/recall.md`
- `codex-rs/core/src/config/mod.rs` comments for recall-related settings
- `codex-rs/core/config.schema.json` descriptions if config docs changed

## Minimum Validation

Run only focused checks unless broader coverage is explicitly required:

```bash
cd codex-rs
just fmt
cargo test -p codex-core --lib tools::handlers::recall -- --test-threads=1
```

If `ConfigToml` comments/types affecting schema docs changed, also run:

```bash
cd codex-rs
just write-config-schema
```
