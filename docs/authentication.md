# Authentication

Codex supports two stable authentication modes:

- **API key**: set `OPENAI_API_KEY` (or use `codex login --with-api-key`). This is the default for API-key workflows.
- **ChatGPT managed (OAuth)**: use `codex login` (browser) or `codex login --device-code` (headless). This uses Codex-managed ChatGPT credentials.

Experimental/internal-only app-server surface:

- **ChatGPT external tokens (`chatgptAuthTokens`)**: the app-server auth surface can accept caller-supplied ChatGPT tokens and keep them in the external ephemeral store instead of the saved-account store. The stable generated app-server schema filters the experimental `account/login/start` request/response variant unless `experimentalApi` is enabled, but stable schemas can still mention `authMode: "chatgptAuthTokens"` and the server-initiated `account/chatgptAuthTokens/refresh` request when that auth path is active.

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
- Supported ChatGPT plans for saved-account auth are **Plus**, **Pro**, **Team**, **Business**, **Enterprise**, and **Edu**; unsupported plans fail the login, and stale unsupported accounts are purged on load or refresh if they are already present.
- External ChatGPT token login (`chatgptAuthTokens`) follows the same supported-plan policy; missing or unsupported plans fail before ephemeral auth can become active.
- Terminal refresh-token failures (`expired`, `reused`, or `invalidated`) evict the matching saved ChatGPT account instead of leaving it in the store with a sticky dead refresh state. If that account was active and another saved ChatGPT account is eligible, Codex switches to the fallback immediately; otherwise the current runtime becomes unauthenticated until you select or sign in to another account.
- `codex logout` deletes the stored credentials (removes the `auth.json` file and the keyring entry, if present).
- In the TUIs, use `/accounts` to switch the active account, add additional accounts, inspect whether an account is leased by the current or another live session, and force-release a foreign live lease when you need to recover from a crashed/stuck session. When multiple accounts are stored, `/logout` lets you choose between logging out all accounts or removing a single account (then exits). `/accounts` status refresh also prunes saved accounts that hit a terminal refresh-token failure while their usage data is being refreshed, and it does not advance cache freshness when account resolution fails transiently before usage fetch starts.
- Keep this aligned with `AuthManager::list_accounts()` and TUI account popups: `/accounts` renders only the summary fields exposed there.
- The app-server account APIs expose the same saved-account roster/switch/lease-recovery surface used by remote `/accounts`: `account/list`, `account/active/set`, and `account/lease/forceRelease`.

## Auto-switch on usage limit

<!-- Merge-safety anchor: usage-limit auto-switch behavior must stay aligned with the pre-refresh fallback-selection path in `codex-rs/core/src/session/turn.rs` and the auth-store eviction path in `codex-rs/login/src/auth/manager.rs`. -->
When the backend returns `usage_limit_reached` (HTTP 429) for the active **ChatGPT** account, Codex will:

1) mark the active account as exhausted until its reset time (when available),
2) refresh stored account usage before fallback selection, evict any saved account whose refresh token fails terminally during that refresh, remove any fallback candidates whose just-fetched usage snapshot reports plan `free`, then switch to another stored ChatGPT account that is not exhausted (and matches `forced_chatgpt_workspace_id` when set), and if no eligible ChatGPT fallback remains leave the current runtime unauthenticated instead of silently switching to another auth mode, and
3) retry the request **in the same turn**.

Codex emits a warning event when this happens.

Missing snapshot plan data does not trigger account removal.

Selection heuristic (best-effort, based on the last cached usage snapshot for each account):

1) Prefer the account with more primary-window headroom.
2) Then prefer the account with more weekly/secondary headroom (when the secondary window represents “weekly”).
3) Then use the remaining cached tie-breakers to keep selection deterministic.
