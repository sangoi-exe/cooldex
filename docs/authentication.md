# Authentication

Codex supports two authentication modes:

- **API key**: set `OPENAI_API_KEY` (or use `codex login --with-api-key`). This is the default for API-key workflows.
- **ChatGPT (OAuth)**: use `codex login` (browser) or `codex login --device-code` (headless). This uses ChatGPT credentials.

## Where credentials are stored

By default, Codex stores credentials in `$CODEX_HOME/auth.json` (defaults to `~/.codex/auth.json`).

You can choose the storage backend via `config.toml`:

```toml
cli_auth_credentials_store = "file"   # default
# cli_auth_credentials_store = "keyring"
# cli_auth_credentials_store = "auto"
```

When `keyring` or `auto` is used, the credentials are stored as a single serialized blob in the OS keyring (the file fallback is removed on successful keyring write).

## ChatGPT multi-account store

When using ChatGPT authentication, `auth.json` (or the keyring entry) stores a **versioned** auth store that can contain multiple ChatGPT accounts.

- Each ChatGPT login **upserts** the account and makes it **active**.
- `codex logout` deletes the stored credentials (removes the `auth.json` file and the keyring entry, if present).
- In the TUIs, use `/accounts` to switch the active account and add additional accounts. When multiple accounts are stored, `/logout` lets you choose between logging out all accounts or removing a single account (then exits).

## Auto-switch on usage limit (opt-in)

When enabled, if the backend returns `usage_limit_reached` (HTTP 429) for the active **ChatGPT** account, Codex will:

1) mark the active account as exhausted until its reset time (when available),
2) switch to another stored ChatGPT account that is not exhausted (and matches `forced_chatgpt_workspace_id` when set), and
3) retry the request **in the same turn**.

Codex emits a warning event when this happens.

Enable it with:

```toml
[auth]
auto_switch_on_usage_limit = true
```

Selection heuristic (best-effort, based on the last cached usage snapshot for each account):

1) Prefer the account closer to the weekly limit (when the secondary window represents “weekly”).
2) Then prefer lower credits balance when available.
3) Then prefer the account closer to the primary limit.
