# Guide Reapply — Recall Tool (Current Session Rollout)

> Canonical post-auto-compact instruction: call `recall` with empty args (`{}`).

## Objective
Reapply the current `recall` implementation in Codex CLI core with the final contract:
- `recall` accepts no parameters (`{}` only)
- unknown fields fail loud (`invalid_contract`)
- recall window is bounded by:
  - lower bound: immediately after latest pre-compaction `event_msg.user_message`
  - upper bound: latest `RolloutItem::Compacted`
- payload cap is controlled only by `recall_kbytes_limit` (KiB) in config

## Canonical Commit Chain
Use these commits in order when reapplying from scratch:
1. `140455920` — add recall tool
2. `7c967f2f4` — add recall hint to auto-compaction warning
3. `c1b707be9` — make recall mandatory in warning
4. `2d05e7923` — user-message boundary + KiB cap
5. `d68d5d8f3` — remove args (`max_items`, `max_chars_per_item`) and fail loud

## Files That Must Match Final Contract

### Core runtime
- `codex-rs/core/src/tools/handlers/recall.rs`
  - `RecallToolArgs` must be empty with `#[serde(deny_unknown_fields)]`
  - `handle_recall` must ignore args payload semantics and call:
    - `build_recall_payload(&rollout_items, turn.config.recall_kbytes_limit)`
  - no `max_items` path
  - no `max_chars_per_item` path
  - no text truncation helper based on char limit
  - `counts` must include `matching_pre_compact_items`, `returned_items`, `returned_bytes`, `bytes_limit`
  - `counts` must not include `max_items`

- `codex-rs/core/src/tools/spec.rs`
  - `create_recall_tool()` must expose an object schema with empty `properties`
  - `additional_properties` must be `false`
  - test `recall_tool_contract_is_read_only` must assert `properties.is_empty()`

- `codex-rs/core/src/codex.rs`
  - warning body must require recall without args:
  - `auto-compaction completed. MANDATORY before any other action: call recall. Then recon unstaged changes, codex_learning_log, and update_plan status.`

### Docs
- `docs/recall.md`
  - request contract must be `{}` only
  - must explicitly state legacy fields are rejected (`max_items`, `max_chars_per_item`)

- `docs/guide_reapply_recall.md`
  - this file (current guide)

## Reapply Path A (Recommended): Cherry-pick
If target branch does not already have recall:

```bash
git cherry-pick 140455920 7c967f2f4 c1b707be9 2d05e7923 d68d5d8f3
```

If target branch already has recall with legacy args:

```bash
git cherry-pick d68d5d8f3
```

## Reapply Path B: Manual Patch Checklist
Use this only if cherry-pick is not possible.

1. Ensure `recall` handler wiring exists in `codex-rs/core/src/tools/handlers/mod.rs` and `codex-rs/core/src/tools/spec.rs` (`register_handler("recall", ...)`).
2. In `codex-rs/core/src/tools/handlers/recall.rs`:
   - replace any argument struct with:
   ```rust
   #[derive(Debug, Deserialize)]
   #[serde(deny_unknown_fields)]
   struct RecallToolArgs {}
   ```
   - remove all logic that validates or applies `max_items`
   - remove all logic that validates or applies `max_chars_per_item`
   - remove char-based truncation helper
   - keep only KiB-cap trimming via config
3. In `codex-rs/core/src/tools/spec.rs`:
   - set recall tool `properties` to empty map
   - keep `additional_properties: false`
4. Update `docs/recall.md` to `{}`-only contract.
5. Ensure warning string in `codex-rs/core/src/codex.rs` is arg-less (`call recall.`).

## Required Verification

### Static checks
```bash
rg -n "struct RecallToolArgs \{\}" codex-rs/core/src/tools/handlers/recall.rs
rg -n "max_items|max_chars_per_item" codex-rs/core/src/tools/handlers/recall.rs codex-rs/core/src/tools/spec.rs docs/recall.md
rg -n "call recall\." codex-rs/core/src/codex.rs codex-rs/core/tests/suite/compact.rs codex-rs/core/tests/suite/compact_remote.rs
```

Expected:
- first command finds empty args struct
- second command must only match rejection/docs wording or regression-test names, not active accepted-argument contract
- third command shows warning text without tool args

### Formatting
```bash
cd codex-rs
just fmt
```

### Focused tests
```bash
cd codex-rs
cargo test -p codex-core --lib tools::handlers::recall -- --test-threads=1
cargo test -p codex-core --lib recall_tool_contract_is_read_only -- --test-threads=1
cargo test -p codex-core --test all compact_remote::remote_auto_compact_warning_is_emitted_after_each_compaction -- --test-threads=1
```

## Runtime Smoke (Mandatory)
Call the tool exactly with empty object:

```json
{}
```

Then validate:
- `mode = "recall_pre_compact"`
- response includes `boundary.start_index` and `boundary.latest_compacted_index`
- response includes `counts.returned_items`, `counts.returned_bytes`, `counts.bytes_limit`
- items contain only `reasoning` and/or `assistant_message`

Negative contract checks (must fail loud):

```json
{"max_items":8}
```

```json
{"max_chars_per_item":500}
```

Both must fail with `invalid_contract` parsing behavior due unknown fields.

## Git Notes
- If `master` is intentionally rewritten to this state, remote update usually requires force push:

```bash
git push --force-with-lease origin master
```

- If local policy blocks force push, execute the command outside the restricted environment.
