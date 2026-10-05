# Configuration

For basic configuration instructions, see [this documentation](https://developers.openai.com/codex/config-basic).

For advanced configuration instructions, see [this documentation](https://developers.openai.com/codex/config-advanced).

For a full configuration reference, see [this documentation](https://developers.openai.com/codex/config-reference).

<!-- Merge-safety anchor: Cooldex client preferences and capacity retry budgets extend the upstream configuration reference. -->
## Safety-buffering preferences

Codex remembers a deliberately selected **Dismiss and keep waiting** action or a confirmed **Retry with a faster model** in the active user configuration file, including a selected profile file. `Learn more` and Esc do not change the remembered choice.

```toml
[tui]
safety_buffering_last_choice = "dismiss-and-keep-waiting"
safety_buffering_auto_apply = false
```

`safety_buffering_last_choice` is optional; its other accepted value is `"retry-with-faster-model"`. With `safety_buffering_auto_apply = false` (the default), the remembered action is only preselected and retry still requires confirmation. With `true`, an available remembered action runs without either menu or retry confirmation: waiting keeps the current request and model; retry stops the attempt, forks a new thread, and resubmits with the server-selected faster model and Low reasoning effort. Already-made file changes remain. If there is no saved choice, or a saved retry is unavailable, the normal menu appears without replacing the preference.

## Model-capacity retries

Capacity failures use the existing stream retry policy: mapped server retry advice takes precedence, otherwise the existing backoff applies. The provider's `stream_max_retries` defaults to 5; this is a stream-stage retry budget, not a global five-request limit. HTTP request retries retain their independent budget, and remote V2 compaction retains its cap of 2 per transport.

## Lifecycle hooks

Admins can set top-level `allow_managed_hooks_only = true` in
`requirements.toml` to ignore user, project, and session hook configs while
still allowing managed hooks from requirements and managed config layers. This
setting is only supported in `requirements.toml`; putting it in `config.toml`
does not enable managed-hooks-only mode.
