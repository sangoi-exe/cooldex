use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use reqwest::StatusCode;
use reqwest::header::HeaderValue;
use serde::Deserialize;
use serde::Serialize;
#[cfg(test)]
use serial_test::serial;
use std::env;
use std::fmt::Debug;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::RwLock;
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::watch;

use codex_account_state::AccountStateStore;
use codex_app_server_protocol::AuthMode;
use codex_app_server_protocol::AuthMode as ApiAuthMode;
use codex_protocol::config_types::ForcedLoginMethod;
use codex_protocol::config_types::ModelProviderAuthInfo;

#[cfg(test)]
use super::account_manager::ACTIVE_ACCOUNT_LEASE_TTL_SECONDS;
#[cfg(test)]
use super::account_manager::AccountLeaseState;
use super::account_manager::AccountManager;
use super::account_manager::AccountRuntimeLoadError;
use super::account_manager::AccountRuntimeMutation;
use super::account_manager::ActiveChatgptAccountSummary;
use super::account_manager::ChatgptAuthAdmissionPolicy;
use super::account_manager::GuardedReloadLoadedStore;
use super::account_manager::LoadedAuthStore;
use super::account_manager::LoadedStoreOrigin;
use super::account_manager::account_matches_required_workspace;
use super::account_manager::enforce_chatgpt_auth_accounts;
use super::account_manager::strip_runtime_active_account_from_store;
use super::external_bearer::BearerTokenRefresher;
use super::revoke::revoke_auth_tokens;
pub use crate::auth::storage::AccountUsageCache;
pub use crate::auth::storage::AgentIdentityAuthRecord;
pub use crate::auth::storage::AuthDotJson;
use crate::auth::storage::AuthStorageBackend;
pub use crate::auth::storage::AuthStore;
pub use crate::auth::storage::StoredAccount;
use crate::auth::storage::create_auth_storage;
use crate::auth::storage::{self};
use crate::auth::util::try_parse_error_message;
use crate::default_client::create_client;
use crate::token_data::TokenData;
use crate::token_data::parse_chatgpt_jwt_claims;
use crate::token_data::parse_jwt_expiration;
use codex_client::CodexHttpClient;
use codex_config::types::AuthCredentialsStoreMode;
use codex_protocol::account::PlanType as AccountPlanType;
use codex_protocol::auth::KnownPlan as InternalKnownPlan;
use codex_protocol::auth::PlanType as InternalPlanType;
use codex_protocol::auth::RefreshTokenFailedError;
use codex_protocol::auth::RefreshTokenFailedReason;
use serde_json::Value;
use thiserror::Error;

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
    state: ChatgptAuthState,
    storage: Arc<dyn AuthStorageBackend>,
}

#[derive(Debug, Clone)]
pub struct ChatgptAuthTokens {
    state: ChatgptAuthState,
    storage: Arc<dyn AuthStorageBackend>,
}

/// Request-auth snapshot for first-party ChatGPT backend calls.
///
/// This is a value snapshot derived by [`AuthManager`] from the AccountManager-owned
/// runtime account. Network/cache leaf helpers should consume this type instead of
/// constructing their own AuthManager or treating raw token data as a request owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatGptRequestAuth {
    authorization: String,
    account_id: String,
    chatgpt_user_id: Option<String>,
    is_workspace_account: bool,
    is_fedramp_account: bool,
}

impl ChatGptRequestAuth {
    pub(crate) fn from_auth(auth: &CodexAuth) -> Result<Option<Self>, AccountRuntimeLoadError> {
        if !auth.is_chatgpt_auth() {
            return Ok(None);
        }
        let token_data = auth.get_token_data().map_err(|error| {
            AccountRuntimeLoadError::request_auth_materialization(error.to_string())
        })?;
        let access_token = token_data.access_token.trim();
        if access_token.is_empty() {
            return Err(AccountRuntimeLoadError::request_auth_materialization(
                "ChatGPT access token is empty",
            ));
        }
        let account_id = token_data
            .account_id
            .clone()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| {
                AccountRuntimeLoadError::request_auth_materialization(
                    "ChatGPT account id is missing",
                )
            })?;
        let authorization = format!("Bearer {access_token}");
        HeaderValue::from_str(&authorization).map_err(|error| {
            AccountRuntimeLoadError::request_auth_materialization(format!(
                "ChatGPT authorization header is invalid: {error}"
            ))
        })?;
        HeaderValue::from_str(&account_id).map_err(|error| {
            AccountRuntimeLoadError::request_auth_materialization(format!(
                "ChatGPT account id header is invalid: {error}"
            ))
        })?;
        Ok(Some(Self {
            authorization,
            account_id,
            chatgpt_user_id: token_data.id_token.chatgpt_user_id.clone(),
            is_workspace_account: token_data.id_token.is_workspace_account(),
            is_fedramp_account: token_data.id_token.is_fedramp_account(),
        }))
    }

    pub fn authorization(&self) -> &str {
        self.authorization.as_str()
    }

    pub fn account_id(&self) -> &str {
        self.account_id.as_str()
    }

    pub fn chatgpt_user_id(&self) -> Option<&str> {
        self.chatgpt_user_id.as_deref()
    }

    pub fn is_workspace_account(&self) -> bool {
        self.is_workspace_account
    }

    pub fn is_fedramp_account(&self) -> bool {
        self.is_fedramp_account
    }

    /// Consider this private to integration tests.
    pub fn create_dummy_for_testing() -> Self {
        let auth = CodexAuth::create_dummy_chatgpt_auth_for_testing();
        match Self::from_auth(&auth) {
            Ok(Some(request_auth)) => request_auth,
            Ok(None) => panic!("dummy ChatGPT auth should produce request auth"),
            Err(error) => panic!("dummy request auth should be constructible: {error}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChatGptAuthContext {
    codex_auth: CodexAuth,
    request_auth: ChatGptRequestAuth,
}

impl ChatGptAuthContext {
    fn from_auth(codex_auth: CodexAuth) -> Result<Option<Self>, AccountRuntimeLoadError> {
        let Some(request_auth) = ChatGptRequestAuth::from_auth(&codex_auth)? else {
            return Ok(None);
        };
        Ok(Some(Self {
            codex_auth,
            request_auth,
        }))
    }

    pub fn codex_auth(&self) -> &CodexAuth {
        &self.codex_auth
    }

    pub fn request_auth(&self) -> &ChatGptRequestAuth {
        &self.request_auth
    }

    pub fn into_parts(self) -> (CodexAuth, ChatGptRequestAuth) {
        (self.codex_auth, self.request_auth)
    }
}

#[derive(Debug, Clone)]
struct ChatgptAuthState {
    active_account: ActiveChatgptAccountSnapshot,
    client: CodexHttpClient,
}

#[derive(Debug, Clone, PartialEq)]
struct ActiveChatgptAccountSnapshot {
    store_account_id: String,
    label: Option<String>,
    tokens: TokenData,
    last_refresh: Option<DateTime<Utc>>,
    auth_mode: ApiAuthMode,
}

impl PartialEq for CodexAuth {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::ApiKey(a), Self::ApiKey(b)) => a.api_key == b.api_key,
            (Self::Chatgpt(a), Self::Chatgpt(b)) => {
                a.state.active_account.store_account_id == b.state.active_account.store_account_id
            }
            (Self::ChatgptAuthTokens(a), Self::ChatgptAuthTokens(b)) => {
                a.state.active_account.store_account_id == b.state.active_account.store_account_id
            }
            _ => false,
        }
    }
}

const TOKEN_REFRESH_INTERVAL: i64 = 8;

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
pub(super) const REVOKE_TOKEN_URL: &str = "https://auth.openai.com/oauth/revoke";
pub const REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR: &str = "CODEX_REFRESH_TOKEN_URL_OVERRIDE";
pub const REVOKE_TOKEN_URL_OVERRIDE_ENV_VAR: &str = "CODEX_REVOKE_TOKEN_URL_OVERRIDE";

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
    pub chatgpt_metadata: Option<ExternalAuthChatgptMetadata>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalAuthChatgptMetadata {
    pub account_id: String,
    pub plan_type: Option<String>,
}

impl ExternalAuthTokens {
    pub fn access_token_only(access_token: impl Into<String>) -> Self {
        Self {
            access_token: access_token.into(),
            chatgpt_metadata: None,
        }
    }

    pub fn chatgpt(
        access_token: impl Into<String>,
        chatgpt_account_id: impl Into<String>,
        chatgpt_plan_type: Option<String>,
    ) -> Self {
        Self {
            access_token: access_token.into(),
            chatgpt_metadata: Some(ExternalAuthChatgptMetadata {
                account_id: chatgpt_account_id.into(),
                plan_type: chatgpt_plan_type,
            }),
        }
    }

    pub fn chatgpt_metadata(&self) -> Option<&ExternalAuthChatgptMetadata> {
        self.chatgpt_metadata.as_ref()
    }
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

/// Refresh policy for resolving a saved ChatGPT account before using it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatgptAccountRefreshMode {
    /// Reuse the stored account auth snapshot as-is.
    Never,
    /// Refresh the account only when the cached access token looks stale.
    IfStale,
    /// Force a refresh attempt before returning the account.
    Force,
}

/// Result of resolving a saved ChatGPT account from the auth store.
#[derive(Clone, Debug, PartialEq)]
pub enum ChatgptAccountAuthResolution {
    /// The stored account is still usable and resolved to a current auth snapshot.
    Auth(Box<ChatGptAuthContext>),
    /// The stored account was removed because refresh-token failure is terminal.
    Removed {
        error: RefreshTokenFailedError,
        switched_to_store_account_id: Option<String>,
    },
    /// The requested stored account was already absent.
    Missing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TerminalRefreshFailureAccountRemoval {
    NotRemoved,
    Removed {
        switched_to_store_account_id: Option<String>,
    },
}

#[async_trait]
/// Pluggable auth provider used by `AuthManager` for externally managed auth flows.
///
/// Implementations may either resolve auth eagerly via `resolve()` or provide refreshed
/// credentials on demand via `refresh()`.
pub trait ExternalAuth: Send + Sync {
    /// Indicates which top-level auth mode this external provider supplies.
    fn auth_mode(&self) -> AuthMode;

    /// Returns cached or immediately available auth, if this provider can resolve it synchronously
    /// from the caller's perspective.
    async fn resolve(&self) -> std::io::Result<Option<ExternalAuthTokens>> {
        Ok(None)
    }

    /// Refreshes auth in response to a manager-driven refresh attempt.
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

fn open_account_state_store(sqlite_home: &Path) -> AccountStateStore {
    AccountStateStore::open(sqlite_home.to_path_buf()).unwrap_or_else(|error| {
        tracing::error!(
            error = %error,
            sqlite_home = %sqlite_home.display(),
            "failed to open required account state store"
        );
        panic!(
            "failed to open required account state store at {}: {error}",
            sqlite_home.display()
        );
    })
}

impl From<RefreshTokenError> for std::io::Error {
    fn from(err: RefreshTokenError) -> Self {
        match err {
            RefreshTokenError::Permanent(failed) => std::io::Error::other(failed),
            RefreshTokenError::Transient(inner) => inner,
        }
    }
}

impl ActiveChatgptAccountSnapshot {
    fn from_stored_account(account: &StoredAccount, auth_mode: ApiAuthMode) -> Self {
        Self {
            store_account_id: account.id.clone(),
            label: account.label.clone(),
            tokens: account.tokens.clone(),
            last_refresh: account.last_refresh,
            auth_mode,
        }
    }

    fn summary(&self) -> ActiveChatgptAccountSummary {
        ActiveChatgptAccountSummary {
            store_account_id: self.store_account_id.clone(),
            label: self.label.clone(),
            email: self.tokens.id_token.email.clone(),
            auth_mode: self.auth_mode,
        }
    }

    fn matches_refresh_snapshot(&self, other: &Self) -> bool {
        self.store_account_id == other.store_account_id
            && self.tokens == other.tokens
            && self.last_refresh == other.last_refresh
            && self.auth_mode == other.auth_mode
    }
}

impl CodexAuth {
    fn from_chatgpt_active_account_snapshot(
        active_account: ActiveChatgptAccountSnapshot,
        storage: Option<Arc<dyn AuthStorageBackend>>,
    ) -> std::io::Result<Self> {
        let state = ChatgptAuthState {
            active_account,
            client: create_client(),
        };

        match state.active_account.auth_mode {
            ApiAuthMode::Chatgpt => {
                let Some(storage) = storage else {
                    return Err(std::io::Error::other(
                        "ChatGPT auth is missing a backing auth store.",
                    ));
                };
                Ok(Self::Chatgpt(ChatgptAuth { state, storage }))
            }
            ApiAuthMode::ChatgptAuthTokens => {
                let Some(storage) = storage else {
                    return Err(std::io::Error::other(
                        "ChatGPT auth tokens are missing a backing auth store.",
                    ));
                };
                Ok(Self::ChatgptAuthTokens(ChatgptAuthTokens {
                    state,
                    storage,
                }))
            }
            ApiAuthMode::ApiKey => Err(std::io::Error::other(
                "API key auth cannot be built from a ChatGPT account snapshot.",
            )),
        }
    }

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

    pub fn auth_mode(&self) -> AuthMode {
        match self {
            Self::ApiKey(_) => AuthMode::ApiKey,
            Self::Chatgpt(_) => AuthMode::Chatgpt,
            Self::ChatgptAuthTokens(_) => AuthMode::ChatgptAuthTokens,
        }
    }

    pub fn internal_auth_mode(&self) -> AuthMode {
        self.auth_mode()
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
        matches!(
            self.internal_auth_mode(),
            AuthMode::Chatgpt | AuthMode::ChatgptAuthTokens
        )
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
        match self.current_chatgpt_account_snapshot() {
            Some(active_account) if active_account.last_refresh.is_some() => {
                Ok(active_account.tokens.clone())
            }
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
        self.current_chatgpt_account_snapshot()
            .and_then(|active_account| active_account.tokens.account_id.clone())
    }

    /// Returns false if `is_chatgpt_auth()` is false or the token omits the FedRAMP claim.
    pub fn is_fedramp_account(&self) -> bool {
        self.get_current_token_data()
            .is_some_and(|t| t.id_token.is_fedramp_account())
    }

    /// Returns `None` if `is_chatgpt_auth()` is false.
    pub fn get_account_email(&self) -> Option<String> {
        self.current_chatgpt_account_snapshot()
            .and_then(|active_account| active_account.tokens.id_token.email.clone())
    }

    /// Returns `None` if `is_chatgpt_auth()` is false.
    pub fn get_chatgpt_user_id(&self) -> Option<String> {
        self.current_chatgpt_account_snapshot()
            .and_then(|active_account| active_account.tokens.id_token.chatgpt_user_id.clone())
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
            InternalKnownPlan::ProLite => AccountPlanType::ProLite,
            InternalKnownPlan::Team => AccountPlanType::Team,
            InternalKnownPlan::SelfServeBusinessUsageBased => {
                AccountPlanType::SelfServeBusinessUsageBased
            }
            InternalKnownPlan::Business => AccountPlanType::Business,
            InternalKnownPlan::EnterpriseCbpUsageBased => AccountPlanType::EnterpriseCbpUsageBased,
            InternalKnownPlan::Enterprise => AccountPlanType::Enterprise,
            InternalKnownPlan::Edu => AccountPlanType::Edu,
        };

        self.current_chatgpt_account_snapshot()
            .map(|active_account| {
                active_account
                    .tokens
                    .id_token
                    .chatgpt_plan_type
                    .as_ref()
                    .map(|pt| match pt {
                        InternalPlanType::Known(k) => map_known(k),
                        InternalPlanType::Unknown(_) => AccountPlanType::Unknown,
                    })
                    .unwrap_or(AccountPlanType::Unknown)
            })
    }

    /// Returns `None` if `is_chatgpt_auth()` is false.
    fn current_chatgpt_account_snapshot(&self) -> Option<&ActiveChatgptAccountSnapshot> {
        let state = match self {
            Self::Chatgpt(auth) => &auth.state,
            Self::ChatgptAuthTokens(auth) => &auth.state,
            Self::ApiKey(_) => return None,
        };
        Some(&state.active_account)
    }

    fn get_current_auth_json(&self) -> Option<AuthDotJson> {
        let (active_account, storage) = match self {
            Self::Chatgpt(auth) => (&auth.state.active_account, &auth.storage),
            Self::ChatgptAuthTokens(auth) => (&auth.state.active_account, &auth.storage),
            Self::ApiKey(_) => return None,
        };
        let agent_identity = storage
            .load()
            .ok()
            .flatten()
            .and_then(|store| store.agent_identity);
        Some(AuthDotJson {
            auth_mode: Some(active_account.auth_mode),
            openai_api_key: None,
            tokens: Some(active_account.tokens.clone()),
            last_refresh: active_account.last_refresh,
            agent_identity,
        })
    }

    fn get_current_token_data(&self) -> Option<TokenData> {
        self.current_chatgpt_account_snapshot()
            .map(|active_account| active_account.tokens.clone())
    }

    pub fn active_chatgpt_account_summary(&self) -> Option<ActiveChatgptAccountSummary> {
        self.current_chatgpt_account_snapshot()
            .map(ActiveChatgptAccountSnapshot::summary)
    }

    pub fn get_agent_identity(&self, workspace_id: &str) -> Option<AgentIdentityAuthRecord> {
        self.get_current_auth_json()
            .and_then(|auth| auth.agent_identity)
            .filter(|identity| identity.workspace_id == workspace_id)
    }

    pub fn set_agent_identity(&self, record: AgentIdentityAuthRecord) -> std::io::Result<()> {
        let storage = match self {
            Self::Chatgpt(auth) => &auth.storage,
            Self::ChatgptAuthTokens(auth) => &auth.storage,
            Self::ApiKey(_) => return Ok(()),
        };
        let mut store = storage
            .load()?
            .ok_or_else(|| std::io::Error::other("auth data is not available"))?;
        store.agent_identity = Some(record);
        storage.save(&store)?;
        Ok(())
    }

    pub fn remove_agent_identity(&self) -> std::io::Result<bool> {
        let storage = match self {
            Self::Chatgpt(auth) => &auth.storage,
            Self::ChatgptAuthTokens(auth) => &auth.storage,
            Self::ApiKey(_) => return Ok(false),
        };
        let Some(mut store) = storage.load()? else {
            return Ok(false);
        };
        let removed = store.agent_identity.take().is_some();
        if removed {
            storage.save(&store)?;
        }
        Ok(removed)
    }

    /// Consider this private to integration tests.
    pub fn create_dummy_chatgpt_auth_for_testing() -> Self {
        let active_account = ActiveChatgptAccountSnapshot {
            store_account_id: "account_id".to_string(),
            label: None,
            tokens: TokenData {
                id_token: Default::default(),
                access_token: "Access Token".to_string(),
                refresh_token: "test".to_string(),
                account_id: Some("account_id".to_string()),
            },
            last_refresh: Some(Utc::now()),
            auth_mode: ApiAuthMode::Chatgpt,
        };

        let storage = create_auth_storage(PathBuf::new(), AuthCredentialsStoreMode::File);
        Self::from_chatgpt_active_account_snapshot(active_account, Some(storage))
            .unwrap_or_else(|error| panic!("dummy ChatGPT auth should be constructible: {error}"))
    }

    pub fn from_api_key(api_key: &str) -> Self {
        Self::ApiKey(ApiKeyAuth {
            api_key: api_key.to_owned(),
        })
    }
}

impl ChatgptAuth {
    fn current_chatgpt_account_snapshot(&self) -> &ActiveChatgptAccountSnapshot {
        &self.state.active_account
    }

    fn store_account_id(&self) -> &str {
        self.current_chatgpt_account_snapshot()
            .store_account_id
            .as_str()
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

pub async fn logout_with_revoke(config: &impl AuthManagerConfig) -> std::io::Result<bool> {
    // Merge-safety anchor: CLI logout-with-revoke is a config-aware production
    // path and must construct AuthManager with resolved sqlite_home, not an
    // implicit codex_home fallback that leaves WS12 leases in a foreign DB.
    AuthManager::shared_from_config(config, /*enable_codex_api_key_env*/ false)
        .map_err(AccountRuntimeLoadError::into_io_error)?
        .logout_with_revoke()
        .await
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
    if !enforce_chatgpt_auth_accounts(&mut store, ChatgptAuthAdmissionPolicy::ExternalStrict)
        .is_empty()
    {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthConfig {
    pub codex_home: PathBuf,
    pub auth_credentials_store_mode: AuthCredentialsStoreMode,
    pub forced_login_method: Option<ForcedLoginMethod>,
    pub forced_chatgpt_workspace_id: Option<String>,
}

pub fn enforce_login_restrictions(config: &AuthConfig) -> std::io::Result<()> {
    let auth_state = load_auth_preflight_state(
        &config.codex_home,
        /*enable_codex_api_key_env*/ true,
        config.auth_credentials_store_mode,
        config.forced_chatgpt_workspace_id.as_deref(),
    )?;
    if auth_state == PreflightAuthState::None {
        return Ok(());
    }

    if let Some(required_method) = config.forced_login_method {
        let method_violation = match (required_method, auth_state) {
            (ForcedLoginMethod::Api, PreflightAuthState::ApiKey) => None,
            (ForcedLoginMethod::Chatgpt, PreflightAuthState::Chatgpt { .. }) => None,
            (ForcedLoginMethod::Api, PreflightAuthState::Chatgpt { .. }) => Some(
                "API key login is required, but ChatGPT is currently being used. Logging out."
                    .to_string(),
            ),
            (ForcedLoginMethod::Chatgpt, PreflightAuthState::ApiKey) => Some(
                "ChatGPT login is required, but an API key is currently being used. Logging out."
                    .to_string(),
            ),
            (_, PreflightAuthState::None) => None,
        };

        if let Some(message) = method_violation {
            return logout_with_message(
                &config.codex_home,
                message,
                config.auth_credentials_store_mode,
            );
        }
    }

    if let Some(expected_account_id) = config.forced_chatgpt_workspace_id.as_deref() {
        if auth_state == PreflightAuthState::ApiKey {
            return Ok(());
        }
        if !matches!(
            auth_state,
            PreflightAuthState::Chatgpt {
                has_matching_workspace: true
            }
        ) {
            return logout_with_message(
                &config.codex_home,
                format!(
                    "Login is restricted to workspace {expected_account_id}, but no saved ChatGPT account matches it. Logging out."
                ),
                config.auth_credentials_store_mode,
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
    // API key via env var takes precedence over any other auth method.
    if enable_codex_api_key_env && let Some(api_key) = read_codex_api_key_from_env() {
        return Ok(Some(CodexAuth::from_api_key(api_key.as_str())));
    }

    // External ChatGPT auth tokens live in the in-memory (ephemeral) store. Always check this
    // first so external auth takes precedence over any persisted credentials.
    let ephemeral_storage = create_auth_storage(
        codex_home.to_path_buf(),
        AuthCredentialsStoreMode::Ephemeral,
    );
    if let Some(mut store) = ephemeral_storage.load()? {
        let removed_account_ids =
            enforce_chatgpt_auth_accounts(&mut store, ChatgptAuthAdmissionPolicy::ExternalStrict);
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
        if let Some(active_account) = store.active_account() {
            let auth = CodexAuth::from_chatgpt_active_account_snapshot(
                ActiveChatgptAccountSnapshot::from_stored_account(
                    active_account,
                    ApiAuthMode::ChatgptAuthTokens,
                ),
                Some(Arc::clone(&ephemeral_storage)),
            )?;
            return Ok(Some(auth));
        }

        if !store.accounts.is_empty() {
            return Ok(None);
        }

        if let Some(api_key) = store.openai_api_key.as_deref() {
            return Ok(Some(CodexAuth::from_api_key(api_key)));
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

    if let Some(active_account) = store.active_account() {
        let auth = CodexAuth::from_chatgpt_active_account_snapshot(
            ActiveChatgptAccountSnapshot::from_stored_account(active_account, ApiAuthMode::Chatgpt),
            Some(storage),
        )?;
        return Ok(Some(auth));
    }

    if !store.accounts.is_empty() {
        return Ok(None);
    }

    if let Some(api_key) = store.openai_api_key.as_deref() {
        return Ok(Some(CodexAuth::from_api_key(api_key)));
    }

    Ok(None)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreflightAuthState {
    None,
    ApiKey,
    Chatgpt { has_matching_workspace: bool },
}

fn preflight_state_from_store(
    store: &mut AuthStore,
    required_workspace_id: Option<&str>,
    admission_policy: ChatgptAuthAdmissionPolicy,
) -> PreflightAuthState {
    enforce_chatgpt_auth_accounts(store, admission_policy);
    if !store.accounts.is_empty() {
        return PreflightAuthState::Chatgpt {
            has_matching_workspace: store
                .accounts
                .iter()
                .any(|account| account_matches_required_workspace(account, required_workspace_id)),
        };
    }
    if store.openai_api_key.is_some() {
        return PreflightAuthState::ApiKey;
    }
    PreflightAuthState::None
}

pub fn load_auth_preflight_state(
    codex_home: &Path,
    enable_codex_api_key_env: bool,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    required_workspace_id: Option<&str>,
) -> std::io::Result<PreflightAuthState> {
    if enable_codex_api_key_env && read_codex_api_key_from_env().is_some() {
        return Ok(PreflightAuthState::ApiKey);
    }

    let ephemeral_storage = create_auth_storage(
        codex_home.to_path_buf(),
        AuthCredentialsStoreMode::Ephemeral,
    );
    if let Some(mut store) = ephemeral_storage.load()? {
        let preflight_state = preflight_state_from_store(
            &mut store,
            required_workspace_id,
            ChatgptAuthAdmissionPolicy::ExternalStrict,
        );
        if preflight_state != PreflightAuthState::None {
            return Ok(preflight_state);
        }
    }

    if auth_credentials_store_mode == AuthCredentialsStoreMode::Ephemeral {
        return Ok(PreflightAuthState::None);
    }

    let storage = create_auth_storage(codex_home.to_path_buf(), auth_credentials_store_mode);
    let mut store = match storage.load()? {
        Some(store) => store,
        None => return Ok(PreflightAuthState::None),
    };
    Ok(preflight_state_from_store(
        &mut store,
        required_workspace_id,
        ChatgptAuthAdmissionPolicy::Persisted,
    ))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PersistedActiveAccountWriteMode {
    #[cfg(test)]
    Preserve,
    Strip,
}

async fn update_tokens(
    codex_home: &Path,
    storage: &Arc<dyn AuthStorageBackend>,
    store_account_id: &str,
    id_token: Option<String>,
    access_token: Option<String>,
    refresh_token: Option<String>,
    persisted_active_account_write_mode: PersistedActiveAccountWriteMode,
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
    let removed_account_ids =
        enforce_chatgpt_auth_accounts(&mut store, ChatgptAuthAdmissionPolicy::Persisted);
    if !removed_account_ids.is_empty() {
        tracing::info!(
            removed_account_ids = ?removed_account_ids,
            "removed accounts with unsupported ChatGPT plans from auth store"
        );
    }
    store.validate()?;
    let mut persisted_store = store.clone();
    if persisted_active_account_write_mode == PersistedActiveAccountWriteMode::Strip {
        strip_runtime_active_account_from_store(&mut persisted_store);
    }
    storage.save(&persisted_store)?;
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
        Some("refresh_token_invalidated") | Some("token_revoked") => {
            RefreshTokenFailedReason::Revoked
        }
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

impl AuthDotJson {
    fn from_external_tokens(
        external: &ExternalAuthTokens,
        required_workspace_id: Option<&str>,
    ) -> std::io::Result<Self> {
        let Some(chatgpt_metadata) = external.chatgpt_metadata() else {
            return Err(std::io::Error::other(
                "external auth tokens are missing ChatGPT metadata",
            ));
        };
        let token_info = validate_external_access_token_claims(
            &external.access_token,
            &chatgpt_metadata.account_id,
            chatgpt_metadata.plan_type.as_deref(),
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
            agent_identity: None,
        })
    }

    fn from_external_access_token(
        access_token: &str,
        chatgpt_account_id: &str,
        chatgpt_plan_type: Option<&str>,
        required_workspace_id: Option<&str>,
    ) -> std::io::Result<Self> {
        let external = ExternalAuthTokens::chatgpt(
            access_token,
            chatgpt_account_id,
            chatgpt_plan_type.map(str::to_string),
        );
        Self::from_external_tokens(&external, required_workspace_id)
    }
}

/// Internal cached auth state.
#[derive(Clone)]
struct CachedAuth {
    store: AuthStore,
    store_origin: LoadedStoreOrigin,
    auth: Option<CodexAuth>,
    /// Permanent refresh failure cached for the current auth snapshot so
    /// later refresh attempts for the same credentials fail fast without network.
    permanent_refresh_failure: Option<AuthScopedRefreshFailure>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum AuthStateNotificationMode {
    Notify,
    Silent,
}

#[derive(Clone)]
struct AuthScopedRefreshFailure {
    auth: CodexAuth,
    error: RefreshTokenFailedError,
}

impl Debug for CachedAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachedAuth")
            .field("store_origin", &self.store_origin)
            .field(
                "auth_mode",
                &self.auth.as_ref().map(CodexAuth::api_auth_mode),
            )
            .field(
                "permanent_refresh_failure",
                &self
                    .permanent_refresh_failure
                    .as_ref()
                    .map(|failure| failure.error.reason),
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
// 1. Attempt to reload the auth data from disk. We only reload if the saved-account id matches
//    the one the current process is running as.
// 2. Attempt to refresh the token using OAuth token refresh flow.
// If after both steps the server still responds with 401 we let the error bubble to the user.
//
// For external auth sources, UnauthorizedRecovery retries once.
//
// - External ChatGPT auth tokens (`chatgptAuthTokens`) are refreshed by asking
//   the parent app for new tokens through the configured
//   `ExternalAuth`, persisting them in the ephemeral auth store, and
//   reloading the cached auth snapshot.
// - External bearer auth sources for custom model providers rerun the provider
//   auth command without touching disk.
pub struct UnauthorizedRecovery {
    manager: Arc<AuthManager>,
    step: UnauthorizedRecoveryStep,
    expected_store_account_id: Option<String>,
    mode: UnauthorizedRecoveryMode,
    load_error: Option<AccountRuntimeLoadError>,
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
        let (cached_auth, load_error) = match manager.auth_cached() {
            Ok(cached_auth) => (cached_auth, None),
            Err(error) => (None, Some(error)),
        };
        let expected_store_account_id = cached_auth
            .as_ref()
            .and_then(CodexAuth::active_chatgpt_account_summary)
            .map(|summary| summary.store_account_id);
        let mode = if manager.has_external_api_key_auth()
            || cached_auth
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
            expected_store_account_id,
            mode,
            load_error,
        }
    }

    pub fn has_next(&self) -> bool {
        if self.load_error.is_some() {
            return !matches!(self.step, UnauthorizedRecoveryStep::Done);
        }
        if self.manager.has_external_api_key_auth() {
            return !matches!(self.step, UnauthorizedRecoveryStep::Done);
        }

        let Ok(cached_auth) = self.manager.auth_cached() else {
            return true;
        };
        if !cached_auth.as_ref().is_some_and(CodexAuth::is_chatgpt_auth) {
            return false;
        }

        if self.mode == UnauthorizedRecoveryMode::External && !self.manager.has_external_auth() {
            return false;
        }

        !matches!(self.step, UnauthorizedRecoveryStep::Done)
    }

    pub fn unavailable_reason(&self) -> &'static str {
        if self.manager.has_external_api_key_auth() {
            return if matches!(self.step, UnauthorizedRecoveryStep::Done) {
                "recovery_exhausted"
            } else {
                "ready"
            };
        }

        let Ok(cached_auth) = self.manager.auth_cached() else {
            return "runtime_owner_load_failed";
        };
        if !cached_auth.as_ref().is_some_and(CodexAuth::is_chatgpt_auth) {
            return "not_chatgpt_auth";
        }

        if self.mode == UnauthorizedRecoveryMode::External && !self.manager.has_external_auth() {
            return "no_external_auth";
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
        if let Some(error) = self.load_error.take() {
            self.step = UnauthorizedRecoveryStep::Done;
            return Err(RefreshTokenError::Transient(error.into_io_error()));
        }
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
                    .reload_if_store_account_id_matches(self.expected_store_account_id.as_deref())
                    .map_err(|error| RefreshTokenError::Transient(error.into_io_error()))?
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

/// Auth facade for auth.json-derived authentication data.
///
/// `AuthManager` owns auth derivation and cached `CodexAuth` handoff, while
/// saved-account roster, runtime-active account state, leases, and usage truth
/// stay under [`AccountManager`]. Live account readers reload through
/// `AccountManager` so external account-store changes can update the cache at
/// explicit account-state boundaries.
pub struct AuthManager {
    codex_home: PathBuf,
    storage: Arc<dyn AuthStorageBackend>,
    account_manager: AccountManager,
    inner: RwLock<CachedAuth>,
    enable_codex_api_key_env: bool,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    _test_home_guard: Option<tempfile::TempDir>,
    refresh_lock: AsyncMutex<()>,
    external_auth: RwLock<Option<Arc<dyn ExternalAuth>>>,
    auth_state_tx: watch::Sender<()>,
}

/// Configuration view required to construct a shared [`AuthManager`].
///
/// Implementations should return the auth-related config values for the
/// already-resolved runtime configuration. The primary implementation is
/// `codex_core::config::Config`, but this trait keeps `codex-login` independent
/// from `codex-core`.
pub trait AuthManagerConfig {
    /// Returns the Codex home directory used for auth storage.
    fn codex_home(&self) -> PathBuf;

    /// Returns the SQLite home directory used for shared runtime account state.
    fn sqlite_home(&self) -> PathBuf;

    /// Returns the CLI auth credential storage mode for auth loading.
    fn cli_auth_credentials_store_mode(&self) -> AuthCredentialsStoreMode;

    /// Returns the workspace ID that ChatGPT auth should be restricted to, if any.
    fn forced_chatgpt_workspace_id(&self) -> Option<String>;
}

impl Debug for AuthManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthManager")
            .field("codex_home", &self.codex_home)
            .field("account_manager", &self.account_manager)
            .field("inner", &self.inner)
            .field("enable_codex_api_key_env", &self.enable_codex_api_key_env)
            .field(
                "auth_credentials_store_mode",
                &self.auth_credentials_store_mode,
            )
            .field("has_external_auth", &self.has_external_auth())
            .finish_non_exhaustive()
    }
}

impl AuthManager {
    /// Create a new manager loading the initial auth using the provided
    /// preferred auth method. AccountManager-owned load failures are returned
    /// instead of being converted into an unauthenticated cache.
    pub fn new(
        codex_home: PathBuf,
        enable_codex_api_key_env: bool,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
    ) -> Result<Self, AccountRuntimeLoadError> {
        Self::new_with_sqlite_home(
            codex_home.clone(),
            codex_home,
            enable_codex_api_key_env,
            auth_credentials_store_mode,
        )
    }

    pub fn new_with_sqlite_home(
        codex_home: PathBuf,
        sqlite_home: PathBuf,
        enable_codex_api_key_env: bool,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
    ) -> Result<Self, AccountRuntimeLoadError> {
        Self::new_with_sqlite_home_and_forced_workspace(
            codex_home,
            sqlite_home,
            /*forced_chatgpt_workspace_id*/ None,
            enable_codex_api_key_env,
            auth_credentials_store_mode,
        )
    }

    fn new_with_sqlite_home_and_forced_workspace(
        codex_home: PathBuf,
        sqlite_home: PathBuf,
        forced_chatgpt_workspace_id: Option<String>,
        enable_codex_api_key_env: bool,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
    ) -> Result<Self, AccountRuntimeLoadError> {
        Self::new_with_sqlite_home_workspace_and_linked_session(
            codex_home,
            sqlite_home,
            forced_chatgpt_workspace_id,
            /*linked_codex_session_id*/ None,
            enable_codex_api_key_env,
            auth_credentials_store_mode,
        )
    }

    fn new_with_sqlite_home_workspace_and_linked_session(
        codex_home: PathBuf,
        sqlite_home: PathBuf,
        forced_chatgpt_workspace_id: Option<String>,
        linked_codex_session_id: Option<String>,
        enable_codex_api_key_env: bool,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
    ) -> Result<Self, AccountRuntimeLoadError> {
        let (auth_state_tx, _) = watch::channel(());
        let storage = create_auth_storage(codex_home.clone(), auth_credentials_store_mode);
        let account_state_store = Some(open_account_state_store(sqlite_home.as_path()));
        let runtime_session_id = uuid::Uuid::new_v4().to_string();
        // Merge-safety anchor: config-aware AuthManager construction must install
        // forced ChatGPT workspace and linked Codex session before AccountManager
        // hydrates runtime-active account state; post-construction setters are too
        // late for cached auth and cloud-requirements bootstrap.
        let account_manager = AccountManager::new_with_runtime_context(
            codex_home.clone(),
            Arc::clone(&storage),
            auth_credentials_store_mode,
            account_state_store,
            runtime_session_id,
            linked_codex_session_id,
            forced_chatgpt_workspace_id,
        );
        let loaded = account_manager.load_store_from_storage()?;
        let store = loaded.store;
        let store_origin = loaded.store_origin;
        let auth = Self::derive_auth_from_store(
            &store,
            &codex_home,
            Arc::clone(&storage),
            enable_codex_api_key_env,
            store_origin,
        );
        Ok(Self {
            codex_home,
            storage,
            account_manager,
            inner: RwLock::new(CachedAuth {
                store,
                store_origin,
                auth,
                permanent_refresh_failure: None,
            }),
            enable_codex_api_key_env,
            auth_credentials_store_mode,
            _test_home_guard: None,
            refresh_lock: AsyncMutex::new(()),
            external_auth: RwLock::new(None),
            auth_state_tx,
        })
    }

    /// Create an AuthManager with a specific CodexAuth, for testing only.
    pub fn from_auth_for_testing(auth: CodexAuth) -> Arc<Self> {
        let temp_dir = tempfile::tempdir().unwrap_or_else(|err| panic!("temp codex home: {err}"));
        let codex_home = temp_dir.path().to_path_buf();
        let storage = create_auth_storage(codex_home.clone(), AuthCredentialsStoreMode::File);
        let account_state_store = Some(open_account_state_store(codex_home.as_path()));
        let store = store_from_auth_for_testing(&auth);
        storage
            .save(&store)
            .unwrap_or_else(|error| panic!("seed test auth store: {error}"));
        let (auth_state_tx, _) = watch::channel(());
        let cached = CachedAuth {
            store,
            store_origin: LoadedStoreOrigin::Persistent,
            auth: Some(auth),
            permanent_refresh_failure: None,
        };
        let account_manager = AccountManager::new(
            codex_home.clone(),
            Arc::clone(&storage),
            AuthCredentialsStoreMode::File,
            account_state_store,
            uuid::Uuid::new_v4().to_string(),
            /*forced_chatgpt_workspace_id*/ None,
        );

        Arc::new(Self {
            codex_home,
            storage,
            account_manager,
            inner: RwLock::new(cached),
            enable_codex_api_key_env: false,
            auth_credentials_store_mode: AuthCredentialsStoreMode::File,
            _test_home_guard: Some(temp_dir),
            refresh_lock: AsyncMutex::new(()),
            external_auth: RwLock::new(None),
            auth_state_tx,
        })
    }

    /// Create an AuthManager with a specific CodexAuth and codex home, for testing only.
    pub fn from_auth_for_testing_with_home(auth: CodexAuth, codex_home: PathBuf) -> Arc<Self> {
        let storage = create_auth_storage(codex_home.clone(), AuthCredentialsStoreMode::File);
        let account_state_store = Some(open_account_state_store(codex_home.as_path()));
        let store = store_from_auth_for_testing(&auth);
        storage
            .save(&store)
            .unwrap_or_else(|error| panic!("seed test auth store: {error}"));
        let (auth_state_tx, _) = watch::channel(());
        let cached = CachedAuth {
            store,
            store_origin: LoadedStoreOrigin::Persistent,
            auth: Some(auth),
            permanent_refresh_failure: None,
        };
        let account_manager = AccountManager::new(
            codex_home.clone(),
            Arc::clone(&storage),
            AuthCredentialsStoreMode::File,
            account_state_store,
            uuid::Uuid::new_v4().to_string(),
            /*forced_chatgpt_workspace_id*/ None,
        );
        Arc::new(Self {
            codex_home,
            storage,
            account_manager,
            inner: RwLock::new(cached),
            enable_codex_api_key_env: false,
            auth_credentials_store_mode: AuthCredentialsStoreMode::File,
            _test_home_guard: None,
            refresh_lock: AsyncMutex::new(()),
            external_auth: RwLock::new(None),
            auth_state_tx,
        })
    }

    pub fn external_bearer_only(config: ModelProviderAuthInfo) -> Arc<Self> {
        let codex_home = PathBuf::from("non-existent");
        let storage = create_auth_storage(codex_home.clone(), AuthCredentialsStoreMode::File);
        let (auth_state_tx, _) = watch::channel(());
        let account_manager = AccountManager::new(
            codex_home.clone(),
            Arc::clone(&storage),
            AuthCredentialsStoreMode::File,
            /*account_state_store*/ None,
            uuid::Uuid::new_v4().to_string(),
            /*forced_chatgpt_workspace_id*/ None,
        );
        Arc::new(Self {
            codex_home,
            storage,
            account_manager,
            inner: RwLock::new(CachedAuth {
                store: AuthStore::default(),
                store_origin: LoadedStoreOrigin::Persistent,
                auth: None,
                permanent_refresh_failure: None,
            }),
            enable_codex_api_key_env: false,
            auth_credentials_store_mode: AuthCredentialsStoreMode::File,
            _test_home_guard: None,
            refresh_lock: AsyncMutex::new(()),
            external_auth: RwLock::new(Some(
                Arc::new(BearerTokenRefresher::new(config)) as Arc<dyn ExternalAuth>
            )),
            auth_state_tx,
        })
    }

    /// Current cached auth (clone) without attempting a refresh.
    pub fn auth_cached(&self) -> Result<Option<CodexAuth>, AccountRuntimeLoadError> {
        let (mut store, store_origin) = {
            let guard = self.inner.read().map_err(|_| {
                AccountRuntimeLoadError::RuntimeActiveAccount(
                    "auth cache lock poisoned".to_string(),
                )
            })?;
            (guard.store.clone(), guard.store_origin)
        };
        self.account_manager
            .hydrate_runtime_active_account(&mut store)
            .map_err(|error| AccountRuntimeLoadError::RuntimeActiveAccount(error.to_string()))?;
        let auth = Self::derive_auth_from_store(
            &store,
            &self.codex_home,
            Arc::clone(&self.storage),
            self.enable_codex_api_key_env,
            store_origin,
        );
        if auth.is_some() || store.accounts.is_empty() {
            self.set_cached_with_auth(store, auth.clone(), store_origin);
            return Ok(auth);
        }

        // Merge-safety anchor: WS12 bearerless recovery must refresh from persisted store truth
        // when runtime-aware cached auth still has saved accounts but no active bearer, or a stale
        // in-memory store can disagree with `/accounts` and strand the session without auth.
        tracing::warn!(
            cached_store_account_count = store.accounts.len(),
            runtime_session_id = self.account_manager.runtime_session_id(),
            linked_codex_session_id = ?self.account_manager.linked_codex_session_id().as_deref(),
            "cached auth store had saved accounts but no active account after runtime hydration; reloading auth store from disk"
        );
        let loaded = self.load_store_from_storage()?;
        let reloaded_auth = Self::derive_auth_from_store(
            &loaded.store,
            &self.codex_home,
            Arc::clone(&self.storage),
            self.enable_codex_api_key_env,
            loaded.store_origin,
        );
        self.set_cached_with_auth(loaded.store, reloaded_auth.clone(), loaded.store_origin);
        Ok(reloaded_auth)
    }

    pub async fn chatgpt_auth(
        &self,
    ) -> Result<Option<ChatGptAuthContext>, AccountRuntimeLoadError> {
        let Some(auth) = self.current_chatgpt_auth_from_storage()? else {
            return Ok(None);
        };
        if !Self::is_stale_for_proactive_refresh(&auth) {
            return ChatGptAuthContext::from_auth(auth);
        }

        let Some(store_account_id) = auth
            .active_chatgpt_account_summary()
            .map(|summary| summary.store_account_id)
        else {
            return ChatGptAuthContext::from_auth(auth);
        };

        match self
            .resolve_chatgpt_auth_for_store_account_id(
                &store_account_id,
                ChatgptAccountRefreshMode::IfStale,
            )
            .await
        {
            Ok(ChatgptAccountAuthResolution::Auth(auth)) => Ok(Some(*auth)),
            Ok(ChatgptAccountAuthResolution::Removed { error, .. }) => {
                tracing::error!("Failed to refresh ChatGPT request token: {}", error);
                match self.current_chatgpt_auth_from_storage()? {
                    Some(auth) => ChatGptAuthContext::from_auth(auth),
                    None => Ok(None),
                }
            }
            Ok(ChatgptAccountAuthResolution::Missing) => Ok(None),
            Err(err) => {
                tracing::error!("Failed to refresh ChatGPT request token: {}", err);
                match self.current_chatgpt_auth_from_storage()? {
                    Some(auth) => ChatGptAuthContext::from_auth(auth),
                    None => Ok(None),
                }
            }
        }
    }

    pub async fn chatgpt_request_auth(
        &self,
    ) -> Result<Option<ChatGptRequestAuth>, AccountRuntimeLoadError> {
        Ok(self
            .chatgpt_auth()
            .await?
            .map(|context| context.request_auth().clone()))
    }

    fn current_chatgpt_auth_from_storage(
        &self,
    ) -> Result<Option<CodexAuth>, AccountRuntimeLoadError> {
        let loaded = self.load_store_from_storage()?;
        // Merge-safety anchor: ChatGPT-only request snapshots are allowed to
        // load AccountManager-owned store truth, but they must not overwrite the
        // generic AuthManager cache used by API-key-capable callers that share
        // this runtime owner, such as `codex exec` plus cloud requirements.
        Ok(Self::derive_auth_from_store(
            &loaded.store,
            &self.codex_home,
            Arc::clone(&self.storage),
            /*enable_codex_api_key_env*/ false,
            loaded.store_origin,
        )
        .filter(CodexAuth::is_chatgpt_auth))
    }

    fn chatgpt_auth_for_store_account_id(
        &self,
        store_account_id: &str,
    ) -> Result<Option<(CodexAuth, LoadedStoreOrigin)>, AccountRuntimeLoadError> {
        let loaded = self.load_store_from_storage()?;
        let auth = Self::derive_chatgpt_auth_from_store_account(
            &loaded.store,
            store_account_id,
            Self::storage_for_store_origin(&self.codex_home, &self.storage, loaded.store_origin),
            loaded.store_origin,
        );
        Ok(auth.map(|auth| (auth, loaded.store_origin)))
    }

    // Merge-safety anchor: `/accounts`, usage-limit auto-switch, and active auth recovery must use
    // one canonical owner for per-account refresh failure eviction.
    pub async fn resolve_chatgpt_auth_for_store_account_id(
        &self,
        store_account_id: &str,
        refresh_mode: ChatgptAccountRefreshMode,
    ) -> Result<ChatgptAccountAuthResolution, RefreshTokenError> {
        if matches!(
            refresh_mode,
            ChatgptAccountRefreshMode::IfStale | ChatgptAccountRefreshMode::Force
        ) {
            // Merge-safety anchor: every ChatGPT account refresh path, including
            // caller-owned request snapshots, must share the same serialization
            // boundary as `refresh_token()` so concurrent stale snapshots do not
            // submit the same rotating refresh token in parallel.
            let _refresh_guard = self.refresh_lock.lock().await;
            return self
                .resolve_chatgpt_auth_for_store_account_id_unlocked(store_account_id, refresh_mode)
                .await;
        }

        self.resolve_chatgpt_auth_for_store_account_id_unlocked(store_account_id, refresh_mode)
            .await
    }

    async fn resolve_chatgpt_auth_for_store_account_id_unlocked(
        &self,
        store_account_id: &str,
        refresh_mode: ChatgptAccountRefreshMode,
    ) -> Result<ChatgptAccountAuthResolution, RefreshTokenError> {
        let Some((auth, store_origin)) =
            self.chatgpt_auth_for_store_account_id(store_account_id)
                .map_err(|error| RefreshTokenError::Transient(error.into_io_error()))?
        else {
            return Ok(ChatgptAccountAuthResolution::Missing);
        };
        let CodexAuth::Chatgpt(chatgpt_auth) = &auth else {
            return Self::chatgpt_account_auth_resolution(auth);
        };

        let cached_refresh_failure = self
            .auth_cached()
            .map_err(|error| RefreshTokenError::Transient(error.into_io_error()))?
            .as_ref()
            .and_then(|cached_auth| match cached_auth {
                CodexAuth::Chatgpt(cached_chatgpt_auth)
                    if cached_chatgpt_auth.store_account_id()
                        == chatgpt_auth.store_account_id() =>
                {
                    self.refresh_failure_for_auth(cached_auth)
                }
                _ => None,
            });
        if let Some(error) = cached_refresh_failure.or_else(|| self.refresh_failure_for_auth(&auth))
        {
            return if let TerminalRefreshFailureAccountRemoval::Removed {
                switched_to_store_account_id,
            } = self.remove_chatgpt_store_account_for_terminal_refresh_failure(
                chatgpt_auth.store_account_id(),
                store_origin,
                &error,
            )? {
                Ok(ChatgptAccountAuthResolution::Removed {
                    error,
                    switched_to_store_account_id,
                })
            } else {
                Err(RefreshTokenError::Permanent(error))
            };
        }

        let should_refresh = match refresh_mode {
            ChatgptAccountRefreshMode::Never => false,
            ChatgptAccountRefreshMode::IfStale => Self::is_stale_for_proactive_refresh(&auth),
            ChatgptAccountRefreshMode::Force => true,
        };
        if !should_refresh {
            return Self::chatgpt_account_auth_resolution(auth);
        }

        let token_data = chatgpt_auth
            .current_chatgpt_account_snapshot()
            .tokens
            .clone();
        match self
            .refresh_and_persist_chatgpt_token(chatgpt_auth, token_data.refresh_token)
            .await
        {
            Ok(_) => {
                self.reload()
                    .map_err(|error| RefreshTokenError::Transient(error.into_io_error()))?;
                match self
                    .chatgpt_auth_for_store_account_id(store_account_id)
                    .map_err(|error| RefreshTokenError::Transient(error.into_io_error()))?
                    .map(|(auth, _store_origin)| auth)
                {
                    Some(auth) => Self::chatgpt_account_auth_resolution(auth),
                    None => Ok(ChatgptAccountAuthResolution::Missing),
                }
            }
            Err(RefreshTokenError::Permanent(error)) => {
                if let TerminalRefreshFailureAccountRemoval::Removed {
                    switched_to_store_account_id,
                } = self.remove_chatgpt_store_account_for_terminal_refresh_failure(
                    chatgpt_auth.store_account_id(),
                    store_origin,
                    &error,
                )? {
                    Ok(ChatgptAccountAuthResolution::Removed {
                        error,
                        switched_to_store_account_id,
                    })
                } else {
                    Err(RefreshTokenError::Permanent(error))
                }
            }
            Err(err) => Err(err),
        }
    }

    fn chatgpt_account_auth_resolution(
        auth: CodexAuth,
    ) -> Result<ChatgptAccountAuthResolution, RefreshTokenError> {
        let Some(context) = ChatGptAuthContext::from_auth(auth)
            .map_err(|error| RefreshTokenError::Transient(error.into_io_error()))?
        else {
            return Ok(ChatgptAccountAuthResolution::Missing);
        };
        Ok(ChatgptAccountAuthResolution::Auth(Box::new(context)))
    }

    pub fn refresh_auth_after_account_runtime_mutation<T>(
        &self,
        mutation: AccountRuntimeMutation<T>,
    ) -> T {
        // Merge-safety anchor: AuthManager only consumes AccountManager's
        // opaque active-mutation token to refresh derived auth cache; it must
        // not regain account-runtime mutation ownership.
        let (out, loaded) = mutation.into_parts();
        if let Some(loaded) = loaded {
            self.set_cached(loaded.store, loaded.store_origin);
        }
        out
    }

    pub fn refresh_failure_for_auth(&self, auth: &CodexAuth) -> Option<RefreshTokenFailedError> {
        self.inner.read().ok().and_then(|cached| {
            cached
                .permanent_refresh_failure
                .as_ref()
                .filter(|failure| Self::auths_equal_for_refresh(Some(auth), Some(&failure.auth)))
                .map(|failure| failure.error.clone())
        })
    }
    /// Current request auth. Uses cached auth on the hot path, reloads through
    /// AccountManager when the cache is empty, and refreshes stale managed
    /// ChatGPT auth only if the on-disk auth is unchanged.
    pub async fn auth(&self) -> Result<Option<CodexAuth>, AccountRuntimeLoadError> {
        if let Some(auth) = self.resolve_external_api_key_auth().await {
            return Ok(Some(auth));
        }

        let auth = match self.auth_cached()? {
            Some(auth) => auth,
            None => {
                // Merge-safety anchor: after terminal saved-account eviction,
                // `auth()` must let AccountManager reload current store truth
                // instead of treating an empty cache as a permanent no-auth
                // owner.
                return self.current_auth_from_storage();
            }
        };
        if Self::is_stale_for_proactive_refresh(&auth)
            && let Err(err) = self.refresh_token().await
        {
            tracing::error!("Failed to refresh token: {}", err);
            let fallback_auth = self.auth_cached()?;
            if fallback_auth.is_none() {
                let attempted_store_account_id = auth
                    .active_chatgpt_account_summary()
                    .map(|summary| summary.store_account_id);
                let accounts = self.account_manager.list_accounts()?;
                let active_saved_account_count =
                    accounts.iter().filter(|account| account.is_active).count();
                let roster = self.account_manager.account_rate_limit_refresh_roster()?;
                tracing::warn!(
                    runtime_session_id = self.account_manager.runtime_session_id(),
                    linked_codex_session_id = ?self.account_manager.linked_codex_session_id().as_deref(),
                    attempted_store_account_id = ?attempted_store_account_id,
                    refresh_error = %err,
                    saved_account_count = accounts.len(),
                    active_saved_account_count,
                    roster_status = ?roster.status,
                    roster_store_account_count = roster.store_account_ids.len(),
                    "proactive refresh failure left auth() without any bearer"
                );
            }
            return Ok(fallback_auth);
        }
        self.auth_cached()
    }

    /// Force a reload of the auth information from auth.json. Returns
    /// whether the auth value changed.
    pub fn reload(&self) -> Result<bool, AccountRuntimeLoadError> {
        tracing::info!("Reloading auth");
        let loaded = self.load_store_from_storage()?;
        let auth = Self::derive_auth_from_store(
            &loaded.store,
            &self.codex_home,
            Arc::clone(&self.storage),
            self.enable_codex_api_key_env,
            loaded.store_origin,
        );
        Ok(self.set_cached_with_auth(loaded.store, auth, loaded.store_origin))
    }

    fn reload_if_store_account_id_matches(
        &self,
        expected_store_account_id: Option<&str>,
    ) -> Result<ReloadOutcome, AccountRuntimeLoadError> {
        let expected_store_account_id = match expected_store_account_id {
            Some(store_account_id) => store_account_id,
            None => {
                tracing::info!("Skipping auth reload because no saved account id is available.");
                return Ok(ReloadOutcome::Skipped);
            }
        };

        let Some(guarded_store) = self.account_manager.load_store_for_guarded_reload()? else {
            return Ok(ReloadOutcome::Skipped);
        };
        let GuardedReloadLoadedStore {
            loaded,
            removed_account_ids,
        } = guarded_store;
        let LoadedAuthStore {
            store,
            store_origin,
        } = loaded;

        let new_auth = Self::derive_auth_from_store(
            &store,
            &self.codex_home,
            Arc::clone(&self.storage),
            self.enable_codex_api_key_env,
            store_origin,
        );
        let new_store_account_id = new_auth
            .as_ref()
            .and_then(CodexAuth::active_chatgpt_account_summary)
            .map(|summary| summary.store_account_id);

        if new_store_account_id.as_deref() != Some(expected_store_account_id) {
            if removed_account_ids
                .iter()
                .any(|id| id == expected_store_account_id)
            {
                tracing::info!(
                    expected_store_account_id,
                    "Reloading auth after expected saved account was removed by supported-plan policy"
                );
                self.set_cached(store, store_origin);
                return Ok(ReloadOutcome::ReloadedChanged);
            }
            let found_store_account_id = new_store_account_id.as_deref().unwrap_or("unknown");
            tracing::info!(
                "Skipping auth reload due to saved account id mismatch (expected: {expected_store_account_id}, found: {found_store_account_id})"
            );
            return Ok(ReloadOutcome::Skipped);
        }

        tracing::info!("Reloading auth for saved account {expected_store_account_id}");
        let cached_before_reload = self.auth_cached()?;
        let auth_changed =
            !Self::auths_equal_for_refresh(cached_before_reload.as_ref(), new_auth.as_ref());
        self.set_cached(store, store_origin);
        if auth_changed {
            Ok(ReloadOutcome::ReloadedChanged)
        } else {
            Ok(ReloadOutcome::ReloadedNoChange)
        }
    }

    fn auths_equal_for_refresh(a: Option<&CodexAuth>, b: Option<&CodexAuth>) -> bool {
        match (a, b) {
            (None, None) => true,
            (Some(a), Some(b)) => match (a.api_auth_mode(), b.api_auth_mode()) {
                (ApiAuthMode::ApiKey, ApiAuthMode::ApiKey) => a.api_key() == b.api_key(),
                (ApiAuthMode::Chatgpt, ApiAuthMode::Chatgpt)
                | (ApiAuthMode::ChatgptAuthTokens, ApiAuthMode::ChatgptAuthTokens) => {
                    match (
                        a.current_chatgpt_account_snapshot(),
                        b.current_chatgpt_account_snapshot(),
                    ) {
                        (Some(a), Some(b)) => a.matches_refresh_snapshot(b),
                        (None, None) => true,
                        _ => false,
                    }
                }
                _ => false,
            },
            _ => false,
        }
    }

    fn auths_equal(a: Option<&CodexAuth>, b: Option<&CodexAuth>) -> bool {
        match (a, b) {
            (None, None) => true,
            (Some(a), Some(b)) => a == b,
            _ => false,
        }
    }

    fn derive_auth_from_store(
        store: &AuthStore,
        codex_home: &Path,
        storage: Arc<dyn AuthStorageBackend>,
        enable_codex_api_key_env: bool,
        store_origin: LoadedStoreOrigin,
    ) -> Option<CodexAuth> {
        if enable_codex_api_key_env && let Some(api_key) = read_codex_api_key_from_env() {
            return Some(CodexAuth::from_api_key(&api_key));
        }

        if let Some(active_account) = store.active_account() {
            return Some(
                CodexAuth::from_chatgpt_active_account_snapshot(
                    ActiveChatgptAccountSnapshot::from_stored_account(
                        active_account,
                        Self::chatgpt_api_auth_mode_for_store_origin(store_origin),
                    ),
                    Some(Self::storage_for_store_origin(
                        codex_home,
                        &storage,
                        store_origin,
                    )),
                )
                .unwrap_or_else(|error| match store_origin {
                    LoadedStoreOrigin::Persistent => {
                        panic!("persisted ChatGPT auth should always have a backing store: {error}")
                    }
                    LoadedStoreOrigin::ExternalEphemeral => {
                        panic!(
                            "external ephemeral ChatGPT token auth should always have a backing store: {error}"
                        )
                    }
                }),
            );
        }

        if !store.accounts.is_empty() {
            return None;
        }

        if let Some(api_key) = store.openai_api_key.as_deref() {
            return Some(CodexAuth::from_api_key(api_key));
        }

        None
    }

    fn chatgpt_api_auth_mode_for_store_origin(store_origin: LoadedStoreOrigin) -> ApiAuthMode {
        match store_origin {
            LoadedStoreOrigin::Persistent => ApiAuthMode::Chatgpt,
            LoadedStoreOrigin::ExternalEphemeral => ApiAuthMode::ChatgptAuthTokens,
        }
    }

    fn storage_for_store_origin(
        codex_home: &Path,
        persistent_storage: &Arc<dyn AuthStorageBackend>,
        store_origin: LoadedStoreOrigin,
    ) -> Arc<dyn AuthStorageBackend> {
        match store_origin {
            LoadedStoreOrigin::Persistent => Arc::clone(persistent_storage),
            LoadedStoreOrigin::ExternalEphemeral => create_auth_storage(
                codex_home.to_path_buf(),
                AuthCredentialsStoreMode::Ephemeral,
            ),
        }
    }

    fn set_cached_with_auth(
        &self,
        store: AuthStore,
        new_auth: Option<CodexAuth>,
        store_origin: LoadedStoreOrigin,
    ) -> bool {
        self.set_cached_with_auth_notification_mode(
            store,
            new_auth,
            store_origin,
            AuthStateNotificationMode::Notify,
        )
    }

    fn set_cached_with_auth_silent(
        &self,
        store: AuthStore,
        new_auth: Option<CodexAuth>,
        store_origin: LoadedStoreOrigin,
    ) -> bool {
        self.set_cached_with_auth_notification_mode(
            store,
            new_auth,
            store_origin,
            AuthStateNotificationMode::Silent,
        )
    }

    fn set_cached_with_auth_notification_mode(
        &self,
        store: AuthStore,
        new_auth: Option<CodexAuth>,
        store_origin: LoadedStoreOrigin,
        notification_mode: AuthStateNotificationMode,
    ) -> bool {
        if let Ok(mut guard) = self.inner.write() {
            let previous = guard.auth.as_ref();
            let changed = !AuthManager::auths_equal(previous, new_auth.as_ref());
            let auth_changed_for_refresh =
                !Self::auths_equal_for_refresh(previous, new_auth.as_ref());
            if auth_changed_for_refresh {
                guard.permanent_refresh_failure = None;
            }
            tracing::info!("Reloaded auth, changed: {changed}");
            guard.store = store;
            guard.store_origin = store_origin;
            guard.auth = new_auth;
            if notification_mode == AuthStateNotificationMode::Notify && changed {
                self.auth_state_tx.send_replace(());
            }
            changed
        } else {
            false
        }
    }

    fn load_store_from_storage(&self) -> Result<LoadedAuthStore, AccountRuntimeLoadError> {
        // Merge-safety anchor: live auth readers must delegate store snapshot
        // loading to AccountManager so runtime-active/usage truth comes from one
        // owner while AuthManager stays on auth derivation and cache refresh.
        self.account_manager.load_store_from_storage()
    }

    /// Records a permanent refresh failure only if the failed refresh was
    /// attempted against the auth snapshot that is still cached.
    fn record_permanent_refresh_failure_if_unchanged(
        &self,
        attempted_auth: &CodexAuth,
        error: &RefreshTokenFailedError,
    ) {
        if let Ok(mut guard) = self.inner.write() {
            let current_auth_matches =
                Self::auths_equal_for_refresh(Some(attempted_auth), guard.auth.as_ref());
            if current_auth_matches {
                guard.permanent_refresh_failure = Some(AuthScopedRefreshFailure {
                    auth: attempted_auth.clone(),
                    error: error.clone(),
                });
            }
        }
    }

    fn set_cached(&self, store: AuthStore, store_origin: LoadedStoreOrigin) -> bool {
        let new_auth = Self::derive_auth_from_store(
            &store,
            &self.codex_home,
            Arc::clone(&self.storage),
            self.enable_codex_api_key_env,
            store_origin,
        );
        self.set_cached_with_auth(store, new_auth, store_origin)
    }

    fn derive_chatgpt_auth_from_store_account(
        store: &AuthStore,
        store_account_id: &str,
        storage: Arc<dyn AuthStorageBackend>,
        store_origin: LoadedStoreOrigin,
    ) -> Option<CodexAuth> {
        let account = store.account(store_account_id)?;
        Some(
            CodexAuth::from_chatgpt_active_account_snapshot(
                ActiveChatgptAccountSnapshot::from_stored_account(
                    account,
                    Self::chatgpt_api_auth_mode_for_store_origin(store_origin),
                ),
                Some(storage),
            )
            .unwrap_or_else(|error| match store_origin {
                LoadedStoreOrigin::Persistent => {
                    panic!("stored ChatGPT account lookup should always have a backing store: {error}")
                }
                LoadedStoreOrigin::ExternalEphemeral => {
                    panic!(
                        "external ephemeral ChatGPT account lookup should always have a backing store: {error}"
                    )
                }
            }),
        )
    }

    pub fn set_external_auth(&self, external_auth: Arc<dyn ExternalAuth>) {
        if let Ok(mut guard) = self.external_auth.write() {
            *guard = Some(external_auth);
            self.auth_state_tx.send_replace(());
        }
    }

    pub fn clear_external_auth(&self) {
        if let Ok(mut guard) = self.external_auth.write() {
            *guard = None;
            self.auth_state_tx.send_replace(());
        }
    }

    pub fn set_forced_chatgpt_workspace_id(&self, workspace_id: Option<String>) {
        if self
            .account_manager
            .set_forced_chatgpt_workspace_id(workspace_id)
        {
            self.auth_state_tx.send_replace(());
        }
    }

    pub fn account_manager(&self) -> &AccountManager {
        &self.account_manager
    }

    pub fn set_linked_codex_session_id(
        &self,
        codex_session_id: Option<String>,
    ) -> std::io::Result<bool> {
        let changed = self
            .account_manager
            .set_linked_codex_session_id(codex_session_id)?;
        if !changed {
            return Ok(false);
        }
        if self.account_manager.has_account_state_store() {
            let _ = self
                .reload()
                .map_err(AccountRuntimeLoadError::into_io_error)?;
        }
        Ok(true)
    }

    pub fn linked_codex_session_id(&self) -> Option<String> {
        self.account_manager.linked_codex_session_id()
    }

    pub fn forced_chatgpt_workspace_id(&self) -> Option<String> {
        self.account_manager.forced_chatgpt_workspace_id()
    }

    pub fn subscribe_auth_state(&self) -> watch::Receiver<()> {
        self.auth_state_tx.subscribe()
    }

    pub fn has_external_auth(&self) -> bool {
        self.external_auth().is_some()
    }

    pub fn is_external_chatgpt_auth_active(&self) -> Result<bool, AccountRuntimeLoadError> {
        Ok(self
            .auth_cached()?
            .as_ref()
            .is_some_and(CodexAuth::is_external_chatgpt_tokens))
    }

    pub fn codex_api_key_env_enabled(&self) -> bool {
        self.enable_codex_api_key_env
    }

    /// Convenience constructor returning an `Arc` wrapper.
    pub fn shared(
        codex_home: PathBuf,
        enable_codex_api_key_env: bool,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
    ) -> Result<Arc<Self>, AccountRuntimeLoadError> {
        Self::shared_with_sqlite_home(
            codex_home.clone(),
            codex_home,
            enable_codex_api_key_env,
            auth_credentials_store_mode,
        )
    }

    pub fn shared_with_sqlite_home(
        codex_home: PathBuf,
        sqlite_home: PathBuf,
        enable_codex_api_key_env: bool,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
    ) -> Result<Arc<Self>, AccountRuntimeLoadError> {
        Ok(Arc::new(Self::new_with_sqlite_home(
            codex_home,
            sqlite_home,
            enable_codex_api_key_env,
            auth_credentials_store_mode,
        )?))
    }

    /// Convenience constructor returning an `Arc` wrapper from explicit
    /// pre-Config auth runtime values, including a forced ChatGPT workspace.
    pub fn shared_with_sqlite_home_and_forced_workspace(
        codex_home: PathBuf,
        sqlite_home: PathBuf,
        forced_chatgpt_workspace_id: Option<String>,
        enable_codex_api_key_env: bool,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
    ) -> Result<Arc<Self>, AccountRuntimeLoadError> {
        Ok(Arc::new(Self::new_with_sqlite_home_and_forced_workspace(
            codex_home,
            sqlite_home,
            forced_chatgpt_workspace_id,
            enable_codex_api_key_env,
            auth_credentials_store_mode,
        )?))
    }

    pub fn shared_with_sqlite_home_workspace_and_linked_session(
        codex_home: PathBuf,
        sqlite_home: PathBuf,
        forced_chatgpt_workspace_id: Option<String>,
        linked_codex_session_id: Option<String>,
        enable_codex_api_key_env: bool,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
    ) -> Result<Arc<Self>, AccountRuntimeLoadError> {
        Ok(Arc::new(
            Self::new_with_sqlite_home_workspace_and_linked_session(
                codex_home,
                sqlite_home,
                forced_chatgpt_workspace_id,
                linked_codex_session_id,
                enable_codex_api_key_env,
                auth_credentials_store_mode,
            )?,
        ))
    }

    /// Convenience constructor returning an `Arc` wrapper from resolved config.
    pub fn shared_from_config(
        config: &impl AuthManagerConfig,
        enable_codex_api_key_env: bool,
    ) -> Result<Arc<Self>, AccountRuntimeLoadError> {
        Self::shared_with_sqlite_home_and_forced_workspace(
            config.codex_home(),
            config.sqlite_home(),
            config.forced_chatgpt_workspace_id(),
            enable_codex_api_key_env,
            config.cli_auth_credentials_store_mode(),
        )
    }

    pub fn unauthorized_recovery(self: &Arc<Self>) -> UnauthorizedRecovery {
        UnauthorizedRecovery::new(Arc::clone(self))
    }

    fn external_auth(&self) -> Option<Arc<dyn ExternalAuth>> {
        self.external_auth
            .read()
            .ok()
            .and_then(|guard| guard.as_ref().cloned())
    }

    fn external_auth_mode(&self) -> Option<AuthMode> {
        self.external_auth()
            .as_ref()
            .map(|external_auth| external_auth.auth_mode())
    }

    fn has_external_api_key_auth(&self) -> bool {
        self.external_auth_mode() == Some(AuthMode::ApiKey)
    }

    async fn resolve_external_api_key_auth(&self) -> Option<CodexAuth> {
        if !self.has_external_api_key_auth() {
            return None;
        }

        let external_auth = self.external_auth()?;

        match external_auth.resolve().await {
            Ok(Some(tokens)) => Some(CodexAuth::from_api_key(&tokens.access_token)),
            Ok(None) => None,
            Err(err) => {
                tracing::error!("Failed to resolve external API key auth: {err}");
                None
            }
        }
    }

    /// Attempt to refresh the token by first performing a guarded reload. Auth
    /// is reloaded from storage only when the account id matches the currently
    /// cached account id. If the persisted token differs from the cached token, we
    /// can assume that some other instance already refreshed it. If the persisted
    /// token is the same as the cached, then ask the token authority to refresh.
    pub async fn refresh_token(&self) -> Result<(), RefreshTokenError> {
        let _refresh_guard = self.refresh_lock.lock().await;
        let auth_before_reload = self
            .auth_cached()
            .map_err(|error| RefreshTokenError::Transient(error.into_io_error()))?;
        if auth_before_reload
            .as_ref()
            .is_some_and(CodexAuth::is_api_key_auth)
        {
            return Ok(());
        }
        let expected_store_account_id = auth_before_reload
            .as_ref()
            .and_then(CodexAuth::active_chatgpt_account_summary)
            .map(|summary| summary.store_account_id);

        match self
            .reload_if_store_account_id_matches(expected_store_account_id.as_deref())
            .map_err(|error| RefreshTokenError::Transient(error.into_io_error()))?
        {
            ReloadOutcome::ReloadedChanged => {
                tracing::info!("Skipping token refresh because auth changed after guarded reload.");
                Ok(())
            }
            ReloadOutcome::ReloadedNoChange => self.refresh_token_from_authority_impl().await,
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
        let _refresh_guard = self.refresh_lock.lock().await;
        self.refresh_token_from_authority_impl().await
    }

    async fn refresh_token_from_authority_impl(&self) -> Result<(), RefreshTokenError> {
        tracing::info!("Refreshing token");

        let auth = match self
            .auth_cached()
            .map_err(|error| RefreshTokenError::Transient(error.into_io_error()))?
        {
            Some(auth) => auth,
            None => return Ok(()),
        };

        let attempted_auth = auth.clone();
        let result = match auth {
            CodexAuth::ChatgptAuthTokens(_) => {
                self.refresh_external_auth(ExternalAuthRefreshReason::Unauthorized)
                    .await
            }
            CodexAuth::Chatgpt(chatgpt_auth) => {
                match self
                    .resolve_chatgpt_auth_for_store_account_id_unlocked(
                        chatgpt_auth.store_account_id(),
                        ChatgptAccountRefreshMode::Force,
                    )
                    .await?
                {
                    ChatgptAccountAuthResolution::Auth(_) => Ok(()),
                    ChatgptAccountAuthResolution::Removed {
                        error,
                        switched_to_store_account_id,
                    } => {
                        let auth_after_removal = self
                            .auth_cached()
                            .map_err(|error| RefreshTokenError::Transient(error.into_io_error()))?;
                        let active_store_account_id_after_removal = auth_after_removal
                            .as_ref()
                            .and_then(CodexAuth::active_chatgpt_account_summary)
                            .map(|summary| summary.store_account_id);
                        if let Some(switched_to_store_account_id) =
                            switched_to_store_account_id.as_deref()
                            && Some(switched_to_store_account_id)
                                == active_store_account_id_after_removal.as_deref()
                        {
                            tracing::info!(
                                removed_store_account_id = chatgpt_auth.store_account_id(),
                                switched_to_store_account_id,
                                "removed active ChatGPT account after terminal refresh-token failure and switched to eligible ChatGPT fallback"
                            );
                            Ok(())
                        } else {
                            Err(RefreshTokenError::Permanent(error))
                        }
                    }
                    ChatgptAccountAuthResolution::Missing => {
                        Err(RefreshTokenError::Permanent(RefreshTokenFailedError::new(
                            RefreshTokenFailedReason::Other,
                            REFRESH_TOKEN_ACCOUNT_MISMATCH_MESSAGE.to_string(),
                        )))
                    }
                }
            }
            CodexAuth::ApiKey(_) => Ok(()),
        };
        if let Err(RefreshTokenError::Permanent(error)) = &result {
            self.record_permanent_refresh_failure_if_unchanged(&attempted_auth, error);
        }
        result
    }

    fn remove_chatgpt_store_account_for_terminal_refresh_failure(
        &self,
        store_account_id: &str,
        store_origin: LoadedStoreOrigin,
        error: &RefreshTokenFailedError,
    ) -> Result<TerminalRefreshFailureAccountRemoval, RefreshTokenError> {
        if !matches!(
            error.reason,
            RefreshTokenFailedReason::Expired
                | RefreshTokenFailedReason::Exhausted
                | RefreshTokenFailedReason::Revoked
        ) {
            return Ok(TerminalRefreshFailureAccountRemoval::NotRemoved);
        }

        let Some((mutation, loaded)) = self
            .account_manager
            .remove_store_account_after_terminal_refresh_failure_from_store_origin(
                store_account_id,
                store_origin,
            )
            .map_err(RefreshTokenError::Transient)?
        else {
            return Ok(TerminalRefreshFailureAccountRemoval::NotRemoved);
        };
        let LoadedAuthStore {
            store,
            store_origin,
        } = loaded;

        // Merge-safety anchor: terminal ChatGPT-account eviction mutates the
        // AccountManager-owned store, but AuthManager's generic cache must still
        // be re-derived by the normal auth owner so API-key runtimes are not
        // overwritten by ChatGPT-only fallback snapshots.
        let storage = Self::storage_for_store_origin(&self.codex_home, &self.storage, store_origin);
        let cached_auth = Self::derive_auth_from_store(
            &store,
            &self.codex_home,
            storage,
            self.enable_codex_api_key_env,
            store_origin,
        );
        self.set_cached_with_auth(store, cached_auth, store_origin);
        tracing::warn!(
            store_account_id,
            failed_reason = ?error.reason,
            switched_to_store_account_id = mutation.switched_to_store_account_id,
            "removed saved ChatGPT account after terminal refresh-token failure"
        );
        Ok(TerminalRefreshFailureAccountRemoval::Removed {
            switched_to_store_account_id: mutation.switched_to_store_account_id,
        })
    }

    /// Log out by deleting the on‑disk auth.json (if present). Returns Ok(true)
    /// if a file was removed, Ok(false) if no auth file existed. On success,
    /// reloads the in‑memory auth cache so callers immediately observe the
    /// unauthenticated state.
    pub fn logout(&self) -> std::io::Result<bool> {
        let removed = logout_all_stores(&self.codex_home, self.auth_credentials_store_mode)?;
        self.finish_logout(removed)
    }

    pub async fn logout_with_revoke(&self) -> std::io::Result<bool> {
        let auth_dot_json = self
            .auth_cached()
            .map_err(AccountRuntimeLoadError::into_io_error)?
            .and_then(|auth| auth.get_current_auth_json());
        if let Err(err) = revoke_auth_tokens(auth_dot_json.as_ref()).await {
            tracing::warn!("failed to revoke auth tokens during logout: {err}");
        }
        let result = logout_all_stores(&self.codex_home, self.auth_credentials_store_mode)?;
        self.finish_logout(result)
    }

    pub fn get_api_auth_mode(&self) -> Result<Option<ApiAuthMode>, AccountRuntimeLoadError> {
        if self.has_external_api_key_auth() {
            return Ok(Some(ApiAuthMode::ApiKey));
        }
        Ok(self
            .current_auth_from_storage()?
            .as_ref()
            .map(CodexAuth::api_auth_mode))
    }

    pub fn current_auth_from_storage(&self) -> Result<Option<CodexAuth>, AccountRuntimeLoadError> {
        let (loaded, auth) = self.load_auth_from_storage_for_live_reader()?;
        self.set_cached_with_auth_silent(loaded.store, auth.clone(), loaded.store_origin);
        Ok(auth)
    }

    fn load_auth_from_storage_for_live_reader(
        &self,
    ) -> Result<(LoadedAuthStore, Option<CodexAuth>), AccountRuntimeLoadError> {
        let loaded = self.load_store_from_storage()?;
        let auth = Self::derive_auth_from_store(
            &loaded.store,
            &self.codex_home,
            Arc::clone(&self.storage),
            self.enable_codex_api_key_env,
            loaded.store_origin,
        );
        Ok((loaded, auth))
    }

    pub fn runtime_session_id(&self) -> &str {
        self.account_manager.runtime_session_id()
    }

    fn finish_logout(&self, removed: bool) -> std::io::Result<bool> {
        // Merge-safety anchor: WS12 logout must clear the runtime active-account lease before the
        // live manager stays resident, or app-server/TUI logout leaves a foreign SQLite lease that
        // blocks the next session from claiming the saved account.
        let release_result = self.account_manager.release_runtime_active_account();
        // Always reload to clear any cached auth (even if file absent).
        self.reload()
            .map_err(AccountRuntimeLoadError::into_io_error)?;
        release_result?;
        Ok(removed)
    }

    pub fn auth_mode(&self) -> Result<Option<AuthMode>, AccountRuntimeLoadError> {
        if self.has_external_api_key_auth() {
            return Ok(Some(AuthMode::ApiKey));
        }
        self.get_internal_auth_mode()
    }

    fn get_internal_auth_mode(&self) -> Result<Option<AuthMode>, AccountRuntimeLoadError> {
        Ok(self
            .auth_cached()?
            .as_ref()
            .map(CodexAuth::internal_auth_mode))
    }

    fn is_stale_for_proactive_refresh(auth: &CodexAuth) -> bool {
        let chatgpt_auth = match auth {
            CodexAuth::Chatgpt(chatgpt_auth) => chatgpt_auth,
            _ => return false,
        };

        let active_account = chatgpt_auth.current_chatgpt_account_snapshot();
        if let Ok(Some(expires_at)) = parse_jwt_expiration(&active_account.tokens.access_token) {
            return expires_at <= Utc::now();
        }
        let last_refresh = match active_account.last_refresh {
            Some(last_refresh) => last_refresh,
            None => return false,
        };
        last_refresh < Utc::now() - chrono::Duration::days(TOKEN_REFRESH_INTERVAL)
    }

    fn map_external_auth_refresh_error(error: std::io::Error) -> RefreshTokenError {
        if let Some(failed) = error
            .get_ref()
            .and_then(|source| source.downcast_ref::<RefreshTokenFailedError>())
        {
            return RefreshTokenError::Permanent(failed.clone());
        }
        RefreshTokenError::Transient(error)
    }

    async fn refresh_external_auth(
        &self,
        reason: ExternalAuthRefreshReason,
    ) -> Result<(), RefreshTokenError> {
        let Some(external_auth) = self.external_auth() else {
            return Err(RefreshTokenError::Transient(std::io::Error::other(
                "external auth is not configured",
            )));
        };
        let forced_chatgpt_workspace_id = self.account_manager.forced_chatgpt_workspace_id();
        let (previous_account_id, active_store_account_id, active_store_origin) =
            if let Ok(guard) = self.inner.read() {
                (
                    guard.auth.as_ref().and_then(CodexAuth::get_account_id),
                    guard
                        .auth
                        .as_ref()
                        .and_then(CodexAuth::active_chatgpt_account_summary)
                        .map(|summary| summary.store_account_id),
                    guard.store_origin,
                )
            } else {
                (None, None, self.account_manager.configured_store_origin())
            };
        let context = ExternalAuthRefreshContext {
            reason,
            previous_account_id,
        };

        let refreshed = match external_auth.refresh(context).await {
            Ok(refreshed) => refreshed,
            Err(error) => {
                return self.finish_external_auth_refresh_failure(
                    active_store_account_id.as_deref(),
                    active_store_origin,
                    Self::map_external_auth_refresh_error(error),
                );
            }
        };
        if external_auth.auth_mode() == AuthMode::ApiKey {
            return Ok(());
        }
        let Some(chatgpt_metadata) = refreshed.chatgpt_metadata() else {
            return Err(RefreshTokenError::Transient(std::io::Error::other(
                "external auth refresh did not return ChatGPT metadata",
            )));
        };
        if let Some(expected_workspace_id) = forced_chatgpt_workspace_id.as_deref()
            && chatgpt_metadata.account_id != expected_workspace_id
        {
            return Err(RefreshTokenError::Transient(std::io::Error::other(
                format!(
                    "external auth refresh returned workspace {:?}, expected {expected_workspace_id:?}",
                    chatgpt_metadata.account_id,
                ),
            )));
        }
        let mut auth_dot_json =
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
        if let Some(previous_auth) = self
            .auth_cached()
            .map_err(|error| RefreshTokenError::Transient(error.into_io_error()))?
            .and_then(|auth| auth.get_current_auth_json())
        {
            auth_dot_json.agent_identity = previous_auth.agent_identity;
        }
        let refreshed_store_account_id = auth_dot_json
            .tokens
            .as_ref()
            .and_then(TokenData::preferred_store_account_id);
        let mut store = AuthStore::from_legacy(auth_dot_json);
        let removed_account_ids =
            enforce_chatgpt_auth_accounts(&mut store, ChatgptAuthAdmissionPolicy::ExternalStrict);
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
        self.reload()
            .map_err(|error| RefreshTokenError::Transient(error.into_io_error()))?;
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

    fn finish_external_auth_refresh_failure(
        &self,
        active_store_account_id: Option<&str>,
        active_store_origin: LoadedStoreOrigin,
        error: RefreshTokenError,
    ) -> Result<(), RefreshTokenError> {
        let RefreshTokenError::Permanent(error) = error else {
            return Err(error);
        };
        let Some(active_store_account_id) = active_store_account_id else {
            return Err(RefreshTokenError::Permanent(error));
        };
        match self.remove_chatgpt_store_account_for_terminal_refresh_failure(
            active_store_account_id,
            active_store_origin,
            &error,
        )? {
            TerminalRefreshFailureAccountRemoval::Removed {
                switched_to_store_account_id,
            } => {
                let active_store_account_id_after_removal = self
                    .auth_cached()
                    .map_err(|error| RefreshTokenError::Transient(error.into_io_error()))?
                    .as_ref()
                    .and_then(CodexAuth::active_chatgpt_account_summary)
                    .map(|summary| summary.store_account_id);
                if let Some(switched_to_store_account_id) = switched_to_store_account_id.as_deref()
                    && Some(switched_to_store_account_id)
                        == active_store_account_id_after_removal.as_deref()
                {
                    tracing::info!(
                        removed_store_account_id = active_store_account_id,
                        switched_to_store_account_id,
                        "removed active external ChatGPT account after terminal refresh-token failure and switched to eligible ChatGPT fallback"
                    );
                    Ok(())
                } else {
                    Err(RefreshTokenError::Permanent(error))
                }
            }
            TerminalRefreshFailureAccountRemoval::NotRemoved => {
                Err(RefreshTokenError::Permanent(error))
            }
        }
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
            auth.store_account_id(),
            refresh_id_token,
            refresh_access_token,
            refresh_refresh_token,
            PersistedActiveAccountWriteMode::Strip,
        )
        .await
        .map_err(RefreshTokenError::from)?;
        if !updated_store
            .accounts
            .iter()
            .any(|account| account.id == auth.store_account_id())
        {
            self.reload()
                .map_err(|error| RefreshTokenError::Transient(error.into_io_error()))?;
            return Err(RefreshTokenError::Permanent(RefreshTokenFailedError::new(
                RefreshTokenFailedReason::Other,
                UNSUPPORTED_CHATGPT_PLAN_REMOVED_MESSAGE.to_string(),
            )));
        }

        Ok(refresh_response)
    }
}

fn store_from_auth_for_testing(auth: &CodexAuth) -> AuthStore {
    match auth {
        CodexAuth::ApiKey(api_key) => AuthStore {
            openai_api_key: Some(api_key.api_key.clone()),
            ..AuthStore::default()
        },
        CodexAuth::Chatgpt(_) | CodexAuth::ChatgptAuthTokens(_) => {
            let active_account = match auth.current_chatgpt_account_snapshot() {
                Some(active_account) => active_account,
                None => return AuthStore::default(),
            };
            AuthStore {
                active_account_id: Some(active_account.store_account_id.clone()),
                accounts: vec![StoredAccount {
                    id: active_account.store_account_id.clone(),
                    label: active_account.label.clone(),
                    tokens: active_account.tokens.clone(),
                    last_refresh: active_account.last_refresh,
                    usage: None,
                }],
                ..AuthStore::default()
            }
        }
    }
}

#[cfg(test)]
#[path = "auth_tests.rs"]
mod tests;
