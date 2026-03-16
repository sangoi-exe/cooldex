# Config JSON Schema

<!-- Merge-safety anchor: generated `codex-rs/core/config.schema.json` must stay aligned with
ConfigToml fail-loud unknown-field rejection so removed hidden prompt_gc config seams do not
reappear as schema-published knobs. -->

We generate a JSON Schema for `~/.codex/config.toml` from the `ConfigToml` type
and commit it at `codex-rs/core/config.schema.json` for editor integration.

When you change any fields included in `ConfigToml` (or nested config types),
regenerate the schema:

```
just write-config-schema
```
