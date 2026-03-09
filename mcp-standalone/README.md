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
4. maintains an in-memory session registry and event journal for the current process lifetime
5. exposes live `session_create`, `message_send`, `session_list`, `session_open`, and `session_poll` routes
6. keeps `session_open` and `session_poll` strictly bridge-owned and current-process only
7. auto-resolves unsupported interactive app-server server requests so turns do not hang forever
8. returns `message_send` as accepted-only and does not wait for turn completion inline
9. resolves an explicit per-session cwd and never inherits the shell cwd for turns

## Durability boundary

This slice is **not durable yet**.

The bridge currently keeps these projections in memory only:

- `sessionId -> threadId`
- event journal
- pending approvals
- summary state

That means a bridge restart loses current-process state. The bridge must not claim replay, persistence, or restart-safe continuity yet.

After a restart:

- `GET /api/codex/v1/sessions` can honestly return an empty set
- `GET /api/codex/v1/sessions/:sessionId` can honestly return `SESSION_NOT_FOUND`
- `GET /api/codex/v1/sessions/:sessionId/events` can honestly return `SESSION_NOT_FOUND`

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
- `session_list` order is:
  - `updatedAt DESC`
  - then `createdAt DESC`
  - then `sessionId ASC`
- If `limit` is omitted on `session_list` or `session_poll`, the bridge returns all currently available in-memory items.
- `session_poll` returns events strictly after the supplied cursor.
- Invalid `session_poll` cursors fail loud with `BAD_REQUEST`.
- Empty `session_poll` results preserve the current cursor instead of resetting it to `null`.
- `session_open` and `session_poll` do not rebuild from Codex-native replay in this slice.
- `session_create` accepts optional `cwd`.
- When `session_create.cwd` is omitted or null, the bridge resolves the effective cwd to `/home/lucas/work/avmb-plus`.
- Invalid `session_create.cwd` values fail loud as `BAD_REQUEST`.
- Every `turn/start` call sends the stored session cwd explicitly.

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
- `BRIDGE_BEARER_TOKEN` — required bearer token for protected bridge routes
- `BRIDGE_DEBUG_TRANSCRIPT` — when truthy, prints a terminal-only transcript of inbound user text, agent output, reasoning deltas, and key lifecycle/tool activity (default `false`)
- `BRIDGE_DEFAULT_SESSION_CWD` — default cwd used when `session_create.cwd` is omitted (default `/home/lucas/work/avmb-plus`)
- `CODEX_COMMAND` — app-server binary/command to spawn (default `codex`)
- `APP_SERVER_STARTUP_TIMEOUT_MS` — startup/handshake timeout for the first app-server initialize round-trip (default `15000`)
- `APP_SERVER_REQUEST_TIMEOUT_MS` — request timeout for app-server JSON-RPC calls (default `30000`)
- `APP_SERVER_SHUTDOWN_TIMEOUT_MS` — graceful shutdown timeout before force-kill (default `5000`)

The bridge validates `BRIDGE_DEFAULT_SESSION_CWD` on startup and starts `codex app-server` in that directory. The bridge does not inherit the shell cwd as a hidden fallback for session turns.

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

This output can contain sensitive prompt, reasoning, and response text. Keep it disabled outside active local debugging.

## Rules

- Do not treat old `dist/` code as canonical source.
- Do not implement fake session persistence or fake replay to make the UI “look alive”.
- Do not bypass the published bridge error envelope.
- Keep the bridge contract aligned with `.sangoi/to-avmb-plus/2026-03-08-bridge-contract-and-dependencies.md`.
- Do not claim restart continuity or replay/backfill semantics that the current in-memory journal does not provide.
