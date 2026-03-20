# Codex NetSuite Bridge

This directory is the authoritative standalone bridge workspace for the NetSuite integration.

## Current state

The tracked source root is now:

- `src/index.js`
- `src/config.js`
- `src/logger.js`
- `src/app.js`
- `src/app-server/*`
- `src/bridge/*`

The existing untracked residue under this directory is **not** authoritative:

- `dist/`
- `node_modules/`
- `.env`
- `.npm-cache/`

Those files are old local build artifacts and reference material only.

## Runtime slice 2 behavior

The current runtime slice now does these things:

1. loads environment-driven config
2. starts a real `codex app-server` child process over `stdio://`
3. performs the required `initialize` then `initialized` handshake
4. persists the bridge-owned session registry, event journal, and pending-approval projection in SQLite
5. exposes live `session_create`, `message_send`, `session_list`, `session_open`, and `session_poll` routes
6. keeps `session_open` and `session_poll` strictly bridge-owned and durable across bridge restarts
7. auto-resolves unsupported interactive app-server server requests so turns do not hang forever
8. returns `message_send` as accepted-only and does not wait for turn completion inline
9. resolves an explicit per-session cwd/config path and never inherits the shell cwd for turns
10. resumes a persisted `threadId` on the first post-restart `message_send` before it calls `turn/start`

## Durability boundary

This slice persists the bridge-owned projection in SQLite at `BRIDGE_STATE_DB_PATH` (default `~/.codex/codex-netsuite-bridge/bridge-state.sqlite`).

The durable projection includes:

- `sessionId -> threadId`
- event journal
- pending approvals
- summary state

After a bridge restart:

- `GET /api/codex/v1/sessions` still reads the persisted bridge projection
- `GET /api/codex/v1/sessions/:sessionId` and `GET /api/codex/v1/sessions/:sessionId/events` still read the persisted bridge snapshot/journal
- sessions recovered from `running`, `waitingOnApproval`, or `waitingOnUserInput` are normalized to `interrupted`
- the next `message_send` resumes the stored `threadId` before it calls `turn/start`

The bridge still must not claim replay/backfill or mid-turn process resurrection semantics it does not provide. If Codex cannot resume the stored `threadId`, `message_send` fails loud instead of fabricating continuity.

## Published v1 route subset

### Live in Slice 2

- `POST /api/codex/v1/sessions`
- `POST /api/codex/v1/sessions/:sessionId/messages`
- `GET /api/codex/v1/sessions`
- `GET /api/codex/v1/sessions/:sessionId`
- `GET /api/codex/v1/sessions/:sessionId/events`

## Route semantics

- All `*At` / `occurredAt` fields are JavaScript epoch milliseconds.
- `SessionSummary` includes the resolved `cwd` used by that session.
- `SessionSummary` includes `operator` as either `null` or `{ userId, userEmail, displayName, key }`.
- `SessionSummary.operator.key` is derived as `firstName:userId` from `displayName` + `userId`, and is `null` when either part is missing.
- `SessionSummary` includes resolved `configPath` (`string | null`) for that session.
- `session_list` order is:
  - `updatedAt DESC`
  - then `createdAt DESC`
  - then `sessionId ASC`
- If `limit` is omitted on `session_list` or `session_poll`, the bridge returns all currently available persisted bridge items.
- `session_poll` returns events strictly after the supplied cursor.
- Invalid `session_poll` cursors fail loud with `BAD_REQUEST`.
- Empty `session_poll` results preserve the current cursor instead of resetting it to `null`.
- `session_open` and `session_poll` do not rebuild from Codex-native replay in this slice.
- `session_create` accepts optional `cwd`, optional `configPath`, and optional `operator`.
- When `session_create.cwd` is omitted or null, the bridge resolves the effective cwd to `/home/lucas/work/avmb-plus`.
- `session_create` resolves `configPath` in this order: explicit `session_create.configPath` -> `BRIDGE_DEFAULT_SESSION_CONFIG_PATH` -> `null`.
- `session_create.operator` must be an object or `null`; when present, `userId`, `userEmail`, and `displayName` must each be `string | null` when provided.
- Keep these semantics aligned with `src/bridge/runtime.js` (`resolveSessionCwd`, `resolveSessionConfigPath`, `normalizeOptionalSessionOperator`) and `src/bridge/store.js` (`cwd`, `configPath`, `operator` persistence fields).
- When `session_create.operator` is omitted, null, or normalizes to all-null identity fields, the bridge stores `session.operator` as `null`.
- When resolved `configPath` is `null`, the bridge does not send a `configPath` override and Codex uses its default config selection behavior.
- Invalid `session_create.cwd` values fail loud as `BAD_REQUEST`.
- Invalid `session_create.configPath` values fail loud as `BAD_REQUEST`.
- Invalid `session_create.operator` values fail loud as `BAD_REQUEST`.
- Every `turn/start` call sends the stored session cwd explicitly.
- `thread/start` receives resolved session `configPath` only when it is non-null, and the bridge fails loud if the response `configPath` mismatches the expected resolved value.
- The first `message_send` after a bridge restart resumes the stored `threadId` with the stored `cwd`, and it reuses the stored `configPath` unless that session was created without an explicit config override.
- `message_send` does not send `configPath` to `turn/start`; the config path is fixed at session creation time, and `message_send` fails loud if stored resolved session state is missing required fields.

## Unsupported interactive requests

- The bridge does not expose manual approval-response HTTP endpoints yet.
- To prevent turns from hanging forever, the bridge auto-resolves unsupported app-server server requests:
  - command approvals -> `decline`
  - file change approvals -> `decline`
  - request-user-input -> empty answers
  - MCP elicitation -> `decline`
  - unknown server requests -> JSON-RPC error back to app-server
- Those transitions are journaled, but they are not a substitute for full approval UX.

## Environment

Runtime floor: Node 22+

- `PORT` — HTTP port (default `8787`)
- `BRIDGE_BASE_PATH` — API prefix (default `/api/codex/v1`)
- `BRIDGE_BEARER_TOKEN` — required bearer token for protected bridge routes; when unset the bridge fails closed with `503 NOT_CONFIGURED` until a token is configured
- `BRIDGE_DEBUG_TRANSCRIPT` — when truthy, prints a terminal-only transcript of inbound user text, agent output, reasoning deltas, and key lifecycle/tool activity (default `false`)
- `BRIDGE_STATE_DB_PATH` — absolute SQLite file path for durable bridge state (default `~/.codex/codex-netsuite-bridge/bridge-state.sqlite`)
- `BRIDGE_DEFAULT_SESSION_CWD` — default cwd used when `session_create.cwd` is omitted (default `/home/lucas/work/avmb-plus`)
- `BRIDGE_DEFAULT_SESSION_CONFIG_PATH` — optional absolute path to the default `config.toml` used when `session_create.configPath` is omitted
- `CODEX_COMMAND` — app-server binary/command to spawn (default `codex`)
- `APP_SERVER_STARTUP_TIMEOUT_MS` — startup/handshake timeout for the first app-server initialize round-trip (default `15000`)
- `APP_SERVER_REQUEST_TIMEOUT_MS` — request timeout for app-server JSON-RPC calls (default `30000`)
- `APP_SERVER_SHUTDOWN_TIMEOUT_MS` — graceful shutdown timeout before force-kill (default `5000`)

The bridge validates `BRIDGE_DEFAULT_SESSION_CWD` and `BRIDGE_DEFAULT_SESSION_CONFIG_PATH` on startup. It starts `codex app-server` in the resolved default cwd. The bridge does not inherit the shell cwd as a hidden fallback for session turns, and it does not require `CODEX_HOME` changes to select a per-session `config.toml`.

## Terminal debug transcript

When `BRIDGE_DEBUG_TRANSCRIPT=true`, the bridge prints a human-readable operator transcript to `stderr` while it runs. This is terminal-only and does not change the bridge HTTP contract.

The transcript currently includes:

- inbound user message text accepted by `message_send`
- turn start/completion markers
- agent message deltas and completed message previews
- reasoning summary/text deltas when the app-server emits them
- item lifecycle markers
- tool output/progress deltas
- auto-resolved interactive request markers

Tool transcript lines are truncated intentionally so heavy tool output does not erase the beginning of the session transcript, and persisted tool result fields are clipped with explicit in-string truncation markers instead of silent clipping.

This output can contain sensitive prompt, reasoning, and response text. Keep it disabled outside active local debugging.

## Rules

- Do not treat old `dist/` code as canonical source.
- Do not implement fake session persistence or fake replay to make the UI “look alive”.
- Do not bypass the published bridge error envelope.
- Keep the bridge contract aligned with `.sangoi/to-avmb-plus/2026-03-08-bridge-contract-and-dependencies.md`.
- Do not claim replay/backfill or mid-turn process resurrection semantics that the current bridge journal does not provide.
