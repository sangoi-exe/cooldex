Wrong command: `rg -n "persists the bridge-owned|BRIDGE_STATE_DB_PATH|resumes the stored \`threadId\`|truncated intentionally|mid-turn process resurrection" mcp-standalone/README.md`
Cause and fix: Unescaped backticks inside a double-quoted shell string triggered command substitution and broke the probe. Use single quotes around the pattern (or escape the backticks) when the search text itself contains backticks.
Correct command: `rg -n 'persists the bridge-owned|BRIDGE_STATE_DB_PATH|resumes the stored `threadId`|truncated intentionally|mid-turn process resurrection' mcp-standalone/README.md`
