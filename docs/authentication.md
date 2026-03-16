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

<!-- Merge-safety anchor: ChatGPT auth admission policy must stay aligned across `/accounts`, external token login, and hidden auth consumers. -->
When using ChatGPT authentication, `auth.json` (or the keyring entry) stores a **versioned** auth store that can contain multiple ChatGPT accounts.

- Each ChatGPT login **upserts** the account and makes it **active**.
- Only **Plus** and **Pro** ChatGPT accounts remain in the saved multi-account store; unsupported plans fail the login instead of being stored and purged later.
- External ChatGPT token login (`chatgptAuthTokens`) follows the same Plus/Pro-only rule; missing or unsupported plans fail before ephemeral auth can become active.
- `codex logout` deletes the stored credentials (removes the `auth.json` file and the keyring entry, if present).
- In the TUIs, use `/accounts` to switch the active account and add additional accounts. When multiple accounts are stored, `/logout` lets you choose between logging out all accounts or removing a single account (then exits).
- Keep this aligned with `AuthManager::list_accounts()` and TUI account popups: `/accounts` renders only the summary fields exposed there.
- This multi-account management surface is currently a TUI feature. App-server account APIs expose the current account and current-account rate limits, not the full list/switch/remove workflow used by `/accounts`.

## Auto-switch on usage limit

<!-- Merge-safety anchor: usage-limit auto-switch behavior must stay aligned with the fresh `/api/codex/usage` refresh and auth-store eviction path in `codex-rs/core/src/codex.rs` and `codex-rs/core/src/auth.rs`. -->
When the backend returns `usage_limit_reached` (HTTP 429) for the active **ChatGPT** account, Codex will:

1) mark the active account as exhausted until its reset time (when available),
2) refresh stored account usage before fallback selection, remove any fallback candidates whose just-fetched usage snapshot reports plan `free` or `unknown`, then switch to another stored ChatGPT account that is not exhausted (and matches `forced_chatgpt_workspace_id` when set), and
3) retry the request **in the same turn**.

Codex emits a warning event when this happens.

Missing snapshot plan data does not trigger account removal.

Selection heuristic (best-effort, based on the last cached usage snapshot for each account):

1) Prefer the account closer to the weekly limit (when the secondary window represents “weekly”).
2) Then prefer lower credits balance when available.
3) Then prefer the account closer to the primary limit.
