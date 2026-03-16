# Codex Local Scratchpad

Workspace-specific notes that should change future behavior.
Keep this high-signal. Avoid operational logs.

- While both `.sangoi/codex_scratchpad_local.md` and `./codex_scratchpad_local.md` exist, update them together; the workspace currently uses both entrypoints and durable memory must not split across them.
- In this workspace, `~/.codex/config.toml` duplicates some sub-agent rules across the playbook and role-specific sections; when one rule changes, update every mirrored instruction in the same edit to avoid stale references.
- `mcp-standalone` usage: session workspace is controlled by `session_create.cwd`; when omitted, the bridge falls back to `BRIDGE_DEFAULT_SESSION_CWD` (default `/home/lucas/work/avmb-plus`). Session config file resolves in order: `session_create.configPath` -> `BRIDGE_DEFAULT_SESSION_CONFIG_PATH` -> `null`; when resolved value is `null`, the bridge sends no config override and Codex applies its default config selection behavior. Session metadata also consumes optional `session_create.operator` and derives `session.operator.key` as `firstName:userId` when both `displayName` and `userId` are present. Do not use `CODEX_HOME` as a config-path workaround here because it changes auth/state.
<!-- Merge-safety anchor: local scratchpads are redirect/drift-trap entrypoints only; the canonical customization inventory lives in AGENTS.md so sync work has one durable source of truth. -->
- The canonical workspace-local customization inventory now lives in `AGENTS.md` under `## Workspace-local Customization Inventory (Source of Truth)`; update it there from the live diff against `upstream/main` and keep scratchpads as redirect/drift-trap notes only.
- Drift traps to remember during future syncs:
  - `recall_debug` defaults to compact mode when unset; if runtime, `docs/recall.md`, `docs/guide_reapply_recall.md`, `codex-rs/core/src/config/mod.rs`, and `codex-rs/core/config.schema.json` drift apart, fix them in one patch.
