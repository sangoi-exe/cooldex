<!-- Merge-safety anchor: slash-command docs must stay aligned with the shipped `/permissions`, `/subagents`, and `/stop` canon; do not reintroduce `/approvals`, `/agent`, or `/clean` as active commands. -->
## Slash Commands

### What are slash commands?

Slash commands are special commands you can type that start with `/`.

---

### Built-in slash commands

Control Codex’s behavior during an interactive session with slash commands.

| Command         | Purpose                                                                    |
| --------------- | -------------------------------------------------------------------------- |
| `/model`        | choose what model and reasoning effort to use                              |
| `/permissions`  | choose what Codex can do without approval                                  |
| `/review`       | review my current changes and find issues                                  |
| `/new`          | start a new chat during a conversation                                     |
| `/resume`       | resume an old chat                                                         |
| `/init`         | create an AGENTS.md file with instructions for Codex                       |
| `/compact`      | summarize conversation to prevent hitting the context limit                |
| `/sanitize`     | sanitize session context                                                   |
| `/diff`         | show git diff (including untracked files)                                  |
| `/debug`        | show the latest raw API response item captured by the TUI                  |
| `/mention`      | mention a file                                                             |
| `/status`       | show current session configuration and token usage                         |
| `/subagents`    | switch the active subagent thread                                          |
| `/stop`         | stop all background terminals                                              |
| `/accounts`     | manage ChatGPT accounts (switch active / add account)                      |
| `/mcp`          | list configured MCP tools                                                  |
| `/experimental` | open the experimental menu to enable features from our beta program        |
| `/skills`       | browse and insert skills (experimental; see [docs/skills.md](./skills.md)) |
| `/logout`       | log out of Codex (or remove a single account when multiple are stored)     |
| `/quit`         | exit Codex                                                                 |
| `/exit`         | exit Codex                                                                 |
| `/feedback`     | send logs to maintainers                                                   |

---
