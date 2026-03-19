mod storage;

use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use reqwest::StatusCode;
use serde::Deserialize;
use serde::Serialize;
#[cfg(test)]
use serial_test::serial;
use std::env;
use std::fmt::Debug;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::RwLock;

use codex_app_server_protocol::AuthMode as ApiAuthMode;
use codex_otel::TelemetryAuthMode;
use codex_protocol::config_types::ForcedLoginMethod;

pub use crate::auth::storage::AccountUsageCache;
pub use crate::auth::storage::AuthCredentialsStoreMode;
pub use crate::auth::storage::AuthDotJson;
use crate::auth::storage::AuthStorageBackend;
pub use crate::auth::storage::AuthStore;
pub use crate::auth::storage::StoredAccount;
use crate::auth::storage::create_auth_storage;
use crate::config::Config;
use crate::error::RefreshTokenFailedError;
use crate::error::RefreshTokenFailedReason;
use crate::token_data::KnownPlan as InternalKnownPlan;
use crate::token_data::PlanType as InternalPlanType;
use crate::token_data::TokenData;
use crate::token_data::parse_chatgpt_jwt_claims;
use crate::util::try_parse_error_message;
use codex_client::CodexHttpClient;
use codex_protocol::account::PlanType as AccountPlanType;
use serde_json::Value;
use thiserror::Error;

/// Account type for the current user.
///
/// This is used internally to determine the base URL for generating responses,
/// and to gate ChatGPT-only behaviors like rate limits and available models (as
/// opposed to API key-based auth).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthMode {
    ApiKey,
    Chatgpt,
}

impl From<AuthMode> for TelemetryAuthMode {
    fn from(mode: AuthMode) -> Self {
        match mode {
            AuthMode::ApiKey => TelemetryAuthMode::ApiKey,
            AuthMode::Chatgpt => TelemetryAuthMode::Chatgpt,
        }
    }
}

/// Authentication mechanism used by the current user.
#[derive(Debug, Clone)]
pub enum CodexAuth {
    ApiKey(ApiKeyAuth),
    Chatgpt(ChatgptAuth),
    ChatgptAuthTokens(ChatgptAuthTokens),
}

#[derive(Debug, Clone)]
pub struct ApiKeyAuth {
    api_key: String,
}

#[derive(Debug, Clone)]
pub struct ChatgptAuth {
    store_account_id: String,
    state: ChatgptAuthState,
    storage: Arc<dyn AuthStorageBackend>,
}

#[derive(Debug, Clone)]
pub struct ChatgptAuthTokens {
    store_account_id: String,
    state: ChatgptAuthState,
}

#[derive(Debug, Clone)]
struct ChatgptAuthState {
    auth_dot_json: Arc<Mutex<Option<AuthDotJson>>>,
    client: CodexHttpClient,
}

impl PartialEq for CodexAuth {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::ApiKey(a), Self::ApiKey(b)) => a.api_key == b.api_key,
            (Self::Chatgpt(a), Self::Chatgpt(b)) => a.store_account_id == b.store_account_id,
            (Self::ChatgptAuthTokens(a), Self::ChatgptAuthTokens(b)) => {
                a.store_account_id == b.store_account_id
            }
            _ => false,
        }
    }
}

// TODO(pakrym): use token exp field to check for expiration instead
const TOKEN_REFRESH_INTERVAL: i64 = 8;
const USAGE_LIMIT_AUTO_SWITCH_COOLDOWN_SECONDS: i64 = 2;

const REFRESH_TOKEN_EXPIRED_MESSAGE: &str = "Your access token could not be refreshed because your refresh token has expired. Please log out and sign in again.";
const REFRESH_TOKEN_REUSED_MESSAGE: &str = "Your access token could not be refreshed because your refresh token was already used. Please log out and sign in again.";
const REFRESH_TOKEN_INVALIDATED_MESSAGE: &str = "Your access token could not be refreshed because your refresh token was revoked. Please log out and sign in again.";
const REFRESH_TOKEN_UNKNOWN_MESSAGE: &str =
    "Your access token could not be refreshed. Please log out and sign in again.";
const REFRESH_TOKEN_ACCOUNT_MISMATCH_MESSAGE: &str = "Your access token could not be refreshed because you have since logged out or signed in to another account. Please sign in again.";
pub const UNSUPPORTED_CHATGPT_PLAN_REMOVED_MESSAGE: &str = "Your ChatGPT account uses an unsupported plan and was removed from saved accounts. Please sign in again with a supported ChatGPT plan.";
pub const EXTERNAL_SUPPORTED_CHATGPT_PLAN_REQUIRED_MESSAGE: &str = "This ChatGPT plan is not supported for external auth. Please sign in again with a supported ChatGPT plan.";
pub const EXTERNAL_INVALID_ACCESS_TOKEN_MESSAGE: &str =
    "External ChatGPT auth requires a valid ChatGPT access token JWT.";
const REFRESH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
pub const REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR: &str = "CODEX_REFRESH_TOKEN_URL_OVERRIDE";

#[derive(Debug, Error)]
pub enum RefreshTokenError {
    #[error("{0}")]
    Permanent(#[from] RefreshTokenFailedError),
    #[error(transparent)]
    Transient(#[from] std::io::Error),
}

#[derive(Debug, Error)]
pub enum ExternalAuthLoginError {
    #[error("{EXTERNAL_SUPPORTED_CHATGPT_PLAN_REQUIRED_MESSAGE}")]
    UnsupportedPlan,
    #[error("{EXTERNAL_INVALID_ACCESS_TOKEN_MESSAGE}")]
    InvalidAccessToken,
    #[error("{0}")]
    MetadataMismatch(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalAuthTokens {
    pub access_token: String,
    pub chatgpt_account_id: String,
    pub chatgpt_plan_type: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExternalAuthRefreshReason {
    Unauthorized,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalAuthRefreshContext {
    pub reason: ExternalAuthRefreshReason,
    pub previous_account_id: Option<String>,
}

#[async_trait]
pub trait ExternalAuthRefresher: Send + Sync {
    async fn refresh(
        &self,
        context: ExternalAuthRefreshContext,
    ) -> std::io::Result<ExternalAuthTokens>;
}

impl RefreshTokenError {
    pub fn failed_reason(&self) -> Option<RefreshTokenFailedReason> {
        match self {
            Self::Permanent(error) => Some(error.reason),
            Self::Transient(_) => None,
        }
    }
}

impl From<RefreshTokenError> for std::io::Error {
    fn from(err: RefreshTokenError) -> Self {
        match err {
            RefreshTokenError::Permanent(failed) => std::io::Error::other(failed),
            RefreshTokenError::Transient(inner) => inner,
        }
    }
}

impl CodexAuth {
    fn from_auth_dot_json(
        codex_home: &Path,
        store_account_id: Option<String>,
        auth_dot_json: AuthDotJson,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
        client: CodexHttpClient,
    ) -> std::io::Result<Self> {
        let auth_mode = auth_dot_json.resolved_mode();
        if auth_mode == ApiAuthMode::ApiKey {
            let Some(api_key) = auth_dot_json.openai_api_key.as_deref() else {
                return Err(std::io::Error::other("API key auth is missing a key."));
            };
            return Ok(CodexAuth::from_api_key_with_client(api_key, client));
        }

        let Some(store_account_id) = store_account_id else {
            return Err(std::io::Error::other(
                "ChatGPT auth is missing an active account identifier.",
            ));
        };

        let storage_mode = auth_dot_json.storage_mode(auth_credentials_store_mode);
        let state = ChatgptAuthState {
            auth_dot_json: Arc::new(Mutex::new(Some(auth_dot_json))),
            client,
        };

        match auth_mode {
            ApiAuthMode::Chatgpt => {
                let storage = create_auth_storage(codex_home.to_path_buf(), storage_mode);
                Ok(Self::Chatgpt(ChatgptAuth {
                    store_account_id,
                    state,
                    storage,
                }))
            }
            ApiAuthMode::ChatgptAuthTokens => Ok(Self::ChatgptAuthTokens(ChatgptAuthTokens {
                store_account_id,
                state,
            })),
            ApiAuthMode::ApiKey => unreachable!("api key mode is handled above"),
        }
    }

    /// Loads the available auth information from auth storage.
    pub fn from_auth_storage(
        codex_home: &Path,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
    ) -> std::io::Result<Option<Self>> {
        load_auth(
            codex_home,
            /*enable_codex_api_key_env*/ false,
            auth_credentials_store_mode,
        )
    }

    pub fn internal_auth_mode(&self) -> AuthMode {
        match self {
            Self::ApiKey(_) => AuthMode::ApiKey,
            Self::Chatgpt(_) | Self::ChatgptAuthTokens(_) => AuthMode::Chatgpt,
        }
    }

    pub fn auth_mode(&self) -> AuthMode {
        self.internal_auth_mode()
    }

    pub fn api_auth_mode(&self) -> ApiAuthMode {
        match self {
            Self::ApiKey(_) => ApiAuthMode::ApiKey,
            Self::Chatgpt(_) => ApiAuthMode::Chatgpt,
            Self::ChatgptAuthTokens(_) => ApiAuthMode::ChatgptAuthTokens,
        }
    }

    pub fn is_api_key_auth(&self) -> bool {
        self.auth_mode() == AuthMode::ApiKey
    }

    pub fn is_chatgpt_auth(&self) -> bool {
        self.internal_auth_mode() == AuthMode::Chatgpt
    }

    pub fn is_external_chatgpt_tokens(&self) -> bool {
        matches!(self, Self::ChatgptAuthTokens(_))
    }

    /// Returns `None` is `is_internal_auth_mode() != AuthMode::ApiKey`.
    pub fn api_key(&self) -> Option<&str> {
        match self {
            Self::ApiKey(auth) => Some(auth.api_key.as_str()),
            Self::Chatgpt(_) | Self::ChatgptAuthTokens(_) => None,
        }
    }

    /// Returns `Err` if `is_chatgpt_auth()` is false.
    pub fn get_token_data(&self) -> Result<TokenData, std::io::Error> {
        let auth_dot_json: Option<AuthDotJson> = self.get_current_auth_json();
        match auth_dot_json {
            Some(AuthDotJson {
                tokens: Some(tokens),
                last_refresh: Some(_),
                ..
            }) => Ok(tokens),
            _ => Err(std::io::Error::other("Token data is not available.")),
        }
    }

    /// Returns the token string used for bearer authentication.
    pub fn get_token(&self) -> Result<String, std::io::Error> {
        match self {
            Self::ApiKey(auth) => Ok(auth.api_key.clone()),
            Self::Chatgpt(_) | Self::ChatgptAuthTokens(_) => {
                let access_token = self.get_token_data()?.access_token;
                Ok(access_token)
            }
        }
    }

    /// Returns `None` if `is_chatgpt_auth()` is false.
    pub fn get_account_id(&self) -> Option<String> {
        self.get_current_token_data().and_then(|t| t.account_id)
    }

    /// Returns `None` if `is_chatgpt_auth()` is false.
    pub fn get_account_email(&self) -> Option<String> {
        self.get_current_token_data().and_then(|t| t.id_token.email)
    }

    /// Returns `None` if `is_chatgpt_auth()` is false.
    pub fn get_chatgpt_user_id(&self) -> Option<String> {
        self.get_current_token_data()
            .and_then(|t| t.id_token.chatgpt_user_id)
    }

    /// Account-facing plan classification derived from the current token.
    /// Returns a high-level `AccountPlanType` (e.g., Free/Plus/Pro/Team/…)
    /// mapped from the ID token's internal plan value. Prefer this when you
    /// need to make UI or product decisions based on the user's subscription.
    /// When ChatGPT auth is active but the token omits the plan claim, report
    /// `Unknown` instead of treating the account as invalid.
    pub fn account_plan_type(&self) -> Option<AccountPlanType> {
        let map_known = |kp: &InternalKnownPlan| match kp {
            InternalKnownPlan::Free => AccountPlanType::Free,
            InternalKnownPlan::Go => AccountPlanType::Go,
            InternalKnownPlan::Plus => AccountPlanType::Plus,
            InternalKnownPlan::Pro => AccountPlanType::Pro,
            InternalKnownPlan::Team => AccountPlanType::Team,
            InternalKnownPlan::Business => AccountPlanType::Business,
            InternalKnownPlan::Enterprise => AccountPlanType::Enterprise,
            InternalKnownPlan::Edu => AccountPlanType::Edu,
        };

        self.get_current_token_data().map(|t| {
            t.id_token
                .chatgpt_plan_type
                .map(|pt| match pt {
                    InternalPlanType::Known(k) => map_known(&k),
                    InternalPlanType::Unknown(_) => AccountPlanType::Unknown,
                })
                .unwrap_or(AccountPlanType::Unknown)
        })
    }

    /// Returns `None` if `is_chatgpt_auth()` is false.
    fn get_current_auth_json(&self) -> Option<AuthDotJson> {
        let state = match self {
            Self::Chatgpt(auth) => &auth.state,
            Self::ChatgptAuthTokens(auth) => &auth.state,
            Self::ApiKey(_) => return None,
        };
        #[expect(clippy::unwrap_used)]
        state.auth_dot_json.lock().unwrap().clone()
    }

    /// Returns `None` if `is_chatgpt_auth()` is false.
    fn get_current_token_data(&self) -> Option<TokenData> {
        self.get_current_auth_json().and_then(|t| t.tokens)
    }

    /// Consider this private to integration tests.
    pub fn create_dummy_chatgpt_auth_for_testing() -> Self {
        let auth_dot_json = AuthDotJson {
            auth_mode: Some(ApiAuthMode::Chatgpt),
            openai_api_key: None,
            tokens: Some(TokenData {
                id_token: Default::default(),
                access_token: "Access Token".to_string(),
                refresh_token: "test".to_string(),
                account_id: Some("account_id".to_string()),
            }),
            last_refresh: Some(Utc::now()),
        };

        let client = crate::default_client::create_client();
        let state = ChatgptAuthState {
            auth_dot_json: Arc::new(Mutex::new(Some(auth_dot_json))),
            client,
        };
        let storage = create_auth_storage(PathBuf::new(), AuthCredentialsStoreMode::File);
        Self::Chatgpt(ChatgptAuth {
            store_account_id: "account_id".to_string(),
            state,
            storage,
        })
    }

    fn from_api_key_with_client(api_key: &str, _client: CodexHttpClient) -> Self {
        Self::ApiKey(ApiKeyAuth {
            api_key: api_key.to_owned(),
        })
    }

    pub fn from_api_key(api_key: &str) -> Self {
        Self::from_api_key_with_client(api_key, crate::default_client::create_client())
    }
}

impl ChatgptAuth {
    fn current_auth_json(&self) -> Option<AuthDotJson> {
        #[expect(clippy::unwrap_used)]
        self.state.auth_dot_json.lock().unwrap().clone()
    }

    fn current_token_data(&self) -> Option<TokenData> {
        self.current_auth_json().and_then(|auth| auth.tokens)
    }

    fn storage(&self) -> &Arc<dyn AuthStorageBackend> {
        &self.storage
    }

    fn client(&self) -> &CodexHttpClient {
        &self.state.client
    }
}

pub const OPENAI_API_KEY_ENV_VAR: &str = "OPENAI_API_KEY";
pub const CODEX_API_KEY_ENV_VAR: &str = "CODEX_API_KEY";

pub fn read_openai_api_key_from_env() -> Option<String> {
    env::var(OPENAI_API_KEY_ENV_VAR)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub fn read_codex_api_key_from_env() -> Option<String> {
    env::var(CODEX_API_KEY_ENV_VAR)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Delete the auth.json file inside `codex_home` if it exists. Returns `Ok(true)`
/// if a file was removed, `Ok(false)` if no auth file was present.
pub fn logout(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> std::io::Result<bool> {
    let _lock = storage::lock_auth_store(codex_home)?;
    let storage = create_auth_storage(codex_home.to_path_buf(), auth_credentials_store_mode);
    storage.delete()
}

/// Writes an `auth.json` that contains only the API key.
pub fn login_with_api_key(
    codex_home: &Path,
    api_key: &str,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> std::io::Result<()> {
    let store = AuthStore {
        openai_api_key: Some(api_key.to_string()),
        ..AuthStore::default()
    };
    save_auth(codex_home, &store, auth_credentials_store_mode)
}

/// Writes an in-memory auth payload for externally managed ChatGPT tokens.
pub fn login_with_chatgpt_auth_tokens(
    codex_home: &Path,
    access_token: &str,
    chatgpt_account_id: &str,
    chatgpt_plan_type: Option<&str>,
    required_workspace_id: Option<&str>,
) -> Result<(), ExternalAuthLoginError> {
    // Merge-safety anchor: external ChatGPT token auth must enforce the same supported-plan
    // admission policy as the saved-account store before ephemeral auth can become active.
    let auth_dot_json = AuthDotJson::from_external_access_token(
        access_token,
        chatgpt_account_id,
        chatgpt_plan_type,
        required_workspace_id,
    )
    .map_err(|error| {
        if error
            .get_ref()
            .and_then(|source| source.downcast_ref::<crate::token_data::IdTokenInfoError>())
            .is_some()
        {
            ExternalAuthLoginError::InvalidAccessToken
        } else if error.kind() == std::io::ErrorKind::InvalidData {
            ExternalAuthLoginError::MetadataMismatch(error.to_string())
        } else {
            ExternalAuthLoginError::Io(error)
        }
    })?;
    let mut store = AuthStore::from_legacy(auth_dot_json);
    if !enforce_supported_chatgpt_auth_accounts(&mut store).is_empty() {
        return Err(ExternalAuthLoginError::UnsupportedPlan);
    }
    save_auth(codex_home, &store, AuthCredentialsStoreMode::Ephemeral)?;
    Ok(())
}

/// Persist the provided auth payload using the specified backend.
pub fn save_auth(
    codex_home: &Path,
    auth: &AuthStore,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> std::io::Result<()> {
    let _lock = storage::lock_auth_store(codex_home)?;
    auth.validate()?;
    let storage = create_auth_storage(codex_home.to_path_buf(), auth_credentials_store_mode);
    storage.save(auth)
}

pub fn update_auth_store<T>(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    mutator: impl FnOnce(&mut AuthStore) -> std::io::Result<T>,
) -> std::io::Result<T> {
    let _lock = storage::lock_auth_store(codex_home)?;
    let storage = create_auth_storage(codex_home.to_path_buf(), auth_credentials_store_mode);
    let mut store = storage.load()?.unwrap_or_default();
    let out = mutator(&mut store)?;
    store.validate()?;
    storage.save(&store)?;
    Ok(out)
}

/// Load CLI auth data using the configured credential store backend.
/// Returns `None` when no credentials are stored. This function is
/// provided only for tests. Production code should not directly load
/// from the auth.json storage. It should use the AuthManager abstraction
/// instead.
pub fn load_auth_store(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> std::io::Result<Option<AuthStore>> {
    let storage = create_auth_storage(codex_home.to_path_buf(), auth_credentials_store_mode);
    storage.load()
}

pub fn enforce_login_restrictions(config: &Config) -> std::io::Result<()> {
    let Some(auth) = load_auth(
        &config.codex_home,
        /*enable_codex_api_key_env*/ true,
        config.cli_auth_credentials_store_mode,
    )?
    else {
        return Ok(());
    };

    if let Some(required_method) = config.forced_login_method {
        let method_violation = match (required_method, auth.internal_auth_mode()) {
            (ForcedLoginMethod::Api, AuthMode::ApiKey) => None,
            (ForcedLoginMethod::Chatgpt, AuthMode::Chatgpt) => None,
            (ForcedLoginMethod::Api, AuthMode::Chatgpt) => Some(
                "API key login is required, but ChatGPT is currently being used. Logging out."
                    .to_string(),
            ),
            (ForcedLoginMethod::Chatgpt, AuthMode::ApiKey) => Some(
                "ChatGPT login is required, but an API key is currently being used. Logging out."
                    .to_string(),
            ),
        };

        if let Some(message) = method_violation {
            return logout_with_message(
                &config.codex_home,
                message,
                config.cli_auth_credentials_store_mode,
            );
        }
    }

    if let Some(expected_account_id) = config.forced_chatgpt_workspace_id.as_deref() {
        if !auth.is_chatgpt_auth() {
            return Ok(());
        }

        let token_data = match auth.get_token_data() {
            Ok(data) => data,
            Err(err) => {
                return logout_with_message(
                    &config.codex_home,
                    format!(
                        "Failed to load ChatGPT credentials while enforcing workspace restrictions: {err}. Logging out."
                    ),
                    config.cli_auth_credentials_store_mode,
                );
            }
        };

        // workspace is the external identifier for account id.
        let chatgpt_account_id = token_data.id_token.chatgpt_account_id.as_deref();
        if chatgpt_account_id != Some(expected_account_id) {
            let message = match chatgpt_account_id {
                Some(actual) => format!(
                    "Login is restricted to workspace {expected_account_id}, but current credentials belong to {actual}. Logging out."
                ),
                None => format!(
                    "Login is restricted to workspace {expected_account_id}, but current credentials lack a workspace identifier. Logging out."
                ),
            };
            return logout_with_message(
                &config.codex_home,
                message,
                config.cli_auth_credentials_store_mode,
            );
        }
    }

    Ok(())
}

fn logout_with_message(
    codex_home: &Path,
    message: String,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> std::io::Result<()> {
    // External auth tokens live in the ephemeral store, but persistent auth may still exist
    // from earlier logins. Clear both so a forced logout truly removes all active auth.
    let removal_result = logout_all_stores(codex_home, auth_credentials_store_mode);
    let error_message = match removal_result {
        Ok(_) => message,
        Err(err) => format!("{message}. Failed to remove auth.json: {err}"),
    };
    Err(std::io::Error::other(error_message))
}

fn logout_all_stores(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> std::io::Result<bool> {
    if auth_credentials_store_mode == AuthCredentialsStoreMode::Ephemeral {
        return logout(codex_home, AuthCredentialsStoreMode::Ephemeral);
    }
    let removed_ephemeral = logout(codex_home, AuthCredentialsStoreMode::Ephemeral)?;
    let removed_managed = logout(codex_home, auth_credentials_store_mode)?;
    Ok(removed_ephemeral || removed_managed)
}

fn load_auth(
    codex_home: &Path,
    enable_codex_api_key_env: bool,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> std::io::Result<Option<CodexAuth>> {
    let build_auth =
        |store_account_id: Option<String>, auth_dot_json: AuthDotJson, storage_mode| {
            let client = crate::default_client::create_client();
            CodexAuth::from_auth_dot_json(
                codex_home,
                store_account_id,
                auth_dot_json,
                storage_mode,
                client,
            )
        };

    // API key via env var takes precedence over any other auth method.
    if enable_codex_api_key_env && let Some(api_key) = read_codex_api_key_from_env() {
        let client = crate::default_client::create_client();
        return Ok(Some(CodexAuth::from_api_key_with_client(
            api_key.as_str(),
            client,
        )));
    }

    // External ChatGPT auth tokens live in the in-memory (ephemeral) store. Always check this
    // first so external auth takes precedence over any persisted credentials.
    let ephemeral_storage = create_auth_storage(
        codex_home.to_path_buf(),
        AuthCredentialsStoreMode::Ephemeral,
    );
    let auth_dot_json_from_store =
        |store: AuthStore, auth_mode: ApiAuthMode| -> Option<(String, AuthDotJson)> {
            let active_account = store
                .active_account_id
                .as_deref()
                .and_then(|id| store.accounts.iter().find(|account| account.id == id))
                .or_else(|| store.accounts.first())?;

            let store_account_id = active_account.id.clone();
            let tokens = active_account.tokens.clone();

            Some((
                store_account_id,
                AuthDotJson {
                    auth_mode: match auth_mode {
                        ApiAuthMode::Chatgpt => None,
                        ApiAuthMode::ChatgptAuthTokens => Some(ApiAuthMode::ChatgptAuthTokens),
                        ApiAuthMode::ApiKey => Some(ApiAuthMode::ApiKey),
                    },
                    openai_api_key: None,
                    tokens: Some(tokens),
                    last_refresh: active_account.last_refresh,
                },
            ))
        };

    if let Some(mut store) = ephemeral_storage.load()? {
        let removed_account_ids = enforce_supported_chatgpt_auth_accounts(&mut store);
        if !removed_account_ids.is_empty() {
            tracing::info!(
                removed_account_ids = ?removed_account_ids,
                "removed accounts with unsupported ChatGPT plans while loading external auth store"
            );
            if let Err(error) = save_auth(codex_home, &store, AuthCredentialsStoreMode::Ephemeral) {
                tracing::warn!(
                    error = %error,
                    "failed to persist supported ChatGPT plan policy while loading external auth store"
                );
            }
        }
        if let Some((store_account_id, auth_dot_json)) =
            auth_dot_json_from_store(store.clone(), ApiAuthMode::ChatgptAuthTokens)
        {
            let auth = build_auth(
                Some(store_account_id),
                auth_dot_json,
                AuthCredentialsStoreMode::Ephemeral,
            )?;
            return Ok(Some(auth));
        }

        if let Some(api_key) = store.openai_api_key.as_deref() {
            let client = crate::default_client::create_client();
            return Ok(Some(CodexAuth::from_api_key_with_client(api_key, client)));
        }
    }

    // If the caller explicitly requested ephemeral auth, there is no persisted fallback.
    if auth_credentials_store_mode == AuthCredentialsStoreMode::Ephemeral {
        return Ok(None);
    }

    let storage = create_auth_storage(codex_home.to_path_buf(), auth_credentials_store_mode);
    let store = match storage.load()? {
        Some(store) => store,
        None => return Ok(None),
    };

    if let Some((store_account_id, auth_dot_json)) =
        auth_dot_json_from_store(store.clone(), ApiAuthMode::Chatgpt)
    {
        let auth = build_auth(
            Some(store_account_id),
            auth_dot_json,
            auth_credentials_store_mode,
        )?;
        return Ok(Some(auth));
    }

    if let Some(api_key) = store.openai_api_key.as_deref() {
        let client = crate::default_client::create_client();
        return Ok(Some(CodexAuth::from_api_key_with_client(api_key, client)));
    }

    Ok(None)
}

async fn update_tokens(
    codex_home: &Path,
    storage: &Arc<dyn AuthStorageBackend>,
    store_account_id: &str,
    id_token: Option<String>,
    access_token: Option<String>,
    refresh_token: Option<String>,
) -> std::io::Result<AuthStore> {
    let _lock = storage::lock_auth_store(codex_home)?;

    let mut store = storage
        .load()?
        .ok_or(std::io::Error::other("Token data is not available."))?;
    let account = store
        .accounts
        .iter_mut()
        .find(|account| account.id == store_account_id)
        .ok_or(std::io::Error::other("Token data is not available."))?;
    let tokens = &mut account.tokens;
    if let Some(id_token) = id_token {
        tokens.id_token = parse_chatgpt_jwt_claims(&id_token).map_err(std::io::Error::other)?;
    }
    if let Some(access_token) = access_token {
        tokens.access_token = access_token;
    }
    if let Some(refresh_token) = refresh_token {
        tokens.refresh_token = refresh_token;
    }
    account.last_refresh = Some(Utc::now());
    let removed_account_ids = enforce_supported_chatgpt_auth_accounts(&mut store);
    if !removed_account_ids.is_empty() {
        tracing::info!(
            removed_account_ids = ?removed_account_ids,
            "removed accounts with unsupported ChatGPT plans from auth store"
        );
    }
    store.validate()?;
    storage.save(&store)?;
    Ok(store)
}

// Requests refreshed ChatGPT OAuth tokens from the auth service using a refresh token.
// The caller is responsible for persisting any returned tokens.
async fn request_chatgpt_token_refresh(
    refresh_token: String,
    client: &CodexHttpClient,
) -> Result<RefreshResponse, RefreshTokenError> {
    let refresh_request = RefreshRequest {
        client_id: CLIENT_ID,
        grant_type: "refresh_token",
        refresh_token,
    };

    let endpoint = refresh_token_endpoint();

    // Use shared client factory to include standard headers
    let response = client
        .post(endpoint.as_str())
        .header("Content-Type", "application/json")
        .json(&refresh_request)
        .send()
        .await
        .map_err(|err| RefreshTokenError::Transient(std::io::Error::other(err)))?;

    let status = response.status();
    if status.is_success() {
        let refresh_response = response
            .json::<RefreshResponse>()
            .await
            .map_err(|err| RefreshTokenError::Transient(std::io::Error::other(err)))?;
        Ok(refresh_response)
    } else {
        let body = response.text().await.unwrap_or_default();
        tracing::error!("Failed to refresh token: {status}: {body}");
        if status == StatusCode::UNAUTHORIZED {
            let failed = classify_refresh_token_failure(&body);
            Err(RefreshTokenError::Permanent(failed))
        } else {
            let message = try_parse_error_message(&body);
            Err(RefreshTokenError::Transient(std::io::Error::other(
                format!("Failed to refresh token: {status}: {message}"),
            )))
        }
    }
}

fn classify_refresh_token_failure(body: &str) -> RefreshTokenFailedError {
    let code = extract_refresh_token_error_code(body);

    let normalized_code = code.as_deref().map(str::to_ascii_lowercase);
    let reason = match normalized_code.as_deref() {
        Some("refresh_token_expired") => RefreshTokenFailedReason::Expired,
        Some("refresh_token_reused") => RefreshTokenFailedReason::Exhausted,
        Some("refresh_token_invalidated") => RefreshTokenFailedReason::Revoked,
        _ => RefreshTokenFailedReason::Other,
    };

    if reason == RefreshTokenFailedReason::Other {
        tracing::warn!(
            backend_code = normalized_code.as_deref(),
            backend_body = body,
            "Encountered unknown 401 response while refreshing token"
        );
    }

    let message = match reason {
        RefreshTokenFailedReason::Expired => REFRESH_TOKEN_EXPIRED_MESSAGE.to_string(),
        RefreshTokenFailedReason::Exhausted => REFRESH_TOKEN_REUSED_MESSAGE.to_string(),
        RefreshTokenFailedReason::Revoked => REFRESH_TOKEN_INVALIDATED_MESSAGE.to_string(),
        RefreshTokenFailedReason::Other => REFRESH_TOKEN_UNKNOWN_MESSAGE.to_string(),
    };

    RefreshTokenFailedError::new(reason, message)
}

fn extract_refresh_token_error_code(body: &str) -> Option<String> {
    if body.trim().is_empty() {
        return None;
    }

    let Value::Object(map) = serde_json::from_str::<Value>(body).ok()? else {
        return None;
    };

    if let Some(error_value) = map.get("error") {
        match error_value {
            Value::Object(obj) => {
                if let Some(code) = obj.get("code").and_then(Value::as_str) {
                    return Some(code.to_string());
                }
            }
            Value::String(code) => {
                return Some(code.to_string());
            }
            _ => {}
        }
    }

    map.get("code").and_then(Value::as_str).map(str::to_string)
}

#[derive(Serialize)]
struct RefreshRequest {
    client_id: &'static str,
    grant_type: &'static str,
    refresh_token: String,
}

#[derive(Deserialize, Clone)]
struct RefreshResponse {
    id_token: Option<String>,
    access_token: Option<String>,
    refresh_token: Option<String>,
}

// Shared constant for token refresh (client id used for oauth token refresh flow)
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

fn refresh_token_endpoint() -> String {
    std::env::var(REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR)
        .unwrap_or_else(|_| REFRESH_TOKEN_URL.to_string())
}

fn external_auth_metadata_error(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message.into())
}

fn external_auth_plan_label(plan: &InternalPlanType) -> String {
    match plan {
        InternalPlanType::Known(plan) => format!("{plan:?}"),
        InternalPlanType::Unknown(raw) => raw.clone(),
    }
}

fn validate_external_access_token_claims(
    access_token: &str,
    provided_account_id: &str,
    provided_plan_type: Option<&str>,
    required_workspace_id: Option<&str>,
) -> std::io::Result<crate::token_data::IdTokenInfo> {
    let token_info = parse_chatgpt_jwt_claims(access_token).map_err(std::io::Error::other)?;
    let actual_account_id = token_info.chatgpt_account_id.as_deref().ok_or_else(|| {
        external_auth_metadata_error(
            "External auth access token is missing chatgpt_account_id claim.",
        )
    })?;
    if actual_account_id != provided_account_id {
        return Err(external_auth_metadata_error(format!(
            "External auth access token workspace claim {actual_account_id:?} does not match provided workspace {provided_account_id:?}."
        )));
    }
    if let Some(required_workspace_id) = required_workspace_id
        && actual_account_id != required_workspace_id
    {
        return Err(external_auth_metadata_error(format!(
            "External auth access token workspace claim {actual_account_id:?} does not match required workspace {required_workspace_id:?}."
        )));
    }
    if let Some(provided_plan_type) = provided_plan_type {
        let actual_plan_type = token_info.chatgpt_plan_type.as_ref().ok_or_else(|| {
            external_auth_metadata_error(
                "External auth access token is missing chatgpt_plan_type claim, so provided plan metadata cannot be verified.",
            )
        })?;
        let provided_plan_type = InternalPlanType::from_raw_value(provided_plan_type);
        if actual_plan_type != &provided_plan_type {
            return Err(external_auth_metadata_error(format!(
                "External auth access token plan claim {:?} does not match provided plan {:?}.",
                external_auth_plan_label(actual_plan_type),
                external_auth_plan_label(&provided_plan_type),
            )));
        }
    }
    Ok(token_info)
}

use std::cmp::Reverse;
use std::collections::HashMap;
use std::collections::HashSet;

impl AuthDotJson {
    fn from_external_tokens(
        external: &ExternalAuthTokens,
        required_workspace_id: Option<&str>,
    ) -> std::io::Result<Self> {
        let token_info = validate_external_access_token_claims(
            &external.access_token,
            external.chatgpt_account_id.as_str(),
            external.chatgpt_plan_type.as_deref(),
            required_workspace_id,
        )?;
        let tokens = TokenData {
            account_id: token_info.chatgpt_account_id.clone(),
            id_token: token_info,
            access_token: external.access_token.clone(),
            refresh_token: String::new(),
        };

        Ok(Self {
            auth_mode: Some(ApiAuthMode::ChatgptAuthTokens),
            openai_api_key: None,
            tokens: Some(tokens),
            last_refresh: Some(Utc::now()),
        })
    }

    fn from_external_access_token(
        access_token: &str,
        chatgpt_account_id: &str,
        chatgpt_plan_type: Option<&str>,
        required_workspace_id: Option<&str>,
    ) -> std::io::Result<Self> {
        let external = ExternalAuthTokens {
            access_token: access_token.to_string(),
            chatgpt_account_id: chatgpt_account_id.to_string(),
            chatgpt_plan_type: chatgpt_plan_type.map(str::to_string),
        };
        Self::from_external_tokens(&external, required_workspace_id)
    }

    fn resolved_mode(&self) -> ApiAuthMode {
        if let Some(mode) = self.auth_mode {
            return mode;
        }
        if self.openai_api_key.is_some() {
            return ApiAuthMode::ApiKey;
        }
        ApiAuthMode::Chatgpt
    }

    fn storage_mode(
        &self,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
    ) -> AuthCredentialsStoreMode {
        if self.resolved_mode() == ApiAuthMode::ChatgptAuthTokens {
            AuthCredentialsStoreMode::Ephemeral
        } else {
            auth_credentials_store_mode
        }
    }
}

/// Internal cached auth state.
#[derive(Clone)]
struct CachedAuth {
    store: AuthStore,
    auth: Option<CodexAuth>,
    /// Callback used to refresh external auth by asking the parent app for new tokens.
    external_refresher: Option<Arc<dyn ExternalAuthRefresher>>,
}

impl Debug for CachedAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachedAuth")
            .field(
                "auth_mode",
                &self.auth.as_ref().map(CodexAuth::api_auth_mode),
            )
            .field(
                "external_refresher",
                &self.external_refresher.as_ref().map(|_| "present"),
            )
            .finish()
    }
}

enum UnauthorizedRecoveryStep {
    Reload,
    RefreshToken,
    ExternalRefresh,
    Done,
}

enum ReloadOutcome {
    /// Reload was performed and the cached auth changed
    ReloadedChanged,
    /// Reload was performed and the cached auth remained the same
    ReloadedNoChange,
    /// Reload was skipped (missing or mismatched account id)
    Skipped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UnauthorizedRecoveryMode {
    Managed,
    External,
}

// UnauthorizedRecovery is a state machine that handles an attempt to refresh the authentication when requests
// to API fail with 401 status code.
// The client calls next() every time it encounters a 401 error, one time per retry.
// For API key based authentication, we don't do anything and let the error bubble to the user.
//
// For ChatGPT based authentication, we:
// 1. Attempt to reload the auth data from disk. We only reload if the account id matches the one the current process is running as.
// 2. Attempt to refresh the token using OAuth token refresh flow.
// If after both steps the server still responds with 401 we let the error bubble to the user.
//
// For external ChatGPT auth tokens (chatgptAuthTokens), UnauthorizedRecovery does not touch disk or refresh
// tokens locally. Instead it calls the ExternalAuthRefresher (account/chatgptAuthTokens/refresh) to ask the
// parent app for new tokens, stores them in the ephemeral auth store, and retries once.
pub struct UnauthorizedRecovery {
    manager: Arc<AuthManager>,
    step: UnauthorizedRecoveryStep,
    expected_account_id: Option<String>,
    mode: UnauthorizedRecoveryMode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnauthorizedRecoveryStepResult {
    auth_state_changed: Option<bool>,
}

impl UnauthorizedRecoveryStepResult {
    pub fn auth_state_changed(&self) -> Option<bool> {
        self.auth_state_changed
    }
}

impl UnauthorizedRecovery {
    fn new(manager: Arc<AuthManager>) -> Self {
        let cached_auth = manager.auth_cached();
        let expected_account_id = cached_auth.as_ref().and_then(CodexAuth::get_account_id);
        let mode = if cached_auth
            .as_ref()
            .is_some_and(CodexAuth::is_external_chatgpt_tokens)
        {
            UnauthorizedRecoveryMode::External
        } else {
            UnauthorizedRecoveryMode::Managed
        };
        let step = match mode {
            UnauthorizedRecoveryMode::Managed => UnauthorizedRecoveryStep::Reload,
            UnauthorizedRecoveryMode::External => UnauthorizedRecoveryStep::ExternalRefresh,
        };
        Self {
            manager,
            step,
            expected_account_id,
            mode,
        }
    }

    pub fn has_next(&self) -> bool {
        if !self
            .manager
            .auth_cached()
            .as_ref()
            .is_some_and(CodexAuth::is_chatgpt_auth)
        {
            return false;
        }

        if self.mode == UnauthorizedRecoveryMode::External
            && !self.manager.has_external_auth_refresher()
        {
            return false;
        }

        !matches!(self.step, UnauthorizedRecoveryStep::Done)
    }

    pub fn unavailable_reason(&self) -> &'static str {
        if !self
            .manager
            .auth_cached()
            .as_ref()
            .is_some_and(CodexAuth::is_chatgpt_auth)
        {
            return "not_chatgpt_auth";
        }

        if self.mode == UnauthorizedRecoveryMode::External
            && !self.manager.has_external_auth_refresher()
        {
            return "no_external_refresher";
        }

        if matches!(self.step, UnauthorizedRecoveryStep::Done) {
            return "recovery_exhausted";
        }

        "ready"
    }

    pub fn mode_name(&self) -> &'static str {
        match self.mode {
            UnauthorizedRecoveryMode::Managed => "managed",
            UnauthorizedRecoveryMode::External => "external",
        }
    }

    pub fn step_name(&self) -> &'static str {
        match self.step {
            UnauthorizedRecoveryStep::Reload => "reload",
            UnauthorizedRecoveryStep::RefreshToken => "refresh_token",
            UnauthorizedRecoveryStep::ExternalRefresh => "external_refresh",
            UnauthorizedRecoveryStep::Done => "done",
        }
    }

    pub async fn next(&mut self) -> Result<UnauthorizedRecoveryStepResult, RefreshTokenError> {
        if !self.has_next() {
            return Err(RefreshTokenError::Permanent(RefreshTokenFailedError::new(
                RefreshTokenFailedReason::Other,
                "No more recovery steps available.",
            )));
        }

        match self.step {
            UnauthorizedRecoveryStep::Reload => {
                match self
                    .manager
                    .reload_if_account_id_matches(self.expected_account_id.as_deref())
                {
                    ReloadOutcome::ReloadedChanged => {
                        self.step = UnauthorizedRecoveryStep::RefreshToken;
                        return Ok(UnauthorizedRecoveryStepResult {
                            auth_state_changed: Some(true),
                        });
                    }
                    ReloadOutcome::ReloadedNoChange => {
                        self.step = UnauthorizedRecoveryStep::RefreshToken;
                        return Ok(UnauthorizedRecoveryStepResult {
                            auth_state_changed: Some(false),
                        });
                    }
                    ReloadOutcome::Skipped => {
                        self.step = UnauthorizedRecoveryStep::Done;
                        return Err(RefreshTokenError::Permanent(RefreshTokenFailedError::new(
                            RefreshTokenFailedReason::Other,
                            REFRESH_TOKEN_ACCOUNT_MISMATCH_MESSAGE.to_string(),
                        )));
                    }
                }
            }
            UnauthorizedRecoveryStep::RefreshToken => {
                self.manager.refresh_token_from_authority().await?;
                self.step = UnauthorizedRecoveryStep::Done;
                return Ok(UnauthorizedRecoveryStepResult {
                    auth_state_changed: Some(true),
                });
            }
            UnauthorizedRecoveryStep::ExternalRefresh => {
                self.manager
                    .refresh_external_auth(ExternalAuthRefreshReason::Unauthorized)
                    .await?;
                self.step = UnauthorizedRecoveryStep::Done;
                return Ok(UnauthorizedRecoveryStepResult {
                    auth_state_changed: Some(true),
                });
            }
            UnauthorizedRecoveryStep::Done => {}
        }
        Ok(UnauthorizedRecoveryStepResult {
            auth_state_changed: None,
        })
    }
}

/// Central manager providing a single source of truth for auth.json derived
/// authentication data. It loads once (or on preference change) and then
/// hands out cloned `CodexAuth` values so the rest of the program has a
/// consistent snapshot.
///
/// External modifications to `auth.json` will NOT be observed until
/// `reload()` is called explicitly. This matches the design goal of avoiding
/// different parts of the program seeing inconsistent auth data mid‑run.
#[derive(Debug)]
pub struct AuthManager {
    codex_home: PathBuf,
    storage: Arc<dyn AuthStorageBackend>,
    inner: RwLock<CachedAuth>,
    enable_codex_api_key_env: bool,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    forced_chatgpt_workspace_id: RwLock<Option<String>>,
    usage_limit_auto_switch_cooldown_until: Mutex<Option<DateTime<Utc>>>,
    _test_home_guard: Option<tempfile::TempDir>,
}

impl AuthManager {
    /// Create a new manager loading the initial auth using the provided
    /// preferred auth method. Errors loading auth are swallowed; `auth()` will
    /// simply return `None` in that case so callers can treat it as an
    /// unauthenticated state.
    pub fn new(
        codex_home: PathBuf,
        enable_codex_api_key_env: bool,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
    ) -> Self {
        let storage = create_auth_storage(codex_home.clone(), auth_credentials_store_mode);
        let mut store = storage.load().ok().flatten().unwrap_or_default();
        let removed_account_ids = enforce_supported_chatgpt_auth_accounts(&mut store);
        if !removed_account_ids.is_empty() {
            tracing::info!(
                removed_account_ids = ?removed_account_ids,
                "removed accounts with unsupported ChatGPT plans during auth manager initialization"
            );
            if let Err(error) = save_auth(&codex_home, &store, auth_credentials_store_mode) {
                tracing::warn!(
                    error = %error,
                    "failed to persist supported ChatGPT plan policy during initialization"
                );
            }
        }
        let auth = load_auth(
            &codex_home,
            enable_codex_api_key_env,
            auth_credentials_store_mode,
        )
        .ok()
        .flatten();
        Self {
            codex_home,
            storage,
            inner: RwLock::new(CachedAuth {
                store,
                auth,
                external_refresher: None,
            }),
            enable_codex_api_key_env,
            auth_credentials_store_mode,
            forced_chatgpt_workspace_id: RwLock::new(None),
            usage_limit_auto_switch_cooldown_until: Mutex::new(None),
            _test_home_guard: None,
        }
    }

    /// Create an AuthManager with a specific CodexAuth, for testing only.
    pub(crate) fn from_auth_for_testing(auth: CodexAuth) -> Arc<Self> {
        let temp_dir = tempfile::tempdir().unwrap_or_else(|err| panic!("temp codex home: {err}"));
        let codex_home = temp_dir.path().to_path_buf();
        let storage = create_auth_storage(codex_home.clone(), AuthCredentialsStoreMode::File);
        let store = store_from_auth_for_testing(&auth);
        let cached = CachedAuth {
            store,
            auth: Some(auth),
            external_refresher: None,
        };

        Arc::new(Self {
            codex_home,
            storage,
            inner: RwLock::new(cached),
            enable_codex_api_key_env: false,
            auth_credentials_store_mode: AuthCredentialsStoreMode::File,
            forced_chatgpt_workspace_id: RwLock::new(None),
            usage_limit_auto_switch_cooldown_until: Mutex::new(None),
            _test_home_guard: Some(temp_dir),
        })
    }

    /// Create an AuthManager with a specific CodexAuth and codex home, for testing only.
    pub(crate) fn from_auth_for_testing_with_home(
        auth: CodexAuth,
        codex_home: PathBuf,
    ) -> Arc<Self> {
        let storage = create_auth_storage(codex_home.clone(), AuthCredentialsStoreMode::File);
        let store = store_from_auth_for_testing(&auth);
        let cached = CachedAuth {
            store,
            auth: Some(auth),
            external_refresher: None,
        };
        Arc::new(Self {
            codex_home,
            storage,
            inner: RwLock::new(cached),
            enable_codex_api_key_env: false,
            auth_credentials_store_mode: AuthCredentialsStoreMode::File,
            forced_chatgpt_workspace_id: RwLock::new(None),
            usage_limit_auto_switch_cooldown_until: Mutex::new(None),
            _test_home_guard: None,
        })
    }

    /// Current cached auth (clone) without attempting a refresh.
    pub fn auth_cached(&self) -> Option<CodexAuth> {
        self.inner.read().ok().and_then(|c| c.auth.clone())
    }

    pub fn chatgpt_auth_for_store_account_id(&self, store_account_id: &str) -> Option<CodexAuth> {
        if self.get_auth_mode() != Some(ApiAuthMode::Chatgpt) {
            return None;
        }

        let store = self.inner.read().ok()?.store.clone();
        Self::derive_chatgpt_auth_from_store_account(
            &store,
            store_account_id,
            Arc::clone(&self.storage),
        )
    }

    pub fn list_accounts(&self) -> Vec<AccountSummary> {
        let Ok(guard) = self.inner.read() else {
            return Vec::new();
        };

        guard
            .store
            .accounts
            .iter()
            .map(|account| {
                AccountSummary::from_stored(account, guard.store.active_account_id.as_deref())
            })
            .collect()
    }

    pub fn set_active_account(&self, id: &str) -> std::io::Result<()> {
        self.update_store(|store| {
            if !store.accounts.iter().any(|account| account.id == id) {
                return Err(std::io::Error::other(format!("account '{id}' not found")));
            }
            store.active_account_id = Some(id.to_string());
            if let Some(account) = store.accounts.iter_mut().find(|account| account.id == id) {
                let usage = account.usage.get_or_insert_with(AccountUsageCache::default);
                usage.last_seen_at = Some(Utc::now());
            }
            Ok(())
        })
    }

    pub fn upsert_account(
        &self,
        tokens: TokenData,
        label: Option<String>,
        make_active: bool,
    ) -> std::io::Result<String> {
        let account_id = tokens
            .preferred_store_account_id()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        self.update_store(|store| {
            let now = Utc::now();
            let existing = store
                .accounts
                .iter_mut()
                .find(|account| account.id == account_id);
            match existing {
                Some(account) => {
                    account.tokens = tokens;
                    account.last_refresh = Some(now);
                    if label.is_some() {
                        account.label = label;
                    }
                }
                None => {
                    store.accounts.push(StoredAccount {
                        id: account_id.clone(),
                        label,
                        tokens,
                        last_refresh: Some(now),
                        usage: None,
                    });
                }
            }
            if make_active || store.active_account_id.is_none() {
                store.active_account_id = Some(account_id.clone());
            }
            Ok(account_id.clone())
        })
    }

    pub fn remove_account(&self, id: &str) -> std::io::Result<bool> {
        self.update_store(|store| {
            let prev_len = store.accounts.len();
            store.accounts.retain(|account| account.id != id);
            let removed = store.accounts.len() != prev_len;
            if !removed {
                return Ok(false);
            }
            if store.active_account_id.as_deref() == Some(id) {
                store.active_account_id = store.accounts.first().map(|a| a.id.clone());
            }
            Ok(true)
        })
    }

    pub fn update_usage_for_active(
        &self,
        snapshot: crate::protocol::RateLimitSnapshot,
    ) -> std::io::Result<()> {
        if self.get_auth_mode() != Some(ApiAuthMode::Chatgpt) {
            return Ok(());
        }
        self.update_store(|store| {
            let Some(active_id) = store.active_account_id.clone() else {
                return Ok(());
            };
            let Some(account) = store
                .accounts
                .iter_mut()
                .find(|account| account.id == active_id)
            else {
                return Err(std::io::Error::other("active account id not found"));
            };

            let now = Utc::now();
            let exhausted_until = exhausted_until_from_snapshot(&snapshot, now);
            let usage = account.usage.get_or_insert_with(AccountUsageCache::default);
            usage.last_rate_limits = Some(snapshot);
            usage.exhausted_until = exhausted_until;
            usage.last_seen_at = Some(now);
            Ok(())
        })
    }

    pub fn update_rate_limits_for_account(
        &self,
        store_account_id: &str,
        snapshot: crate::protocol::RateLimitSnapshot,
    ) -> std::io::Result<()> {
        if self.get_auth_mode() != Some(ApiAuthMode::Chatgpt) {
            return Ok(());
        }

        self.update_store(|store| {
            let Some(account) = store
                .accounts
                .iter_mut()
                .find(|account| account.id == store_account_id)
            else {
                return Err(std::io::Error::other(format!(
                    "account '{store_account_id}' not found"
                )));
            };

            let now = Utc::now();
            let exhausted_until = exhausted_until_from_snapshot(&snapshot, now);
            let usage = account.usage.get_or_insert_with(AccountUsageCache::default);
            usage.last_rate_limits = Some(snapshot);
            usage.exhausted_until = exhausted_until;
            usage.last_seen_at = Some(now);
            Ok(())
        })
    }

    pub fn update_rate_limits_for_accounts(
        &self,
        updates: impl IntoIterator<Item = (String, crate::protocol::RateLimitSnapshot)>,
    ) -> std::io::Result<usize> {
        if self.get_auth_mode() != Some(ApiAuthMode::Chatgpt) {
            return Ok(0);
        }

        let mut updates = updates.into_iter().collect::<HashMap<_, _>>();
        if updates.is_empty() {
            return Ok(0);
        }

        self.update_store(|store| {
            let now = Utc::now();
            let mut updated = 0usize;
            for account in &mut store.accounts {
                if let Some(snapshot) = updates.remove(&account.id) {
                    let exhausted_until = exhausted_until_from_snapshot(&snapshot, now);
                    let usage = account.usage.get_or_insert_with(AccountUsageCache::default);
                    usage.last_rate_limits = Some(snapshot);
                    usage.exhausted_until = exhausted_until;
                    usage.last_seen_at = Some(now);
                    updated = updated.saturating_add(1);
                }
            }
            Ok(updated)
        })
    }

    pub fn mark_usage_limit_reached(
        &self,
        resets_at: Option<DateTime<Utc>>,
        snapshot: Option<crate::protocol::RateLimitSnapshot>,
    ) -> std::io::Result<()> {
        if self.get_auth_mode() != Some(ApiAuthMode::Chatgpt) {
            return Ok(());
        }
        self.update_store(|store| {
            let Some(active_id) = store.active_account_id.clone() else {
                return Ok(());
            };
            let Some(account) = store
                .accounts
                .iter_mut()
                .find(|account| account.id == active_id)
            else {
                return Err(std::io::Error::other("active account id not found"));
            };

            let now = Utc::now();
            let usage = account.usage.get_or_insert_with(AccountUsageCache::default);
            usage.last_seen_at = Some(now);
            if let Some(snapshot) = snapshot {
                usage.last_rate_limits = Some(snapshot);
            }

            let exhausted_until = exhausted_until(resets_at, usage.last_rate_limits.as_ref(), now);
            usage.exhausted_until = Some(exhausted_until);
            Ok(())
        })
    }

    pub fn switch_account_on_usage_limit(
        &self,
        required_workspace_id: Option<&str>,
        failing_store_account_id: Option<&str>,
        resets_at: Option<DateTime<Utc>>,
        snapshot: Option<crate::protocol::RateLimitSnapshot>,
        freshly_unsupported_store_account_ids: &HashSet<String>,
        protected_store_account_id: Option<&str>,
    ) -> std::io::Result<Option<String>> {
        if self.get_auth_mode() != Some(ApiAuthMode::Chatgpt) {
            return Ok(None);
        }

        let cooldown_check_now = Utc::now();
        let mut cooldown_until = self
            .usage_limit_auto_switch_cooldown_until
            .lock()
            .map_err(|_| std::io::Error::other("auto-switch cooldown lock poisoned"))?;
        if cooldown_until.is_some_and(|until| until > cooldown_check_now) {
            tracing::debug!(
                cooldown_until = ?*cooldown_until,
                "skipping usage-limit auto-switch during cooldown"
            );
            return Ok(None);
        }

        let switched_to = self.update_store(|store| {
            let mutation_now = Utc::now();
            let failing_store_account_id = match failing_store_account_id {
                Some(store_account_id) => {
                    if store
                        .accounts
                        .iter()
                        .any(|account| account.id == store_account_id)
                    {
                        Some(store_account_id.to_string())
                    } else {
                        return Ok(None);
                    }
                }
                _ => store.active_account_id.clone(),
            };

            let Some(failing_store_account_id) = failing_store_account_id else {
                return Ok(None);
            };

            let Some(failing_account) = store
                .accounts
                .iter_mut()
                .find(|account| account.id == failing_store_account_id)
            else {
                return Ok(None);
            };

            let usage = failing_account
                .usage
                .get_or_insert_with(AccountUsageCache::default);
            usage.last_seen_at = Some(mutation_now);
            if let Some(snapshot) = snapshot.clone() {
                usage.last_rate_limits = Some(snapshot);
            }
            usage.exhausted_until = Some(exhausted_until(
                resets_at,
                usage.last_rate_limits.as_ref(),
                mutation_now,
            ));

            // Merge-safety anchor: usage-limit auto-switch must purge fallback accounts whose
            // just-refreshed usage snapshot proves they are `free` or `unknown`, or `/accounts`
            // can immediately retry into a GPT-5.4-ineligible account.
            let removed_fallback_account_ids = store
                .accounts
                .iter()
                .filter(|account| {
                    account.id != failing_store_account_id
                        && Some(account.id.as_str()) != protected_store_account_id
                        && freshly_unsupported_store_account_ids.contains(&account.id)
                        && account_matches_required_workspace(account, required_workspace_id)
                })
                .map(|account| account.id.clone())
                .collect::<Vec<_>>();
            if !removed_fallback_account_ids.is_empty() {
                store.accounts.retain(|account| {
                    account.id == failing_store_account_id
                        || Some(account.id.as_str()) == protected_store_account_id
                        || !freshly_unsupported_store_account_ids.contains(&account.id)
                        || !account_matches_required_workspace(account, required_workspace_id)
                });
                let active_account_still_present =
                    store
                        .active_account_id
                        .as_ref()
                        .is_some_and(|active_account_id| {
                            store
                                .accounts
                                .iter()
                                .any(|account| &account.id == active_account_id)
                        });
                if !active_account_still_present {
                    store.active_account_id = Some(failing_store_account_id.clone())
                        .filter(|active_account_id| {
                            store
                                .accounts
                                .iter()
                                .any(|account| &account.id == active_account_id)
                        })
                        .or_else(|| store.accounts.first().map(|account| account.id.clone()));
                }
                tracing::info!(
                    removed_account_ids = ?removed_fallback_account_ids,
                    "removed freshly unsupported fallback accounts during usage-limit auto-switch"
                );
            }

            if let Some(protected_store_account_id) = protected_store_account_id
                && store.accounts.iter().any(|account| {
                    account.id == protected_store_account_id
                        && account_selectable(account, required_workspace_id, mutation_now)
                })
            {
                store.active_account_id = Some(protected_store_account_id.to_string());
                if let Some(next_account) = store
                    .accounts
                    .iter_mut()
                    .find(|account| account.id == protected_store_account_id)
                {
                    let usage = next_account
                        .usage
                        .get_or_insert_with(AccountUsageCache::default);
                    usage.last_seen_at = Some(mutation_now);
                }
                return Ok(Some(protected_store_account_id.to_string()));
            }

            let mut candidates = store
                .accounts
                .iter()
                .filter(|account| {
                    account.id != failing_store_account_id
                        && account_selectable(account, required_workspace_id, mutation_now)
                })
                .collect::<Vec<_>>();

            candidates.sort_by(|a, b| compare_auto_switch_candidates(a, b));
            let Some(next_account_id) = candidates.first().map(|account| account.id.clone()) else {
                return Ok(None);
            };

            store.active_account_id = Some(next_account_id.clone());
            if let Some(next_account) = store
                .accounts
                .iter_mut()
                .find(|account| account.id == next_account_id)
            {
                let usage = next_account
                    .usage
                    .get_or_insert_with(AccountUsageCache::default);
                usage.last_seen_at = Some(mutation_now);
            }

            Ok(Some(next_account_id))
        })?;
        let should_start_cooldown = switched_to
            .as_deref()
            .is_some_and(|switched_to| Some(switched_to) != protected_store_account_id);
        if should_start_cooldown {
            let cooldown_started_at = Utc::now();
            *cooldown_until = Some(
                cooldown_started_at
                    + chrono::Duration::seconds(USAGE_LIMIT_AUTO_SWITCH_COOLDOWN_SECONDS),
            );
        }
        Ok(switched_to)
    }

    pub fn select_account_for_auto_switch(
        &self,
        required_workspace_id: Option<&str>,
        exclude_store_account_id: Option<&str>,
    ) -> Option<String> {
        if self.get_auth_mode() != Some(ApiAuthMode::Chatgpt) {
            return None;
        }
        let store = self.inner.read().ok()?.store.clone();
        let now = Utc::now();
        let mut candidates = store
            .accounts
            .iter()
            .filter(|account| {
                Some(account.id.as_str()) != exclude_store_account_id
                    && account_selectable(account, required_workspace_id, now)
            })
            .collect::<Vec<_>>();

        candidates.sort_by(|a, b| compare_auto_switch_candidates(a, b));
        candidates.first().map(|account| account.id.clone())
    }

    pub fn accounts_rate_limits_cache_expires_at(
        &self,
        now: DateTime<Utc>,
    ) -> Option<DateTime<Utc>> {
        if self.get_auth_mode() != Some(ApiAuthMode::Chatgpt) {
            return None;
        }

        let guard = self.inner.read().ok()?;
        let store = &guard.store;

        let next_release_at = store
            .accounts
            .iter()
            .filter_map(|account| {
                account
                    .usage
                    .as_ref()
                    .and_then(|usage| usage.exhausted_until)
            })
            .filter(|until| *until > now)
            .min();
        if next_release_at.is_some() {
            return next_release_at;
        }

        store
            .accounts
            .iter()
            .filter_map(|account| {
                account
                    .usage
                    .as_ref()
                    .and_then(|usage| usage.last_rate_limits.as_ref())
            })
            .filter_map(|snapshot| snapshot_next_reset_at(snapshot, now))
            .min()
    }

    /// Current cached auth (clone). May be `None` if not logged in or load failed.
    /// Refreshes cached ChatGPT tokens if they are stale before returning.
    pub async fn auth(&self) -> Option<CodexAuth> {
        let auth = self.auth_cached()?;
        if let Err(err) = self.refresh_if_stale(&auth).await {
            tracing::error!("Failed to refresh token: {}", err);
            return Some(auth);
        }
        self.auth_cached()
    }

    /// Force a reload of the auth information from auth.json. Returns
    /// whether the auth value changed.
    pub fn reload(&self) -> bool {
        tracing::info!("Reloading auth");
        let store = self.load_store_from_storage();
        self.set_cached(store)
    }

    /// Like `reload()`, but fails loudly if the auth store cannot be loaded.
    pub fn reload_strict(&self) -> std::io::Result<bool> {
        tracing::info!("Reloading auth (strict)");
        let _lock = storage::lock_auth_store(&self.codex_home)?;
        let store = self.storage.load()?.unwrap_or_default();
        Ok(self.set_cached(store))
    }

    fn reload_if_account_id_matches(&self, expected_account_id: Option<&str>) -> ReloadOutcome {
        let expected_account_id = match expected_account_id {
            Some(account_id) => account_id,
            None => {
                tracing::info!("Skipping auth reload because no account id is available.");
                return ReloadOutcome::Skipped;
            }
        };

        let mut store = match self.storage.load() {
            Ok(Some(store)) => store,
            Ok(None) => {
                tracing::info!("Skipping auth reload because auth store is missing.");
                return ReloadOutcome::Skipped;
            }
            Err(err) => {
                tracing::warn!(
                    "Skipping auth reload because auth store could not be loaded: {err}"
                );
                return ReloadOutcome::Skipped;
            }
        };
        let removed_account_ids = enforce_supported_chatgpt_auth_accounts(&mut store);
        if !removed_account_ids.is_empty() {
            tracing::info!(
                removed_account_ids = ?removed_account_ids,
                "removed accounts with unsupported ChatGPT plans during guarded auth reload"
            );
            if let Err(error) =
                save_auth(&self.codex_home, &store, self.auth_credentials_store_mode)
            {
                tracing::warn!(
                    error = %error,
                    "failed to persist supported ChatGPT plan policy during guarded reload"
                );
                return ReloadOutcome::Skipped;
            }
        }

        let new_auth = Self::derive_auth_from_store(
            &store,
            Arc::clone(&self.storage),
            self.enable_codex_api_key_env,
        );
        let new_account_id = new_auth.as_ref().and_then(CodexAuth::get_account_id);

        if new_account_id.as_deref() != Some(expected_account_id) {
            if removed_account_ids
                .iter()
                .any(|id| id == expected_account_id)
            {
                tracing::info!(
                    expected_account_id,
                    "Reloading auth after expected account was removed by supported-plan policy"
                );
                self.set_cached(store);
                return ReloadOutcome::ReloadedChanged;
            }
            let found_account_id = new_account_id.as_deref().unwrap_or("unknown");
            tracing::info!(
                "Skipping auth reload due to account id mismatch (expected: {expected_account_id}, found: {found_account_id})"
            );
            return ReloadOutcome::Skipped;
        }

        tracing::info!("Reloading auth for account {expected_account_id}");
        let cached_before_reload = self.auth_cached();
        let auth_changed =
            !Self::auths_equal_for_refresh(cached_before_reload.as_ref(), new_auth.as_ref());
        self.set_cached(store);
        if auth_changed {
            ReloadOutcome::ReloadedChanged
        } else {
            ReloadOutcome::ReloadedNoChange
        }
    }

    fn auths_equal_for_refresh(a: Option<&CodexAuth>, b: Option<&CodexAuth>) -> bool {
        match (a, b) {
            (None, None) => true,
            (Some(a), Some(b)) => match (a.api_auth_mode(), b.api_auth_mode()) {
                (ApiAuthMode::ApiKey, ApiAuthMode::ApiKey) => a.api_key() == b.api_key(),
                (ApiAuthMode::Chatgpt, ApiAuthMode::Chatgpt)
                | (ApiAuthMode::ChatgptAuthTokens, ApiAuthMode::ChatgptAuthTokens) => {
                    a.get_current_auth_json() == b.get_current_auth_json()
                }
                _ => false,
            },
            _ => false,
        }
    }

    fn apply_refresh_to_cached_chatgpt_account(
        &self,
        store_account_id: &str,
        refreshed: &RefreshResponse,
    ) -> Result<(), RefreshTokenError> {
        let now = Utc::now();

        let refreshed_id_token = match refreshed.id_token.as_deref() {
            Some(id_token) => Some(
                parse_chatgpt_jwt_claims(id_token)
                    .map_err(|err| RefreshTokenError::Transient(std::io::Error::other(err)))?,
            ),
            None => None,
        };
        let refreshed_access_token = refreshed.access_token.clone();
        let refreshed_refresh_token = refreshed.refresh_token.clone();

        let Ok(mut guard) = self.inner.write() else {
            return Err(RefreshTokenError::Transient(std::io::Error::other(
                "failed to lock cached auth state",
            )));
        };

        let mut store = guard.store.clone();
        let Some(account) = store
            .accounts
            .iter_mut()
            .find(|account| account.id == store_account_id)
        else {
            return Err(RefreshTokenError::Transient(std::io::Error::other(
                "cached auth store is missing the refreshed account",
            )));
        };

        if let Some(id_token) = refreshed_id_token {
            account.tokens.id_token = id_token;
        }
        if let Some(access_token) = refreshed_access_token {
            account.tokens.access_token = access_token;
        }
        if let Some(refresh_token) = refreshed_refresh_token {
            account.tokens.refresh_token = refresh_token;
        }
        account.last_refresh = Some(now);

        store
            .validate()
            .map_err(|err| RefreshTokenError::Transient(std::io::Error::other(err)))?;

        let Some(auth) = Self::derive_chatgpt_auth_from_store_account(
            &store,
            store_account_id,
            Arc::clone(&self.storage),
        ) else {
            return Err(RefreshTokenError::Transient(std::io::Error::other(
                "failed to rebuild cached auth after refresh",
            )));
        };

        guard.store = store;
        guard.auth = Some(auth);
        Ok(())
    }

    fn derive_auth_from_store(
        store: &AuthStore,
        storage: Arc<dyn AuthStorageBackend>,
        enable_codex_api_key_env: bool,
    ) -> Option<CodexAuth> {
        if enable_codex_api_key_env && let Some(api_key) = read_codex_api_key_from_env() {
            let client = crate::default_client::create_client();
            return Some(CodexAuth::from_api_key_with_client(&api_key, client));
        }

        let client = crate::default_client::create_client();
        let active_account = store
            .active_account_id
            .as_deref()
            .and_then(|id| store.accounts.iter().find(|account| account.id == id))
            .or_else(|| store.accounts.first());

        if let Some(active_account) = active_account {
            let store_account_id = active_account.id.clone();
            let tokens = active_account.tokens.clone();
            let auth_dot_json = AuthDotJson {
                auth_mode: Some(ApiAuthMode::Chatgpt),
                openai_api_key: None,
                tokens: Some(tokens),
                last_refresh: active_account.last_refresh,
            };
            let state = ChatgptAuthState {
                auth_dot_json: Arc::new(Mutex::new(Some(auth_dot_json))),
                client,
            };
            return Some(CodexAuth::Chatgpt(ChatgptAuth {
                store_account_id,
                state,
                storage,
            }));
        }

        if let Some(api_key) = store.openai_api_key.as_deref() {
            return Some(CodexAuth::from_api_key_with_client(api_key, client));
        }

        None
    }

    fn load_store_from_storage(&self) -> AuthStore {
        match self.storage.load() {
            Ok(Some(mut store)) => {
                let removed_account_ids = enforce_supported_chatgpt_auth_accounts(&mut store);
                if !removed_account_ids.is_empty() {
                    tracing::info!(
                        removed_account_ids = ?removed_account_ids,
                        "removed accounts with unsupported ChatGPT plans while loading auth store"
                    );
                    if let Err(error) =
                        save_auth(&self.codex_home, &store, self.auth_credentials_store_mode)
                    {
                        tracing::warn!(
                            error = %error,
                            "failed to persist supported ChatGPT plan policy while loading store"
                        );
                    }
                }
                store
            }
            Ok(None) => AuthStore::default(),
            Err(err) => {
                tracing::warn!("Failed to load auth store: {err}");
                AuthStore::default()
            }
        }
    }

    fn set_cached(&self, store: AuthStore) -> bool {
        let new_auth = load_auth(
            &self.codex_home,
            self.enable_codex_api_key_env,
            self.auth_credentials_store_mode,
        )
        .ok()
        .flatten();
        if let Ok(mut guard) = self.inner.write() {
            let changed = guard.auth != new_auth;
            tracing::info!("Reloaded auth, changed: {changed}");
            guard.store = store;
            guard.auth = new_auth;
            changed
        } else {
            false
        }
    }

    fn derive_chatgpt_auth_from_store_account(
        store: &AuthStore,
        store_account_id: &str,
        storage: Arc<dyn AuthStorageBackend>,
    ) -> Option<CodexAuth> {
        let account = store
            .accounts
            .iter()
            .find(|account| account.id == store_account_id)?;

        let store_account_id = account.id.clone();
        let tokens = account.tokens.clone();
        let auth_dot_json = AuthDotJson {
            auth_mode: Some(ApiAuthMode::Chatgpt),
            openai_api_key: None,
            tokens: Some(tokens),
            last_refresh: account.last_refresh,
        };
        let state = ChatgptAuthState {
            auth_dot_json: Arc::new(Mutex::new(Some(auth_dot_json))),
            client: crate::default_client::create_client(),
        };
        Some(CodexAuth::Chatgpt(ChatgptAuth {
            store_account_id,
            state,
            storage,
        }))
    }

    fn update_store<T>(
        &self,
        mutator: impl FnOnce(&mut AuthStore) -> std::io::Result<T>,
    ) -> std::io::Result<T> {
        let _lock = storage::lock_auth_store(&self.codex_home)?;

        let mut store = match self.storage.load()? {
            Some(store) => store,
            None => {
                // `from_auth_for_testing` seeds an in-memory store without writing auth.json.
                // In that mode, treat the cached store as the source of truth if no stored
                // auth exists yet.
                if self._test_home_guard.is_some() {
                    self.inner
                        .read()
                        .ok()
                        .map(|cached| cached.store.clone())
                        .unwrap_or_default()
                } else {
                    AuthStore::default()
                }
            }
        };
        let removed_before_mutation = enforce_supported_chatgpt_auth_accounts(&mut store);
        if !removed_before_mutation.is_empty() {
            tracing::info!(
                removed_account_ids = ?removed_before_mutation,
                "removed accounts with unsupported ChatGPT plans before auth store mutation"
            );
        }

        let out = mutator(&mut store)?;
        let removed_after_mutation = enforce_supported_chatgpt_auth_accounts(&mut store);
        if !removed_after_mutation.is_empty() {
            tracing::info!(
                removed_account_ids = ?removed_after_mutation,
                "removed accounts with unsupported ChatGPT plans after auth store mutation"
            );
        }
        store.validate()?;
        self.storage.save(&store)?;
        self.set_cached(store);
        Ok(out)
    }

    pub fn set_external_auth_refresher(&self, refresher: Arc<dyn ExternalAuthRefresher>) {
        if let Ok(mut guard) = self.inner.write() {
            guard.external_refresher = Some(refresher);
        }
    }

    pub fn clear_external_auth_refresher(&self) {
        if let Ok(mut guard) = self.inner.write() {
            guard.external_refresher = None;
        }
    }

    pub fn set_forced_chatgpt_workspace_id(&self, workspace_id: Option<String>) {
        if let Ok(mut guard) = self.forced_chatgpt_workspace_id.write() {
            *guard = workspace_id;
        }
    }

    pub fn forced_chatgpt_workspace_id(&self) -> Option<String> {
        self.forced_chatgpt_workspace_id
            .read()
            .ok()
            .and_then(|guard| guard.clone())
    }

    pub fn has_external_auth_refresher(&self) -> bool {
        self.inner
            .read()
            .ok()
            .map(|guard| guard.external_refresher.is_some())
            .unwrap_or(false)
    }

    pub fn is_external_auth_active(&self) -> bool {
        self.auth_cached()
            .as_ref()
            .is_some_and(CodexAuth::is_external_chatgpt_tokens)
    }

    pub fn codex_api_key_env_enabled(&self) -> bool {
        self.enable_codex_api_key_env
    }

    /// Convenience constructor returning an `Arc` wrapper.
    pub fn shared(
        codex_home: PathBuf,
        enable_codex_api_key_env: bool,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
    ) -> Arc<Self> {
        Arc::new(Self::new(
            codex_home,
            enable_codex_api_key_env,
            auth_credentials_store_mode,
        ))
    }

    pub fn unauthorized_recovery(self: &Arc<Self>) -> UnauthorizedRecovery {
        UnauthorizedRecovery::new(Arc::clone(self))
    }

    /// Attempt to refresh the token by first performing a guarded reload. Auth
    /// is reloaded from storage only when the account id matches the currently
    /// cached account id. If the persisted token differs from the cached token, we
    /// can assume that some other instance already refreshed it. If the persisted
    /// token is the same as the cached, then ask the token authority to refresh.
    pub async fn refresh_token(&self) -> Result<(), RefreshTokenError> {
        let auth_before_reload = self.auth_cached();
        let expected_account_id = auth_before_reload
            .as_ref()
            .and_then(CodexAuth::get_account_id);

        match self.reload_if_account_id_matches(expected_account_id.as_deref()) {
            ReloadOutcome::ReloadedChanged => {
                tracing::info!("Skipping token refresh because auth changed after guarded reload.");
                Ok(())
            }
            ReloadOutcome::ReloadedNoChange => self.refresh_token_from_authority().await,
            ReloadOutcome::Skipped => {
                Err(RefreshTokenError::Permanent(RefreshTokenFailedError::new(
                    RefreshTokenFailedReason::Other,
                    REFRESH_TOKEN_ACCOUNT_MISMATCH_MESSAGE.to_string(),
                )))
            }
        }
    }

    /// Attempt to refresh the current auth token from the authority that issued
    /// the token. On success, reloads the auth state from disk so other components
    /// observe refreshed token. If the token refresh fails, returns the error to
    /// the caller.
    pub async fn refresh_token_from_authority(&self) -> Result<(), RefreshTokenError> {
        tracing::info!("Refreshing token");

        let auth = match self.auth_cached() {
            Some(auth) => auth,
            None => return Ok(()),
        };
        match auth {
            CodexAuth::ChatgptAuthTokens(_) => {
                self.refresh_external_auth(ExternalAuthRefreshReason::Unauthorized)
                    .await
            }
            CodexAuth::Chatgpt(chatgpt_auth) => {
                let token_data = chatgpt_auth.current_token_data().ok_or_else(|| {
                    RefreshTokenError::Transient(std::io::Error::other(
                        "Token data is not available.",
                    ))
                })?;
                let expected_account_id = token_data.account_id.clone();
                let refreshed = self
                    .refresh_and_persist_chatgpt_token(&chatgpt_auth, token_data.refresh_token)
                    .await?;

                match self.reload_if_account_id_matches(expected_account_id.as_deref()) {
                    ReloadOutcome::ReloadedChanged | ReloadOutcome::ReloadedNoChange => {
                        tracing::info!("Reloaded auth after token refresh");
                        Ok(())
                    }
                    ReloadOutcome::Skipped => {
                        tracing::info!(
                            store_account_id = chatgpt_auth.store_account_id.as_str(),
                            expected_account_id = expected_account_id.as_deref(),
                            "Skipping auth reload after token refresh; updating cached tokens"
                        );
                        self.apply_refresh_to_cached_chatgpt_account(
                            chatgpt_auth.store_account_id.as_str(),
                            &refreshed,
                        )
                    }
                }
            }
            CodexAuth::ApiKey(_) => Ok(()),
        }
    }

    /// Log out by deleting the on‑disk auth.json (if present). Returns Ok(true)
    /// if a file was removed, Ok(false) if no auth file existed. On success,
    /// reloads the in‑memory auth cache so callers immediately observe the
    /// unauthenticated state.
    pub fn logout(&self) -> std::io::Result<bool> {
        let removed = logout_all_stores(&self.codex_home, self.auth_credentials_store_mode)?;
        // Always reload to clear any cached auth (even if file absent).
        self.reload();
        Ok(removed)
    }

    pub fn get_api_auth_mode(&self) -> Option<ApiAuthMode> {
        self.auth_cached().as_ref().map(CodexAuth::api_auth_mode)
    }

    pub fn get_auth_mode(&self) -> Option<ApiAuthMode> {
        self.get_api_auth_mode()
    }

    pub fn auth_mode(&self) -> Option<AuthMode> {
        self.get_internal_auth_mode()
    }

    pub fn get_internal_auth_mode(&self) -> Option<AuthMode> {
        self.auth_cached()
            .as_ref()
            .map(CodexAuth::internal_auth_mode)
    }

    async fn refresh_if_stale(&self, auth: &CodexAuth) -> Result<bool, RefreshTokenError> {
        let chatgpt_auth = match auth {
            CodexAuth::Chatgpt(chatgpt_auth) => chatgpt_auth,
            _ => return Ok(false),
        };

        let auth_dot_json = match chatgpt_auth.current_auth_json() {
            Some(auth_dot_json) => auth_dot_json,
            None => return Ok(false),
        };
        let tokens = match auth_dot_json.tokens {
            Some(tokens) => tokens,
            None => return Ok(false),
        };
        let last_refresh = match auth_dot_json.last_refresh {
            Some(last_refresh) => last_refresh,
            None => return Ok(false),
        };
        if last_refresh >= Utc::now() - chrono::Duration::days(TOKEN_REFRESH_INTERVAL) {
            return Ok(false);
        }
        let expected_account_id = tokens.account_id.clone();
        let refreshed = self
            .refresh_and_persist_chatgpt_token(chatgpt_auth, tokens.refresh_token)
            .await?;
        match self.reload_if_account_id_matches(expected_account_id.as_deref()) {
            ReloadOutcome::ReloadedChanged | ReloadOutcome::ReloadedNoChange => {
                tracing::info!("Reloaded auth after stale token refresh");
            }
            ReloadOutcome::Skipped => {
                tracing::info!(
                    store_account_id = chatgpt_auth.store_account_id.as_str(),
                    expected_account_id = expected_account_id.as_deref(),
                    "Skipping auth reload after stale token refresh; updating cached tokens"
                );
                self.apply_refresh_to_cached_chatgpt_account(
                    chatgpt_auth.store_account_id.as_str(),
                    &refreshed,
                )?;
            }
        }
        Ok(true)
    }

    async fn refresh_external_auth(
        &self,
        reason: ExternalAuthRefreshReason,
    ) -> Result<(), RefreshTokenError> {
        let forced_chatgpt_workspace_id = self.forced_chatgpt_workspace_id();
        let refresher = match self.inner.read() {
            Ok(guard) => guard.external_refresher.clone(),
            Err(_) => {
                return Err(RefreshTokenError::Transient(std::io::Error::other(
                    "failed to read external auth state",
                )));
            }
        };

        let Some(refresher) = refresher else {
            return Err(RefreshTokenError::Transient(std::io::Error::other(
                "external auth refresher is not configured",
            )));
        };

        let previous_account_id = self
            .auth_cached()
            .as_ref()
            .and_then(CodexAuth::get_account_id);
        let context = ExternalAuthRefreshContext {
            reason,
            previous_account_id,
        };

        let refreshed = refresher.refresh(context).await?;
        let auth_dot_json =
            AuthDotJson::from_external_tokens(&refreshed, forced_chatgpt_workspace_id.as_deref())
                .map_err(|error| {
                if error.kind() == std::io::ErrorKind::InvalidData {
                    RefreshTokenError::Permanent(RefreshTokenFailedError::new(
                        RefreshTokenFailedReason::Other,
                        error.to_string(),
                    ))
                } else {
                    RefreshTokenError::Transient(error)
                }
            })?;
        let refreshed_store_account_id = auth_dot_json
            .tokens
            .as_ref()
            .and_then(TokenData::preferred_store_account_id);
        let mut store = AuthStore::from_legacy(auth_dot_json);
        let removed_account_ids = enforce_supported_chatgpt_auth_accounts(&mut store);
        if !removed_account_ids.is_empty() {
            tracing::info!(
                removed_account_ids = ?removed_account_ids,
                "removed accounts with unsupported ChatGPT plans after external auth refresh"
            );
        }
        save_auth(
            &self.codex_home,
            &store,
            AuthCredentialsStoreMode::Ephemeral,
        )
        .map_err(RefreshTokenError::Transient)?;
        self.reload();
        if refreshed_store_account_id
            .as_ref()
            .is_some_and(|store_account_id| {
                removed_account_ids
                    .iter()
                    .any(|removed_account_id| removed_account_id == store_account_id)
            })
        {
            return Err(RefreshTokenError::Permanent(RefreshTokenFailedError::new(
                RefreshTokenFailedReason::Other,
                EXTERNAL_SUPPORTED_CHATGPT_PLAN_REQUIRED_MESSAGE.to_string(),
            )));
        }
        Ok(())
    }

    // Refreshes ChatGPT OAuth tokens and persists updated auth state for the
    // current cached account.
    async fn refresh_and_persist_chatgpt_token(
        &self,
        auth: &ChatgptAuth,
        refresh_token: String,
    ) -> Result<RefreshResponse, RefreshTokenError> {
        let refresh_response = request_chatgpt_token_refresh(refresh_token, auth.client()).await?;
        let refresh_id_token = refresh_response.id_token.clone();
        let refresh_access_token = refresh_response.access_token.clone();
        let refresh_refresh_token = refresh_response.refresh_token.clone();

        let updated_store = update_tokens(
            &self.codex_home,
            auth.storage(),
            auth.store_account_id.as_str(),
            refresh_id_token,
            refresh_access_token,
            refresh_refresh_token,
        )
        .await
        .map_err(RefreshTokenError::from)?;
        if !updated_store
            .accounts
            .iter()
            .any(|account| account.id == auth.store_account_id)
        {
            self.reload();
            return Err(RefreshTokenError::Permanent(RefreshTokenFailedError::new(
                RefreshTokenFailedReason::Other,
                UNSUPPORTED_CHATGPT_PLAN_REMOVED_MESSAGE.to_string(),
            )));
        }

        Ok(refresh_response)
    }
}

/// Merge-safety anchor: `/accounts` and `/logout` render this exact summary from
/// `AuthManager::list_accounts`; keep field semantics aligned with TUI account flows.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountSummary {
    pub id: String,
    pub label: Option<String>,
    pub email: Option<String>,
    pub is_active: bool,
    pub exhausted_until: Option<DateTime<Utc>>,
    pub last_rate_limits: Option<crate::protocol::RateLimitSnapshot>,
}

impl AccountSummary {
    fn from_stored(account: &StoredAccount, active_id: Option<&str>) -> Self {
        Self {
            id: account.id.clone(),
            label: account.label.clone(),
            email: account.tokens.id_token.email.clone(),
            is_active: active_id == Some(account.id.as_str()),
            exhausted_until: account.usage.as_ref().and_then(|u| u.exhausted_until),
            last_rate_limits: account
                .usage
                .as_ref()
                .and_then(|u| u.last_rate_limits.clone()),
        }
    }
}

fn store_from_auth_for_testing(auth: &CodexAuth) -> AuthStore {
    match auth {
        CodexAuth::ApiKey(api_key) => AuthStore {
            openai_api_key: Some(api_key.api_key.clone()),
            ..AuthStore::default()
        },
        CodexAuth::Chatgpt(chatgpt) => {
            let Some(auth_dot_json) = chatgpt.current_auth_json() else {
                return AuthStore::default();
            };
            let Some(tokens) = auth_dot_json.tokens else {
                return AuthStore::default();
            };

            AuthStore {
                openai_api_key: auth_dot_json.openai_api_key,
                active_account_id: Some(chatgpt.store_account_id.clone()),
                accounts: vec![StoredAccount {
                    id: chatgpt.store_account_id.clone(),
                    label: None,
                    tokens,
                    last_refresh: auth_dot_json.last_refresh,
                    usage: None,
                }],
                ..AuthStore::default()
            }
        }
        CodexAuth::ChatgptAuthTokens(chatgpt) => {
            let Some(auth_dot_json) = ({
                #[expect(clippy::unwrap_used)]
                chatgpt.state.auth_dot_json.lock().unwrap().clone()
            }) else {
                return AuthStore::default();
            };
            let Some(tokens) = auth_dot_json.tokens else {
                return AuthStore::default();
            };

            AuthStore {
                openai_api_key: auth_dot_json.openai_api_key,
                active_account_id: Some(chatgpt.store_account_id.clone()),
                accounts: vec![StoredAccount {
                    id: chatgpt.store_account_id.clone(),
                    label: None,
                    tokens,
                    last_refresh: auth_dot_json.last_refresh,
                    usage: None,
                }],
                ..AuthStore::default()
            }
        }
    }
}

fn is_supported_chatgpt_auth_account(account: &StoredAccount) -> bool {
    account.tokens.id_token.is_supported_chatgpt_auth_plan()
}

fn enforce_supported_chatgpt_auth_accounts(store: &mut AuthStore) -> Vec<String> {
    let mut removed_account_ids = Vec::new();
    store.accounts.retain(|account| {
        let keep_account = is_supported_chatgpt_auth_account(account);
        if !keep_account {
            removed_account_ids.push(account.id.clone());
        }
        keep_account
    });

    let has_active_account = store
        .active_account_id
        .as_ref()
        .is_some_and(|active_account_id| {
            store
                .accounts
                .iter()
                .any(|account| &account.id == active_account_id)
        });
    if !has_active_account {
        store.active_account_id = store.accounts.first().map(|account| account.id.clone());
    }

    removed_account_ids
}

fn exhausted_until(
    resets_at: Option<DateTime<Utc>>,
    snapshot: Option<&crate::protocol::RateLimitSnapshot>,
    now: DateTime<Utc>,
) -> DateTime<Utc> {
    let from_snapshot = snapshot.and_then(|snapshot| {
        snapshot
            .secondary
            .as_ref()
            .and_then(|w| w.resets_at)
            .or_else(|| snapshot.primary.as_ref().and_then(|w| w.resets_at))
            .and_then(|seconds| DateTime::<Utc>::from_timestamp(seconds, 0))
    });
    resets_at
        .or(from_snapshot)
        .unwrap_or_else(|| now + chrono::Duration::minutes(15))
}

fn exhausted_until_from_snapshot(
    snapshot: &crate::protocol::RateLimitSnapshot,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    if rate_limit_window_blocked(snapshot.secondary.as_ref(), now) {
        return Some(
            rate_limit_window_reset_at(snapshot.secondary.as_ref())
                .unwrap_or_else(|| exhausted_until(None, Some(snapshot), now)),
        );
    }
    if rate_limit_window_blocked(snapshot.primary.as_ref(), now) {
        return Some(
            rate_limit_window_reset_at(snapshot.primary.as_ref())
                .unwrap_or_else(|| exhausted_until(None, Some(snapshot), now)),
        );
    }
    None
}

fn rate_limit_window_blocked(
    window: Option<&crate::protocol::RateLimitWindow>,
    now: DateTime<Utc>,
) -> bool {
    let Some(window) = window else {
        return false;
    };

    if let Some(resets_at_seconds) = window.resets_at
        && let Some(resets_at) = DateTime::<Utc>::from_timestamp(resets_at_seconds, 0)
        && now >= resets_at
    {
        return false;
    }

    window.used_percent >= 100.0
}

fn rate_limit_window_reset_at(
    window: Option<&crate::protocol::RateLimitWindow>,
) -> Option<DateTime<Utc>> {
    let window = window?;
    let resets_at_seconds = window.resets_at?;
    DateTime::<Utc>::from_timestamp(resets_at_seconds, 0)
}

fn snapshot_next_reset_at(
    snapshot: &crate::protocol::RateLimitSnapshot,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    [
        rate_limit_window_reset_at(snapshot.primary.as_ref()),
        rate_limit_window_reset_at(snapshot.secondary.as_ref()),
    ]
    .into_iter()
    .flatten()
    .filter(|reset_at| *reset_at > now)
    .min()
}

fn account_matches_required_workspace(
    account: &StoredAccount,
    required_workspace_id: Option<&str>,
) -> bool {
    if let Some(required) = required_workspace_id
        && account.tokens.id_token.chatgpt_account_id.as_deref() != Some(required)
    {
        return false;
    }

    true
}

pub(crate) fn usage_limit_auto_switch_removes_plan_type(
    plan_type: Option<&AccountPlanType>,
) -> bool {
    matches!(
        plan_type,
        Some(AccountPlanType::Free | AccountPlanType::Unknown)
    )
}

fn account_selectable(
    account: &StoredAccount,
    required_workspace_id: Option<&str>,
    now: DateTime<Utc>,
) -> bool {
    if !account_matches_required_workspace(account, required_workspace_id) {
        return false;
    }

    if let Some(until) = account.usage.as_ref().and_then(|u| u.exhausted_until)
        && until > now
    {
        return false;
    }

    true
}

fn compare_auto_switch_candidates(a: &StoredAccount, b: &StoredAccount) -> std::cmp::Ordering {
    let a_snapshot = a.usage.as_ref().and_then(|u| u.last_rate_limits.as_ref());
    let b_snapshot = b.usage.as_ref().and_then(|u| u.last_rate_limits.as_ref());

    let (a_weekly_kind, a_weekly_used) = weekly_used_percent_rank(a_snapshot);
    let (b_weekly_kind, b_weekly_used) = weekly_used_percent_rank(b_snapshot);

    let (a_credit_kind, a_balance) = credits_balance_rank(a_snapshot);
    let (b_credit_kind, b_balance) = credits_balance_rank(b_snapshot);

    let (a_primary_kind, a_primary_used) = primary_used_percent_rank(a_snapshot);
    let (b_primary_kind, b_primary_used) = primary_used_percent_rank(b_snapshot);

    let a_last_seen = a
        .usage
        .as_ref()
        .and_then(|u| u.last_seen_at)
        .map_or(i64::MIN, |dt| dt.timestamp());
    let b_last_seen = b
        .usage
        .as_ref()
        .and_then(|u| u.last_seen_at)
        .map_or(i64::MIN, |dt| dt.timestamp());

    (
        a_weekly_kind,
        Reverse(a_weekly_used),
        a_credit_kind,
        a_balance,
        a_primary_kind,
        Reverse(a_primary_used),
        a_last_seen,
        a.id.as_str(),
    )
        .cmp(&(
            b_weekly_kind,
            Reverse(b_weekly_used),
            b_credit_kind,
            b_balance,
            b_primary_kind,
            Reverse(b_primary_used),
            b_last_seen,
            b.id.as_str(),
        ))
}

fn credits_balance_rank(snapshot: Option<&crate::protocol::RateLimitSnapshot>) -> (u8, i64) {
    let Some(snapshot) = snapshot else {
        return (1, i64::MAX);
    };
    let Some(credits) = snapshot.credits.as_ref() else {
        return (1, i64::MAX);
    };
    if !credits.has_credits {
        return (1, i64::MAX);
    }
    if credits.unlimited {
        return (2, i64::MAX);
    }
    let Some(raw) = credits.balance.as_deref() else {
        return (1, i64::MAX);
    };
    match parse_credits_balance(raw) {
        Some(balance) => (0, balance),
        None => (1, i64::MAX),
    }
}

fn weekly_used_percent_rank(snapshot: Option<&crate::protocol::RateLimitSnapshot>) -> (u8, i64) {
    let Some(snapshot) = snapshot else {
        return (1, 0);
    };
    let Some(window) = snapshot.secondary.as_ref() else {
        return (1, 0);
    };
    if window.window_minutes.is_some() {
        return (1, 0);
    }
    (0, percent_basis_points(window.used_percent))
}

fn primary_used_percent_rank(snapshot: Option<&crate::protocol::RateLimitSnapshot>) -> (u8, i64) {
    let Some(snapshot) = snapshot else {
        return (1, 0);
    };
    let Some(window) = snapshot.primary.as_ref() else {
        return (1, 0);
    };
    (0, percent_basis_points(window.used_percent))
}

fn percent_basis_points(percent: f64) -> i64 {
    let clamped = percent.clamp(0.0, 100.0);
    (clamped * 100.0).round() as i64
}

fn parse_credits_balance(raw: &str) -> Option<i64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(value) = trimmed.parse::<i64>() {
        return Some(value);
    }
    if let Ok(value) = trimmed.parse::<f64>()
        && value.is_finite()
    {
        return Some(value.round() as i64);
    }
    None
}

#[cfg(test)]
#[path = "auth_tests.rs"]
mod tests;
