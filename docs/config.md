# Configuration

For basic configuration instructions, see [this documentation](https://developers.openai.com/codex/config-basic).

For advanced configuration instructions, see [this documentation](https://developers.openai.com/codex/config-advanced).

For a full configuration reference, see [this documentation](https://developers.openai.com/codex/config-reference).

## Connecting to MCP servers

Codex can connect to MCP servers configured in `~/.codex/config.toml`. See the configuration reference for the latest MCP server options:

- https://developers.openai.com/codex/config-reference

MCP tools default to serialized calls. To mark every tool exposed by one server
as eligible for parallel tool calls, set `supports_parallel_tool_calls` on that
server:

```toml
[mcp_servers.docs]
command = "docs-server"
supports_parallel_tool_calls = true
```

Only enable parallel calls for MCP servers whose tools are safe to run at the
same time. If tools read and write shared state, files, databases, or external
resources, review those read/write race conditions before enabling this setting.

## MCP tool approvals

Codex stores approval defaults and per-tool overrides for custom MCP servers
under `mcp_servers` in `~/.codex/config.toml`. Set
`default_tools_approval_mode` on the server to apply a default to every tool,
and use per-tool `approval_mode` entries for exceptions:

```toml
[mcp_servers.docs]
command = "docs-server"
default_tools_approval_mode = "approve"

[mcp_servers.docs.tools.search]
approval_mode = "prompt"
```

## Apps (Connectors)

Use `$` in the composer to insert a ChatGPT connector; the popover lists accessible
apps. The `/apps` command lists available and installed apps. Connected apps appear first
and are labeled as connected; others are marked as can be installed.

## Notify

Codex can run a notification hook when the agent finishes a turn. See the configuration reference for the latest notification settings:

- https://developers.openai.com/codex/config-reference

When Codex knows which client started the turn, the legacy notify JSON payload also includes a top-level `client` field. The TUI reports `codex-tui`, and the app server reports the `clientInfo.name` value from `initialize`.

## Post-Compact Recovery Warning

`post_compact_recovery_warning` customizes the user-visible warning emitted
after runtime-owned post-compact recovery context is prepared. This text is a
UI notice only; Codex injects the structured recovery packet as transient
developer context and does not persist the warning as conversation history.
When rollout-backed recovery is unavailable, Codex ignores this override and
emits the runtime-owned unavailable warning instead.

The removed `pos_compact_instructions` key is not accepted as an alias. A
config file that still uses it fails to load.

## JSON Schema

The generated JSON Schema for `config.toml` lives at `codex-rs/core/config.schema.json`.

<!-- Merge-safety anchor: resume-history docs must stay aligned with the
rollout-backed plain/app-server TUI replay contract and the default
since-last-compaction truncation boundary. -->
## Resume transcript rendering

`[tui].resume_history` controls how much persisted transcript the TUI replays
when you resume a stored session.

- `since-last-compaction` (default): replay only the suffix starting at the
  last surviving visible `Context compacted` marker.
- `full`: replay the full reconstructed persisted transcript.

This setting applies to resume bootstraps in both the plain TUI and the
app-server-backed TUI. In the plain TUI, once reconstructed turns are
available they define the resume boundary even if the surviving suffix is not
currently renderable; Codex falls back to the legacy `initial_messages` replay
path only when reconstructed turns could not be loaded.

<!-- Merge-safety anchor: final-turn handoff debug docs must stay aligned with
the core turn-finish raw `last_agent_message` dump contract, including the
CODEX_HOME/debug/<session_uuid>/turn-<turn_id> path and warning-only failures. -->
## Final turn handoff debug dump

Set `[tui].final_turn_handoff_debug = true` to dump the raw final assistant
handoff text for each completed turn to:

`$CODEX_HOME/debug/<session_uuid>/turn-<turn_id>-final-handoff-raw.txt`

Codex writes the exact `last_agent_message` string before the TUI renders it.
The file preserves Markdown and plain-text symbols verbatim and does not
include wrapped TUI output, ANSI escapes, or other post-format content. Codex
only writes the file when the flag is enabled and the final message is
non-empty. If directory creation or file writing fails, Codex emits a warning
for that turn and still completes the turn.

## SQLite State DB

Codex stores the SQLite-backed state DB under `sqlite_home` (config key) or the
`CODEX_SQLITE_HOME` environment variable. When unset, WorkspaceWrite sandbox
sessions default to a temp directory; other modes default to `CODEX_HOME`.

## Custom CA Certificates

Codex can trust a custom root CA bundle for outbound HTTPS and secure websocket
connections when enterprise proxies or gateways intercept TLS. This applies to
login flows and to Codex's other external connections, including Codex
components that build reqwest clients or secure websocket clients through the
shared `codex-client` CA-loading path and remote MCP connections that use it.

Set `CODEX_CA_CERTIFICATE` to the path of a PEM file containing one or more
certificate blocks to use a Codex-specific CA bundle. If
`CODEX_CA_CERTIFICATE` is unset, Codex falls back to `SSL_CERT_FILE`. If
neither variable is set, Codex uses the system root certificates.

`CODEX_CA_CERTIFICATE` takes precedence over `SSL_CERT_FILE`. Empty values are
treated as unset.

The PEM file may contain multiple certificates. Codex also tolerates OpenSSL
`TRUSTED CERTIFICATE` labels and ignores well-formed `X509 CRL` sections in the
same bundle. If the file is empty, unreadable, or malformed, the affected Codex
HTTP or secure websocket connection reports a user-facing error that points
back to these environment variables.

## Notices

Codex stores "do not show again" flags for some UI prompts under the `[notice]` table.

## Plan mode defaults

`plan_mode_reasoning_effort` lets you set a Plan-mode-specific default reasoning
effort override. When unset, Plan mode uses the built-in Plan preset default
(currently `medium`). When explicitly set (including `none`), it overrides the
Plan preset. The string value `none` means "no reasoning" (an explicit Plan
override), not "inherit the global default". There is currently no separate
config value for "follow the global default in Plan mode".

## Agents preemption control

Use `[agents].allow_running_subagent_preemption` to control whether collab
tools may preempt active sub-agents.

- Default: `true`
- When set to `false`, `send_input` with `interrupt = true` is rejected for
  non-final agent statuses.
- When set to `false`, `close_agent` is rejected for non-final agent statuses.
- Plain `send_input` without `interrupt` is unchanged, and terminal/final agents
  can still be closed.

<!-- Merge-safety anchor: this section is the durable operator note for the spawn-only child file-mutation contract and must stay aligned with the legacy spawn/resume runtime owners. -->
## Spawn-only child file-mutation denial

Profiles can define spawn-only child restrictions under
`[profiles.<name>.subagent]`.

```toml
[profiles.recon.subagent]
file_mutation = "deny"
```

When that profile is selected through `spawn_agent(profile = "...")`, the
spawned child keeps read access but cannot mutate files. The restriction stays
active after `resume_agent`, blocks `apply_patch`, rejects filesystem write
permission requests, and rejects unsandboxed execution or extra filesystem
write access for shell-style tools.

Use `file_mutation = "inherit"` or omit the field to keep the existing child
behavior. This setting is spawn-only; selecting the same profile for the lead
session does not make the lead read-only.

## Realtime start instructions

`experimental_realtime_start_instructions` lets you replace the built-in
developer message Codex inserts when realtime becomes active. It only affects
the realtime start message in prompt history and does not change websocket
backend prompt settings or the realtime end/inactive message.

Ctrl+C/Ctrl+D quitting uses a ~1 second double-press hint (`ctrl + c again to quit`).
