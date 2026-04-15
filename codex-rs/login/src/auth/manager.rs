use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use reqwest::StatusCode;
use serde::Deserialize;
use serde::Serialize;
#[cfg(test)]
use serial_test::serial;
use std::collections::HashMap;
use std::collections::HashSet;
use std::env;
use std::fmt::Debug;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::RwLock;
use tokio::sync::Mutex as AsyncMutex;

use codex_account_state::AccountLeaseConflict;
use codex_account_state::AccountStateStore;
use codex_account_state::AccountUsageState;
use codex_account_state::SessionActiveAccountRefresh;
use codex_account_state::SessionActiveAccountSetOutcome;
use codex_app_server_protocol::AuthMode;
use codex_app_server_protocol::AuthMode as ApiAuthMode;
use codex_protocol::config_types::ForcedLoginMethod;
use codex_protocol::config_types::ModelProviderAuthInfo;

use super::external_bearer::BearerTokenRefresher;
pub use crate::auth::storage::AccountUsageCache;
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
use codex_protocol::protocol::RateLimitSnapshot;
use codex_protocol::protocol::RateLimitWindow;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveChatgptAccountSummary {
    pub store_account_id: String,
    pub label: Option<String>,
    pub email: Option<String>,
    pub auth_mode: AuthMode,
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
const USAGE_LIMIT_AUTO_SWITCH_COOLDOWN_SECONDS: i64 = 2;
const ACTIVE_ACCOUNT_LEASE_TTL_SECONDS: i64 = 5 * 60;

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
    Auth(Box<CodexAuth>),
    /// The stored account was removed because refresh-token failure is terminal.
    Removed {
        error: RefreshTokenFailedError,
        switched_to_store_account_id: Option<String>,
    },
    /// The requested stored account was already absent.
    Missing,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AccountRateLimitRefreshOutcome {
    Snapshot(RateLimitSnapshot),
    NoUsableSnapshot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageLimitAutoSwitchSelectionScope<'a> {
    PersistedTruth,
    FreshlySelectable(&'a HashSet<String>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageLimitAutoSwitchFallbackSelectionMode {
    AllowFallbackSelection,
    CancelStaleRequestFallbackSelection,
}

pub struct UsageLimitAutoSwitchRequest<'a> {
    pub required_workspace_id: Option<&'a str>,
    pub failing_store_account_id: Option<&'a str>,
    pub resets_at: Option<DateTime<Utc>>,
    pub snapshot: Option<RateLimitSnapshot>,
    pub freshly_unsupported_store_account_ids: &'a HashSet<String>,
    pub protected_store_account_id: Option<&'a str>,
    pub selection_scope: UsageLimitAutoSwitchSelectionScope<'a>,
    pub fallback_selection_mode: UsageLimitAutoSwitchFallbackSelectionMode,
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

fn apply_rate_limit_refresh_outcome(
    usage: &mut AccountUsageCache,
    outcome: AccountRateLimitRefreshOutcome,
    now: DateTime<Utc>,
) {
    match outcome {
        AccountRateLimitRefreshOutcome::Snapshot(snapshot) => {
            let exhausted_until = exhausted_until_from_snapshot(&snapshot, now);
            usage.last_rate_limits = Some(snapshot);
            usage.exhausted_until = exhausted_until;
        }
        AccountRateLimitRefreshOutcome::NoUsableSnapshot => {
            usage.last_rate_limits = None;
            usage.exhausted_until = None;
        }
    }
    usage.last_seen_at = Some(now);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UsageLimitWindowSlot {
    Primary,
    Secondary,
}

fn clamp_usage_limit_snapshot(
    mut snapshot: RateLimitSnapshot,
    resets_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> RateLimitSnapshot {
    if clamp_matched_usage_limit_window(&mut snapshot, resets_at) {
        return snapshot;
    }

    if clamp_depleted_usage_limit_windows(&mut snapshot, now) {
        return snapshot;
    }

    match usage_limit_default_window_slot(&snapshot) {
        Some(UsageLimitWindowSlot::Primary) => clamp_rate_limit_window(snapshot.primary.as_mut()),
        Some(UsageLimitWindowSlot::Secondary) => {
            clamp_rate_limit_window(snapshot.secondary.as_mut());
        }
        None => {}
    }

    snapshot
}

fn clamp_matched_usage_limit_window(
    snapshot: &mut RateLimitSnapshot,
    resets_at: Option<DateTime<Utc>>,
) -> bool {
    let Some(resets_at) = resets_at else {
        return false;
    };

    let resets_at = resets_at.timestamp();
    let primary_matches = snapshot
        .primary
        .as_ref()
        .is_some_and(|window| window.resets_at == Some(resets_at));
    let secondary_matches = snapshot
        .secondary
        .as_ref()
        .is_some_and(|window| window.resets_at == Some(resets_at));

    if primary_matches {
        clamp_rate_limit_window(snapshot.primary.as_mut());
    }
    if secondary_matches {
        clamp_rate_limit_window(snapshot.secondary.as_mut());
    }

    primary_matches || secondary_matches
}

fn clamp_depleted_usage_limit_windows(
    snapshot: &mut RateLimitSnapshot,
    now: DateTime<Utc>,
) -> bool {
    let mut clamped = false;
    if snapshot
        .primary
        .as_ref()
        .is_some_and(|window| window.is_depleted_at(now.timestamp()))
    {
        clamp_rate_limit_window(snapshot.primary.as_mut());
        clamped = true;
    }
    if snapshot
        .secondary
        .as_ref()
        .is_some_and(|window| window.is_depleted_at(now.timestamp()))
    {
        clamp_rate_limit_window(snapshot.secondary.as_mut());
        clamped = true;
    }
    clamped
}

fn usage_limit_default_window_slot(snapshot: &RateLimitSnapshot) -> Option<UsageLimitWindowSlot> {
    if snapshot.primary.is_some() {
        return Some(UsageLimitWindowSlot::Primary);
    }

    snapshot
        .secondary
        .as_ref()
        .map(|_| UsageLimitWindowSlot::Secondary)
}

fn clamp_rate_limit_window(window: Option<&mut RateLimitWindow>) {
    if let Some(window) = window {
        window.remaining_percent = 0.0;
    }
}

fn account_usage_state_from_cache(cache: &AccountUsageCache) -> AccountUsageState {
    AccountUsageState {
        last_rate_limits: cache.last_rate_limits.clone(),
        exhausted_until: cache.exhausted_until,
        last_seen_at: cache.last_seen_at,
    }
}

fn account_usage_cache_from_state(state: &AccountUsageState) -> AccountUsageCache {
    AccountUsageCache {
        last_rate_limits: state.last_rate_limits.clone(),
        exhausted_until: state.exhausted_until,
        last_seen_at: state.last_seen_at,
    }
}

fn legacy_usage_states_from_store(store: &AuthStore) -> HashMap<String, AccountUsageState> {
    store
        .accounts
        .iter()
        .filter_map(|account| {
            account
                .usage
                .as_ref()
                .map(|usage| (account.id.clone(), account_usage_state_from_cache(usage)))
        })
        .collect()
}

fn strip_usage_from_store(store: &mut AuthStore) {
    for account in &mut store.accounts {
        account.usage = None;
    }
}

fn strip_runtime_active_account_from_store(store: &mut AuthStore) {
    store.active_account_id = None;
}

fn persist_stripped_auth_store(
    storage: &dyn AuthStorageBackend,
    store: &AuthStore,
) -> std::io::Result<()> {
    let mut stripped_store = store.clone();
    strip_usage_from_store(&mut stripped_store);
    strip_runtime_active_account_from_store(&mut stripped_store);
    storage.save(&stripped_store)
}

fn persist_auth_store(
    storage: &dyn AuthStorageBackend,
    store: &AuthStore,
    strip_usage: bool,
) -> std::io::Result<()> {
    let mut persisted_store = store.clone();
    strip_runtime_active_account_from_store(&mut persisted_store);
    if strip_usage {
        strip_usage_from_store(&mut persisted_store);
    }
    storage.save(&persisted_store)
}

fn persist_usage_state_from_store(
    account_state_store: &AccountStateStore,
    store: &AuthStore,
) -> std::io::Result<()> {
    let usage_by_account = legacy_usage_states_from_store(store);
    account_state_store
        .replace_usage_states(&usage_by_account)
        .map_err(std::io::Error::other)
}

fn hydrate_store_usage_from_sqlite(
    account_state_store: &AccountStateStore,
    store: &mut AuthStore,
) -> std::io::Result<()> {
    let account_ids = store
        .accounts
        .iter()
        .map(|account| account.id.clone())
        .collect::<Vec<_>>();
    let usage_by_account = account_state_store
        .load_usage_states_for_accounts(account_ids.as_slice())
        .map_err(std::io::Error::other)?;
    for account in &mut store.accounts {
        account.usage = usage_by_account
            .get(&account.id)
            .map(account_usage_cache_from_state);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct UsageStateSyncOutcome {
    strip_persisted_auth_store: bool,
}

fn synchronize_store_usage_from_legacy_and_sqlite(
    account_state_store: &AccountStateStore,
    store: &mut AuthStore,
) -> std::io::Result<UsageStateSyncOutcome> {
    let legacy_usage_by_account = legacy_usage_states_from_store(store);
    let strip_persisted_auth_store = !legacy_usage_by_account.is_empty();
    if strip_persisted_auth_store {
        tracing::info!(
            legacy_usage_accounts = legacy_usage_by_account.len(),
            "skipping legacy auth-store usage backfill during sqlite usage-truth cutover"
        );
    }
    hydrate_store_usage_from_sqlite(account_state_store, store)?;
    Ok(UsageStateSyncOutcome {
        strip_persisted_auth_store,
    })
}

fn open_account_state_store(sqlite_home: &Path) -> Option<AccountStateStore> {
    match AccountStateStore::open(sqlite_home.to_path_buf()) {
        Ok(account_state_store) => Some(account_state_store),
        Err(error) => {
            tracing::warn!(
                error = %error,
                sqlite_home = %sqlite_home.display(),
                "failed to open account state store; preserving legacy auth-store usage truth"
            );
            None
        }
    }
}

fn lease_conflict_error(account_id: &str, conflict: &AccountLeaseConflict) -> std::io::Error {
    std::io::Error::other(format!(
        "account '{account_id}' is currently leased by another live session until {}",
        conflict.lease_until
    ))
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
                Ok(Self::ChatgptAuthTokens(ChatgptAuthTokens { state }))
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

    pub fn active_chatgpt_account_summary(&self) -> Option<ActiveChatgptAccountSummary> {
        self.current_chatgpt_account_snapshot()
            .map(ActiveChatgptAccountSnapshot::summary)
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
        if let Some(active_account) = store.active_account() {
            let auth = CodexAuth::from_chatgpt_active_account_snapshot(
                ActiveChatgptAccountSnapshot::from_stored_account(
                    active_account,
                    ApiAuthMode::ChatgptAuthTokens,
                ),
                None,
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
) -> PreflightAuthState {
    enforce_supported_chatgpt_auth_accounts(store);
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
        let preflight_state = preflight_state_from_store(&mut store, required_workspace_id);
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
    let removed_account_ids = enforce_supported_chatgpt_auth_accounts(&mut store);
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
    store_origin: CachedStoreOrigin,
    auth: Option<CodexAuth>,
    /// Permanent refresh failure cached for the current auth snapshot so
    /// later refresh attempts for the same credentials fail fast without network.
    permanent_refresh_failure: Option<AuthScopedRefreshFailure>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CachedStoreOrigin {
    Persistent,
    ExternalEphemeral,
}

#[derive(Clone)]
struct LoadedCachedStore {
    store: AuthStore,
    store_origin: CachedStoreOrigin,
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
        }
    }

    pub fn has_next(&self) -> bool {
        if self.manager.has_external_api_key_auth() {
            return !matches!(self.step, UnauthorizedRecoveryStep::Done);
        }

        if !self
            .manager
            .auth_cached()
            .as_ref()
            .is_some_and(CodexAuth::is_chatgpt_auth)
        {
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

        if !self
            .manager
            .auth_cached()
            .as_ref()
            .is_some_and(CodexAuth::is_chatgpt_auth)
        {
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
pub struct AuthManager {
    codex_home: PathBuf,
    storage: Arc<dyn AuthStorageBackend>,
    account_state_store: Option<AccountStateStore>,
    runtime_session_id: String,
    inner: RwLock<CachedAuth>,
    enable_codex_api_key_env: bool,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    forced_chatgpt_workspace_id: RwLock<Option<String>>,
    usage_limit_auto_switch_cooldown_until: Mutex<Option<DateTime<Utc>>>,
    _test_home_guard: Option<tempfile::TempDir>,
    refresh_lock: AsyncMutex<()>,
    external_auth: RwLock<Option<Arc<dyn ExternalAuth>>>,
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
            .field("account_state_store", &self.account_state_store)
            .field("runtime_session_id", &self.runtime_session_id)
            .field("inner", &self.inner)
            .field("enable_codex_api_key_env", &self.enable_codex_api_key_env)
            .field(
                "auth_credentials_store_mode",
                &self.auth_credentials_store_mode,
            )
            .field(
                "forced_chatgpt_workspace_id",
                &self.forced_chatgpt_workspace_id,
            )
            .field("has_external_auth", &self.has_external_auth())
            .finish_non_exhaustive()
    }
}

impl AuthManager {
    fn bootstrap_runtime_active_account_candidates(
        store: &AuthStore,
        required_workspace_id: Option<&str>,
    ) -> Vec<String> {
        let mut candidates = Vec::new();
        let mut seen = HashSet::new();
        if let Some(legacy_active_account_id) = store.active_account_id.as_deref()
            && store
                .account(legacy_active_account_id)
                .is_some_and(|account| {
                    account_matches_required_workspace(account, required_workspace_id)
                })
        {
            candidates.push(legacy_active_account_id.to_string());
            seen.insert(legacy_active_account_id.to_string());
        }
        for account in &store.accounts {
            if !account_matches_required_workspace(account, required_workspace_id) {
                continue;
            }
            if seen.insert(account.id.clone()) {
                candidates.push(account.id.clone());
            }
        }
        candidates
    }

    fn hydrate_runtime_active_account(
        account_state_store: Option<&AccountStateStore>,
        runtime_session_id: &str,
        required_workspace_id: Option<&str>,
        store: &mut AuthStore,
    ) -> std::io::Result<()> {
        if store.accounts.is_empty() {
            store.active_account_id = None;
            return Ok(());
        }
        let Some(account_state_store) = account_state_store else {
            store.active_account_id = None;
            return Ok(());
        };

        let now = Utc::now();
        let mut allow_bootstrap = true;
        let mut current_active_account_id = match account_state_store
            .refresh_session_active_account(
                runtime_session_id,
                now,
                ACTIVE_ACCOUNT_LEASE_TTL_SECONDS,
            ) {
            Ok(SessionActiveAccountRefresh::Active(account_id)) => Some(account_id),
            Ok(SessionActiveAccountRefresh::None) => None,
            Ok(SessionActiveAccountRefresh::LostToOtherSession {
                account_id,
                owner_session_id,
                lease_until,
            }) => {
                tracing::info!(
                    account_id,
                    owner_session_id,
                    lease_until = ?lease_until,
                    "runtime session lost its active-account lease to another live session"
                );
                allow_bootstrap = false;
                None
            }
            Err(error) => {
                return Err(std::io::Error::other(format!(
                    "failed to refresh runtime active-account lease: {error}"
                )));
            }
        };

        if let Some(active_account_id) = current_active_account_id.as_deref()
            && !store.account(active_account_id).is_some_and(|account| {
                account_matches_required_workspace(account, required_workspace_id)
            })
        {
            account_state_store
                .clear_session_active_account(runtime_session_id)
                .map_err(|error| {
                    std::io::Error::other(format!(
                        "failed to clear invalid runtime active-account row: {error}"
                    ))
                })?;
            current_active_account_id = None;
        }

        if current_active_account_id.is_none() && allow_bootstrap {
            for candidate_account_id in
                Self::bootstrap_runtime_active_account_candidates(store, required_workspace_id)
            {
                match account_state_store.set_session_active_account(
                    runtime_session_id,
                    candidate_account_id.as_str(),
                    now,
                    ACTIVE_ACCOUNT_LEASE_TTL_SECONDS,
                ) {
                    Ok(SessionActiveAccountSetOutcome::Assigned) => {
                        current_active_account_id = Some(candidate_account_id);
                        break;
                    }
                    Ok(SessionActiveAccountSetOutcome::Conflict(conflict)) => {
                        tracing::debug!(
                            candidate_account_id,
                            owner_session_id = conflict.owner_session_id,
                            lease_until = ?conflict.lease_until,
                            "skipping runtime active-account bootstrap candidate leased by another live session"
                        );
                    }
                    Err(error) => {
                        return Err(std::io::Error::other(format!(
                            "failed to bootstrap runtime active-account row: {error}"
                        )));
                    }
                }
            }
        }

        store.active_account_id = current_active_account_id;
        Ok(())
    }

    fn reconcile_runtime_active_account(&self, store: &AuthStore) -> std::io::Result<()> {
        let Some(account_state_store) = self.account_state_store.as_ref() else {
            if store.active_account_id.is_some() {
                return Err(std::io::Error::other(
                    "runtime active-account owner is unavailable",
                ));
            }
            return Ok(());
        };
        let now = Utc::now();
        match store.active_account_id.as_deref() {
            Some(active_account_id) => {
                if store.account(active_account_id).is_none() {
                    return Err(std::io::Error::other(format!(
                        "runtime active account '{active_account_id}' is missing from the auth store"
                    )));
                }
                match account_state_store.set_session_active_account(
                    self.runtime_session_id.as_str(),
                    active_account_id,
                    now,
                    ACTIVE_ACCOUNT_LEASE_TTL_SECONDS,
                ) {
                    Ok(SessionActiveAccountSetOutcome::Assigned) => Ok(()),
                    Ok(SessionActiveAccountSetOutcome::Conflict(conflict)) => {
                        Err(lease_conflict_error(active_account_id, &conflict))
                    }
                    Err(error) => Err(std::io::Error::other(format!(
                        "failed to persist runtime active-account state: {error}"
                    ))),
                }
            }
            None => account_state_store
                .clear_session_active_account(self.runtime_session_id.as_str())
                .map(|_| ())
                .map_err(|error| {
                    std::io::Error::other(format!(
                        "failed to clear runtime active-account state: {error}"
                    ))
                }),
        }
    }

    /// Create a new manager loading the initial auth using the provided
    /// preferred auth method. Errors loading auth or opening the WS12
    /// account-state store are swallowed; `auth()` will simply return `None`
    /// in that case so callers can treat it as an unauthenticated state, and
    /// saved-account usage truth falls back to the legacy auth-store cache
    /// until SQLite ownership becomes available again.
    pub fn new(
        codex_home: PathBuf,
        enable_codex_api_key_env: bool,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
    ) -> Self {
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
    ) -> Self {
        let storage = create_auth_storage(codex_home.clone(), auth_credentials_store_mode);
        let account_state_store = open_account_state_store(sqlite_home.as_path());
        let runtime_session_id = uuid::Uuid::new_v4().to_string();
        let loaded = Self::load_store_from_storage_impl(
            &codex_home,
            &storage,
            auth_credentials_store_mode,
            account_state_store.as_ref(),
            runtime_session_id.as_str(),
            None,
        );
        let store = loaded.store;
        let store_origin = loaded.store_origin;
        let auth = Self::derive_auth_from_store(
            &store,
            Arc::clone(&storage),
            enable_codex_api_key_env,
            store_origin,
        );
        Self {
            codex_home,
            storage,
            account_state_store,
            runtime_session_id,
            inner: RwLock::new(CachedAuth {
                store,
                store_origin,
                auth,
                permanent_refresh_failure: None,
            }),
            enable_codex_api_key_env,
            auth_credentials_store_mode,
            forced_chatgpt_workspace_id: RwLock::new(None),
            usage_limit_auto_switch_cooldown_until: Mutex::new(None),
            _test_home_guard: None,
            refresh_lock: AsyncMutex::new(()),
            external_auth: RwLock::new(None),
        }
    }

    /// Create an AuthManager with a specific CodexAuth, for testing only.
    pub fn from_auth_for_testing(auth: CodexAuth) -> Arc<Self> {
        let temp_dir = tempfile::tempdir().unwrap_or_else(|err| panic!("temp codex home: {err}"));
        let codex_home = temp_dir.path().to_path_buf();
        let storage = create_auth_storage(codex_home.clone(), AuthCredentialsStoreMode::File);
        let account_state_store = open_account_state_store(codex_home.as_path());
        let store = store_from_auth_for_testing(&auth);
        let cached = CachedAuth {
            store,
            store_origin: CachedStoreOrigin::Persistent,
            auth: Some(auth),
            permanent_refresh_failure: None,
        };

        Arc::new(Self {
            codex_home,
            storage,
            account_state_store,
            runtime_session_id: uuid::Uuid::new_v4().to_string(),
            inner: RwLock::new(cached),
            enable_codex_api_key_env: false,
            auth_credentials_store_mode: AuthCredentialsStoreMode::File,
            forced_chatgpt_workspace_id: RwLock::new(None),
            usage_limit_auto_switch_cooldown_until: Mutex::new(None),
            _test_home_guard: Some(temp_dir),
            refresh_lock: AsyncMutex::new(()),
            external_auth: RwLock::new(None),
        })
    }

    /// Create an AuthManager with a specific CodexAuth and codex home, for testing only.
    pub fn from_auth_for_testing_with_home(auth: CodexAuth, codex_home: PathBuf) -> Arc<Self> {
        let storage = create_auth_storage(codex_home.clone(), AuthCredentialsStoreMode::File);
        let account_state_store = open_account_state_store(codex_home.as_path());
        let store = store_from_auth_for_testing(&auth);
        let cached = CachedAuth {
            store,
            store_origin: CachedStoreOrigin::Persistent,
            auth: Some(auth),
            permanent_refresh_failure: None,
        };
        Arc::new(Self {
            codex_home,
            storage,
            account_state_store,
            runtime_session_id: uuid::Uuid::new_v4().to_string(),
            inner: RwLock::new(cached),
            enable_codex_api_key_env: false,
            auth_credentials_store_mode: AuthCredentialsStoreMode::File,
            forced_chatgpt_workspace_id: RwLock::new(None),
            usage_limit_auto_switch_cooldown_until: Mutex::new(None),
            _test_home_guard: None,
            refresh_lock: AsyncMutex::new(()),
            external_auth: RwLock::new(None),
        })
    }

    pub fn external_bearer_only(config: ModelProviderAuthInfo) -> Arc<Self> {
        let codex_home = PathBuf::from("non-existent");
        let storage = create_auth_storage(codex_home.clone(), AuthCredentialsStoreMode::File);
        Arc::new(Self {
            codex_home,
            storage,
            account_state_store: None,
            runtime_session_id: uuid::Uuid::new_v4().to_string(),
            inner: RwLock::new(CachedAuth {
                store: AuthStore::default(),
                store_origin: CachedStoreOrigin::Persistent,
                auth: None,
                permanent_refresh_failure: None,
            }),
            enable_codex_api_key_env: false,
            auth_credentials_store_mode: AuthCredentialsStoreMode::File,
            forced_chatgpt_workspace_id: RwLock::new(None),
            usage_limit_auto_switch_cooldown_until: Mutex::new(None),
            _test_home_guard: None,
            refresh_lock: AsyncMutex::new(()),
            external_auth: RwLock::new(Some(
                Arc::new(BearerTokenRefresher::new(config)) as Arc<dyn ExternalAuth>
            )),
        })
    }

    /// Current cached auth (clone) without attempting a refresh.
    pub fn auth_cached(&self) -> Option<CodexAuth> {
        let (mut store, store_origin) = {
            let guard = self.inner.read().ok()?;
            (guard.store.clone(), guard.store_origin)
        };
        if let Err(error) = Self::hydrate_runtime_active_account(
            self.account_state_store.as_ref(),
            self.runtime_session_id.as_str(),
            self.forced_chatgpt_workspace_id().as_deref(),
            &mut store,
        ) {
            tracing::warn!(
                error = %error,
                "failed to hydrate runtime active-account state while reading cached auth"
            );
            store.active_account_id = None;
        }
        let auth = Self::derive_auth_from_store(
            &store,
            Arc::clone(&self.storage),
            self.enable_codex_api_key_env,
            store_origin,
        );
        self.set_cached_with_auth(store, auth.clone(), store_origin);
        auth
    }

    pub fn active_chatgpt_account_summary(&self) -> Option<ActiveChatgptAccountSummary> {
        self.auth_cached()
            .and_then(|auth| auth.active_chatgpt_account_summary())
    }

    fn chatgpt_auth_for_store_account_id(&self, store_account_id: &str) -> Option<CodexAuth> {
        let store = self.inner.read().ok()?.store.clone();
        Self::derive_chatgpt_auth_from_store_account(
            &store,
            store_account_id,
            Arc::clone(&self.storage),
        )
    }

    // Merge-safety anchor: `/accounts`, usage-limit auto-switch, and active auth recovery must use
    // one canonical owner for per-account refresh failure eviction.
    pub async fn resolve_chatgpt_auth_for_store_account_id(
        &self,
        store_account_id: &str,
        refresh_mode: ChatgptAccountRefreshMode,
    ) -> Result<ChatgptAccountAuthResolution, RefreshTokenError> {
        let Some(auth) = self.chatgpt_auth_for_store_account_id(store_account_id) else {
            return Ok(ChatgptAccountAuthResolution::Missing);
        };
        let CodexAuth::Chatgpt(chatgpt_auth) = &auth else {
            return Ok(ChatgptAccountAuthResolution::Auth(Box::new(auth)));
        };

        let cached_refresh_failure =
            self.auth_cached()
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
            return Ok(ChatgptAccountAuthResolution::Auth(Box::new(auth)));
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
                self.reload();
                Ok(self
                    .chatgpt_auth_for_store_account_id(store_account_id)
                    .map(Box::new)
                    .map(ChatgptAccountAuthResolution::Auth)
                    .unwrap_or(ChatgptAccountAuthResolution::Missing))
            }
            Err(RefreshTokenError::Permanent(error)) => {
                if let TerminalRefreshFailureAccountRemoval::Removed {
                    switched_to_store_account_id,
                } = self.remove_chatgpt_store_account_for_terminal_refresh_failure(
                    chatgpt_auth.store_account_id(),
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

    pub fn list_accounts(&self) -> Vec<AccountSummary> {
        let Ok(guard) = self.inner.read() else {
            return Vec::new();
        };

        let mut store = guard.store.clone();
        drop(guard);

        if let Some(account_state_store) = self.account_state_store.as_ref()
            && let Err(error) = hydrate_store_usage_from_sqlite(account_state_store, &mut store)
        {
            tracing::warn!(
                error = %error,
                "failed to refresh saved-account usage truth before listing accounts"
            );
        }
        if let Err(error) = Self::hydrate_runtime_active_account(
            self.account_state_store.as_ref(),
            self.runtime_session_id.as_str(),
            self.forced_chatgpt_workspace_id().as_deref(),
            &mut store,
        ) {
            tracing::warn!(
                error = %error,
                "failed to hydrate runtime active-account state before listing accounts"
            );
            store.active_account_id = None;
        }

        let active_account_id = store.active_account_id.clone();
        store
            .accounts
            .iter()
            .map(|account| AccountSummary::from_stored(account, active_account_id.as_deref()))
            .collect()
    }

    pub fn set_active_account(&self, id: &str) -> std::io::Result<()> {
        let required_workspace_id = self.forced_chatgpt_workspace_id();
        self.update_store(|store| {
            let Some(account) = store.accounts.iter().find(|account| account.id == id) else {
                return Err(std::io::Error::other(format!("account '{id}' not found")));
            };
            if !account_matches_required_workspace(account, required_workspace_id.as_deref())
                && let Some(required_workspace_id) = required_workspace_id.as_deref()
            {
                return Err(std::io::Error::other(format!(
                    "account '{id}' does not match required workspace {required_workspace_id:?}"
                )));
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
            if make_active {
                store.active_account_id = Some(account_id.clone());
            }
            Ok(account_id.clone())
        })
    }

    pub fn remove_account(&self, id: &str) -> std::io::Result<bool> {
        self.update_store(|store| {
            let required_workspace_id = self.forced_chatgpt_workspace_id();
            let prev_len = store.accounts.len();
            store.accounts.retain(|account| account.id != id);
            let removed = store.accounts.len() != prev_len;
            if !removed {
                return Ok(false);
            }
            if store.active_account_id.as_deref() == Some(id) {
                store.active_account_id = self.select_account_for_auto_switch_with_leases(
                    store,
                    required_workspace_id.as_deref(),
                    /*exclude_store_account_id*/ None,
                    Utc::now(),
                    UsageLimitAutoSwitchSelectionScope::PersistedTruth,
                );
            }
            Ok(true)
        })
    }

    pub fn update_usage_for_active(&self, snapshot: RateLimitSnapshot) -> std::io::Result<()> {
        if !self.has_saved_chatgpt_accounts() {
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
        snapshot: RateLimitSnapshot,
    ) -> std::io::Result<()> {
        if !self.has_saved_chatgpt_accounts() {
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
            let usage = account.usage.get_or_insert_with(AccountUsageCache::default);
            apply_rate_limit_refresh_outcome(
                usage,
                AccountRateLimitRefreshOutcome::Snapshot(snapshot),
                now,
            );
            Ok(())
        })
    }

    pub fn update_rate_limits_for_accounts(
        &self,
        updates: impl IntoIterator<Item = (String, RateLimitSnapshot)>,
    ) -> std::io::Result<usize> {
        if !self.has_saved_chatgpt_accounts() {
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
                    let usage = account.usage.get_or_insert_with(AccountUsageCache::default);
                    apply_rate_limit_refresh_outcome(
                        usage,
                        AccountRateLimitRefreshOutcome::Snapshot(snapshot),
                        now,
                    );
                    updated = updated.saturating_add(1);
                }
            }
            Ok(updated)
        })
    }

    pub fn reconcile_account_rate_limit_refresh_outcomes(
        &self,
        outcomes: impl IntoIterator<Item = (String, AccountRateLimitRefreshOutcome)>,
    ) -> std::io::Result<usize> {
        if !self.has_saved_chatgpt_accounts() {
            return Ok(0);
        }

        let mut outcomes = outcomes.into_iter().collect::<HashMap<_, _>>();
        if outcomes.is_empty() {
            return Ok(0);
        }

        self.update_store(|store| {
            let now = Utc::now();
            let mut updated = 0usize;
            for account in &mut store.accounts {
                if let Some(outcome) = outcomes.remove(&account.id) {
                    let usage = account.usage.get_or_insert_with(AccountUsageCache::default);
                    apply_rate_limit_refresh_outcome(usage, outcome, now);
                    updated = updated.saturating_add(1);
                }
            }
            Ok(updated)
        })
    }

    pub fn mark_usage_limit_reached(
        &self,
        resets_at: Option<DateTime<Utc>>,
        snapshot: Option<RateLimitSnapshot>,
    ) -> std::io::Result<()> {
        if !self.has_saved_chatgpt_accounts() {
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
            if let Some(snapshot) = snapshot.or_else(|| usage.last_rate_limits.clone()) {
                usage.last_rate_limits = Some(clamp_usage_limit_snapshot(snapshot, resets_at, now));
            }

            let exhausted_until = exhausted_until(resets_at, usage.last_rate_limits.as_ref(), now);
            usage.exhausted_until = Some(exhausted_until);
            Ok(())
        })
    }

    pub fn switch_account_on_usage_limit(
        &self,
        request: UsageLimitAutoSwitchRequest<'_>,
    ) -> std::io::Result<Option<String>> {
        let UsageLimitAutoSwitchRequest {
            required_workspace_id,
            failing_store_account_id,
            resets_at,
            snapshot,
            freshly_unsupported_store_account_ids,
            protected_store_account_id,
            selection_scope,
            fallback_selection_mode,
        } = request;
        if !self.has_saved_chatgpt_accounts() {
            return Ok(None);
        }

        let cooldown_check_now = Utc::now();
        let mut cooldown_until = self
            .usage_limit_auto_switch_cooldown_until
            .lock()
            .map_err(|_| std::io::Error::other("auto-switch cooldown lock poisoned"))?;
        let cooldown_active = cooldown_until.is_some_and(|until| until > cooldown_check_now);
        if cooldown_active {
            tracing::debug!(
                cooldown_until = ?*cooldown_until,
                "skipping usage-limit auto-switch during cooldown"
            );
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

            if cooldown_active {
                return Ok(None);
            }

            if let Some(protected_store_account_id) = protected_store_account_id
                && store.accounts.iter().any(|account| {
                    account.id == protected_store_account_id
                        && account_selectable_for_auto_switch(
                            account,
                            required_workspace_id,
                            mutation_now,
                            selection_scope,
                        )
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

            if fallback_selection_mode
                == UsageLimitAutoSwitchFallbackSelectionMode::CancelStaleRequestFallbackSelection
            {
                return Ok(None);
            }

            let Some(next_account_id) = self.select_account_for_auto_switch_with_leases(
                store,
                required_workspace_id,
                Some(failing_store_account_id.as_str()),
                mutation_now,
                selection_scope,
            ) else {
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

    fn select_account_for_auto_switch_with_leases(
        &self,
        store: &AuthStore,
        required_workspace_id: Option<&str>,
        exclude_store_account_id: Option<&str>,
        now: DateTime<Utc>,
        selection_scope: UsageLimitAutoSwitchSelectionScope,
    ) -> Option<String> {
        let account_state_store = self.account_state_store.as_ref()?;
        let mut filtered_store = store.clone();
        filtered_store.accounts.retain(|account| {
            if Some(account.id.as_str()) == exclude_store_account_id {
                return false;
            }
            match account_state_store.account_is_leased_by_other(
                self.runtime_session_id.as_str(),
                account.id.as_str(),
                now,
            ) {
                Ok(false) => true,
                Ok(true) => false,
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        account_id = account.id,
                        "failed to evaluate active-account lease conflict while selecting an auto-switch candidate"
                    );
                    false
                }
            }
        });
        select_account_for_auto_switch_from_store(
            &filtered_store,
            required_workspace_id,
            exclude_store_account_id,
            now,
            selection_scope,
        )
    }

    pub fn select_account_for_auto_switch(
        &self,
        required_workspace_id: Option<&str>,
        exclude_store_account_id: Option<&str>,
    ) -> Option<String> {
        if !self.has_saved_chatgpt_accounts() {
            return None;
        }
        let store = self.inner.read().ok()?.store.clone();
        self.select_account_for_auto_switch_with_leases(
            &store,
            required_workspace_id,
            exclude_store_account_id,
            Utc::now(),
            UsageLimitAutoSwitchSelectionScope::PersistedTruth,
        )
    }

    pub fn accounts_rate_limits_cache_expires_at(
        &self,
        now: DateTime<Utc>,
    ) -> Option<DateTime<Utc>> {
        if !self.has_saved_chatgpt_accounts() {
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

    pub fn refresh_failure_for_auth(&self, auth: &CodexAuth) -> Option<RefreshTokenFailedError> {
        self.inner.read().ok().and_then(|cached| {
            cached
                .permanent_refresh_failure
                .as_ref()
                .filter(|failure| Self::auths_equal_for_refresh(Some(auth), Some(&failure.auth)))
                .map(|failure| failure.error.clone())
        })
    }
    /// Current cached auth (clone). May be `None` if not logged in or load failed.
    /// For stale managed ChatGPT auth, first performs a guarded reload and then
    /// refreshes only if the on-disk auth is unchanged.
    pub async fn auth(&self) -> Option<CodexAuth> {
        if let Some(auth) = self.resolve_external_api_key_auth().await {
            return Some(auth);
        }

        let auth = self.auth_cached()?;
        if Self::is_stale_for_proactive_refresh(&auth)
            && let Err(err) = self.refresh_token().await
        {
            tracing::error!("Failed to refresh token: {}", err);
            return self.auth_cached();
        }
        self.auth_cached()
    }

    /// Force a reload of the auth information from auth.json. Returns
    /// whether the auth value changed.
    pub fn reload(&self) -> bool {
        tracing::info!("Reloading auth");
        let loaded = self.load_store_from_storage();
        let auth = Self::derive_auth_from_store(
            &loaded.store,
            Arc::clone(&self.storage),
            self.enable_codex_api_key_env,
            loaded.store_origin,
        );
        self.set_cached_with_auth(loaded.store, auth, loaded.store_origin)
    }

    /// Like `reload()`, but fails loudly if the auth store cannot be loaded.
    pub fn reload_strict(&self) -> std::io::Result<bool> {
        tracing::info!("Reloading auth (strict)");
        let _lock = storage::lock_auth_store(&self.codex_home)?;
        let mut store = self.storage.load()?.unwrap_or_default();
        let loaded_had_persisted_active_account = store.active_account_id.is_some();
        let removed_account_ids = enforce_supported_chatgpt_auth_accounts(&mut store);
        let mut sync_outcome = UsageStateSyncOutcome::default();
        if let Some(account_state_store) = self.account_state_store.as_ref() {
            sync_outcome =
                synchronize_store_usage_from_legacy_and_sqlite(account_state_store, &mut store)?;
        }
        Self::hydrate_runtime_active_account(
            self.account_state_store.as_ref(),
            self.runtime_session_id.as_str(),
            self.forced_chatgpt_workspace_id().as_deref(),
            &mut store,
        )?;
        if sync_outcome.strip_persisted_auth_store
            || !removed_account_ids.is_empty()
            || (self.account_state_store.is_some() && loaded_had_persisted_active_account)
        {
            persist_auth_store(
                &*self.storage,
                &store,
                sync_outcome.strip_persisted_auth_store,
            )?;
        }
        Ok(self.set_cached(store))
    }

    fn reload_if_store_account_id_matches(
        &self,
        expected_store_account_id: Option<&str>,
    ) -> ReloadOutcome {
        let expected_store_account_id = match expected_store_account_id {
            Some(store_account_id) => store_account_id,
            None => {
                tracing::info!("Skipping auth reload because no saved account id is available.");
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
        let loaded_had_persisted_active_account = store.active_account_id.is_some();
        let removed_account_ids = enforce_supported_chatgpt_auth_accounts(&mut store);
        if !removed_account_ids.is_empty() {
            tracing::info!(
                removed_account_ids = ?removed_account_ids,
                "removed accounts with unsupported ChatGPT plans during guarded auth reload"
            );
        }
        if let Err(error) = Self::hydrate_runtime_active_account(
            self.account_state_store.as_ref(),
            self.runtime_session_id.as_str(),
            self.forced_chatgpt_workspace_id().as_deref(),
            &mut store,
        ) {
            tracing::warn!(
                error = %error,
                "skipping guarded auth reload because runtime active-account state could not be hydrated"
            );
            return ReloadOutcome::Skipped;
        }
        if loaded_had_persisted_active_account || !removed_account_ids.is_empty() {
            let persist_result = if self.account_state_store.is_some() {
                let mut stripped_store = store.clone();
                strip_runtime_active_account_from_store(&mut stripped_store);
                save_auth(
                    &self.codex_home,
                    &stripped_store,
                    self.auth_credentials_store_mode,
                )
            } else {
                save_auth(&self.codex_home, &store, self.auth_credentials_store_mode)
            };
            if let Err(error) = persist_result {
                tracing::warn!(
                    error = %error,
                    "failed to persist auth store during guarded auth reload"
                );
                return ReloadOutcome::Skipped;
            }
        }

        let new_auth = Self::derive_auth_from_store(
            &store,
            Arc::clone(&self.storage),
            self.enable_codex_api_key_env,
            CachedStoreOrigin::Persistent,
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
                self.set_cached(store);
                return ReloadOutcome::ReloadedChanged;
            }
            let found_store_account_id = new_store_account_id.as_deref().unwrap_or("unknown");
            tracing::info!(
                "Skipping auth reload due to saved account id mismatch (expected: {expected_store_account_id}, found: {found_store_account_id})"
            );
            return ReloadOutcome::Skipped;
        }

        tracing::info!("Reloading auth for saved account {expected_store_account_id}");
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
        storage: Arc<dyn AuthStorageBackend>,
        enable_codex_api_key_env: bool,
        store_origin: CachedStoreOrigin,
    ) -> Option<CodexAuth> {
        if enable_codex_api_key_env && let Some(api_key) = read_codex_api_key_from_env() {
            return Some(CodexAuth::from_api_key(&api_key));
        }

        if let Some(active_account) = store.active_account() {
            let (auth_mode, storage) = match store_origin {
                CachedStoreOrigin::Persistent => (ApiAuthMode::Chatgpt, Some(storage)),
                CachedStoreOrigin::ExternalEphemeral => (ApiAuthMode::ChatgptAuthTokens, None),
            };
            return Some(
                CodexAuth::from_chatgpt_active_account_snapshot(
                    ActiveChatgptAccountSnapshot::from_stored_account(active_account, auth_mode),
                    storage,
                )
                .unwrap_or_else(|error| match store_origin {
                    CachedStoreOrigin::Persistent => {
                        panic!("persisted ChatGPT auth should always have a backing store: {error}")
                    }
                    CachedStoreOrigin::ExternalEphemeral => {
                        panic!("external ChatGPT token auth should be constructible: {error}")
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

    fn set_cached_with_auth(
        &self,
        store: AuthStore,
        new_auth: Option<CodexAuth>,
        store_origin: CachedStoreOrigin,
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
            changed
        } else {
            false
        }
    }

    fn load_store_from_storage(&self) -> LoadedCachedStore {
        Self::load_store_from_storage_impl(
            &self.codex_home,
            &self.storage,
            self.auth_credentials_store_mode,
            self.account_state_store.as_ref(),
            self.runtime_session_id.as_str(),
            self.forced_chatgpt_workspace_id().as_deref(),
        )
    }

    fn load_store_from_storage_impl(
        codex_home: &Path,
        storage: &Arc<dyn AuthStorageBackend>,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
        account_state_store: Option<&AccountStateStore>,
        runtime_session_id: &str,
        required_workspace_id: Option<&str>,
    ) -> LoadedCachedStore {
        let external_storage = (auth_credentials_store_mode != AuthCredentialsStoreMode::Ephemeral)
            .then(|| {
                create_auth_storage(
                    codex_home.to_path_buf(),
                    AuthCredentialsStoreMode::Ephemeral,
                )
            });

        let (mut store, store_origin, persist_storage) = match external_storage.as_ref() {
            Some(external_storage) => match external_storage.load() {
                Ok(Some(store)) if !store.accounts.is_empty() || store.openai_api_key.is_some() => {
                    (
                        store,
                        CachedStoreOrigin::ExternalEphemeral,
                        Arc::clone(external_storage),
                    )
                }
                Ok(Some(_)) | Ok(None) => match storage.load() {
                    Ok(Some(store)) => (store, CachedStoreOrigin::Persistent, Arc::clone(storage)),
                    Ok(None) => (
                        AuthStore::default(),
                        CachedStoreOrigin::Persistent,
                        Arc::clone(storage),
                    ),
                    Err(err) => {
                        tracing::warn!("Failed to load auth store: {err}");
                        (
                            AuthStore::default(),
                            CachedStoreOrigin::Persistent,
                            Arc::clone(storage),
                        )
                    }
                },
                Err(err) => {
                    tracing::warn!("Failed to load external auth store: {err}");
                    match storage.load() {
                        Ok(Some(store)) => {
                            (store, CachedStoreOrigin::Persistent, Arc::clone(storage))
                        }
                        Ok(None) => (
                            AuthStore::default(),
                            CachedStoreOrigin::Persistent,
                            Arc::clone(storage),
                        ),
                        Err(err) => {
                            tracing::warn!("Failed to load auth store: {err}");
                            (
                                AuthStore::default(),
                                CachedStoreOrigin::Persistent,
                                Arc::clone(storage),
                            )
                        }
                    }
                }
            },
            None => match storage.load() {
                Ok(Some(store)) => {
                    let store_origin =
                        if auth_credentials_store_mode == AuthCredentialsStoreMode::Ephemeral {
                            CachedStoreOrigin::ExternalEphemeral
                        } else {
                            CachedStoreOrigin::Persistent
                        };
                    (store, store_origin, Arc::clone(storage))
                }
                Ok(None) => (
                    AuthStore::default(),
                    CachedStoreOrigin::Persistent,
                    Arc::clone(storage),
                ),
                Err(err) => {
                    tracing::warn!("Failed to load auth store: {err}");
                    (
                        AuthStore::default(),
                        CachedStoreOrigin::Persistent,
                        Arc::clone(storage),
                    )
                }
            },
        };

        let loaded_had_persisted_active_account = store.active_account_id.is_some();
        let removed_account_ids = enforce_supported_chatgpt_auth_accounts(&mut store);
        let mut sync_outcome = UsageStateSyncOutcome::default();
        if let Some(account_state_store) = account_state_store {
            match synchronize_store_usage_from_legacy_and_sqlite(account_state_store, &mut store) {
                Ok(outcome) => {
                    sync_outcome = outcome;
                }
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        "failed to synchronize saved-account usage truth while loading auth store"
                    );
                }
            }
        }
        if !removed_account_ids.is_empty() {
            tracing::info!(
                removed_account_ids = ?removed_account_ids,
                "removed accounts with unsupported ChatGPT plans while loading auth store"
            );
        }
        if let Err(error) = Self::hydrate_runtime_active_account(
            account_state_store,
            runtime_session_id,
            required_workspace_id,
            &mut store,
        ) {
            tracing::warn!(
                error = %error,
                "failed to hydrate runtime active-account state while loading auth store"
            );
            store.active_account_id = None;
        }
        if (sync_outcome.strip_persisted_auth_store
            || !removed_account_ids.is_empty()
            || (account_state_store.is_some() && loaded_had_persisted_active_account))
            && let Err(error) = persist_auth_store(
                &*persist_storage,
                &store,
                sync_outcome.strip_persisted_auth_store,
            )
        {
            tracing::warn!(
                error = %error,
                "failed to persist auth store while loading store"
            );
        }

        LoadedCachedStore {
            store,
            store_origin,
        }
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

    fn set_cached(&self, store: AuthStore) -> bool {
        let store_origin = self
            .inner
            .read()
            .ok()
            .map(|guard| guard.store_origin)
            .unwrap_or(CachedStoreOrigin::Persistent);
        let new_auth = Self::derive_auth_from_store(
            &store,
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
    ) -> Option<CodexAuth> {
        let account = store.account(store_account_id)?;
        Some(
            CodexAuth::from_chatgpt_active_account_snapshot(
                ActiveChatgptAccountSnapshot::from_stored_account(account, ApiAuthMode::Chatgpt),
                Some(storage),
            )
            .unwrap_or_else(|error| {
                panic!("stored ChatGPT account lookup should always have a backing store: {error}")
            }),
        )
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
        if let Some(account_state_store) = self.account_state_store.as_ref() {
            synchronize_store_usage_from_legacy_and_sqlite(account_state_store, &mut store)?;
        }
        Self::hydrate_runtime_active_account(
            self.account_state_store.as_ref(),
            self.runtime_session_id.as_str(),
            self.forced_chatgpt_workspace_id().as_deref(),
            &mut store,
        )?;

        let out = mutator(&mut store)?;
        let removed_after_mutation = enforce_supported_chatgpt_auth_accounts(&mut store);
        if !removed_after_mutation.is_empty() {
            tracing::info!(
                removed_account_ids = ?removed_after_mutation,
                "removed accounts with unsupported ChatGPT plans after auth store mutation"
            );
        }
        self.reconcile_runtime_active_account(&store)?;
        store.validate()?;
        if let Some(account_state_store) = self.account_state_store.as_ref() {
            persist_usage_state_from_store(account_state_store, &store)?;
            persist_stripped_auth_store(&*self.storage, &store)?;
        } else {
            self.storage.save(&store)?;
        }
        self.set_cached(store);
        Ok(out)
    }

    pub fn set_external_auth(&self, external_auth: Arc<dyn ExternalAuth>) {
        if let Ok(mut guard) = self.external_auth.write() {
            *guard = Some(external_auth);
        }
    }

    pub fn clear_external_auth(&self) {
        if let Ok(mut guard) = self.external_auth.write() {
            *guard = None;
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

    pub fn has_external_auth(&self) -> bool {
        self.external_auth().is_some()
    }

    pub fn is_external_chatgpt_auth_active(&self) -> bool {
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
    ) -> Arc<Self> {
        Arc::new(Self::new_with_sqlite_home(
            codex_home,
            sqlite_home,
            enable_codex_api_key_env,
            auth_credentials_store_mode,
        ))
    }

    /// Convenience constructor returning an `Arc` wrapper from resolved config.
    pub fn shared_from_config(
        config: &impl AuthManagerConfig,
        enable_codex_api_key_env: bool,
    ) -> Arc<Self> {
        let auth_manager = Self::shared_with_sqlite_home(
            config.codex_home(),
            config.sqlite_home(),
            enable_codex_api_key_env,
            config.cli_auth_credentials_store_mode(),
        );
        auth_manager.set_forced_chatgpt_workspace_id(config.forced_chatgpt_workspace_id());
        auth_manager
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
        let auth_before_reload = self.auth_cached();
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

        match self.reload_if_store_account_id_matches(expected_store_account_id.as_deref()) {
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

        let auth = match self.auth_cached() {
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
                    .resolve_chatgpt_auth_for_store_account_id(
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
                        let auth_after_removal = self.auth_cached();
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

        let _lock = storage::lock_auth_store(&self.codex_home)?;
        let mut store = match self.storage.load().map_err(RefreshTokenError::Transient)? {
            Some(store) => store,
            None => {
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
                "removed accounts with unsupported ChatGPT plans before terminal refresh-token eviction"
            );
        }
        Self::hydrate_runtime_active_account(
            self.account_state_store.as_ref(),
            self.runtime_session_id.as_str(),
            self.forced_chatgpt_workspace_id().as_deref(),
            &mut store,
        )
        .map_err(RefreshTokenError::Transient)?;

        let was_active = store.active_account_id.as_deref() == Some(store_account_id);
        let previous_account_count = store.accounts.len();
        store
            .accounts
            .retain(|account| account.id != store_account_id);
        if store.accounts.len() == previous_account_count {
            return Ok(TerminalRefreshFailureAccountRemoval::NotRemoved);
        }

        let required_workspace_id = self.forced_chatgpt_workspace_id();
        let switched_to_store_account_id = if was_active {
            let next_store_account_id = self.select_account_for_auto_switch_with_leases(
                &store,
                required_workspace_id.as_deref(),
                /*exclude_store_account_id*/ None,
                Utc::now(),
                UsageLimitAutoSwitchSelectionScope::PersistedTruth,
            );
            store.active_account_id = next_store_account_id.clone();
            next_store_account_id
        } else {
            store.active_account_id = store.active_account_id.clone().filter(|active_account_id| {
                store
                    .accounts
                    .iter()
                    .any(|account| &account.id == active_account_id)
            });
            None
        };

        let removed_after_mutation = enforce_supported_chatgpt_auth_accounts(&mut store);
        if !removed_after_mutation.is_empty() {
            tracing::info!(
                removed_account_ids = ?removed_after_mutation,
                "removed accounts with unsupported ChatGPT plans after terminal refresh-token eviction"
            );
        }

        self.reconcile_runtime_active_account(&store)
            .map_err(RefreshTokenError::Transient)?;
        store.validate().map_err(RefreshTokenError::Transient)?;
        if let Some(account_state_store) = self.account_state_store.as_ref() {
            persist_usage_state_from_store(account_state_store, &store)
                .map_err(RefreshTokenError::Transient)?;
            persist_stripped_auth_store(&*self.storage, &store)
                .map_err(RefreshTokenError::Transient)?;
        } else {
            self.storage
                .save(&store)
                .map_err(RefreshTokenError::Transient)?;
        }

        let cached_auth =
            if let Some(switched_to_store_account_id) = switched_to_store_account_id.as_deref() {
                Self::derive_chatgpt_auth_from_store_account(
                    &store,
                    switched_to_store_account_id,
                    Arc::clone(&self.storage),
                )
            } else if was_active {
                None
            } else {
                Self::derive_auth_from_store(
                    &store,
                    Arc::clone(&self.storage),
                    self.enable_codex_api_key_env,
                    CachedStoreOrigin::Persistent,
                )
            };
        self.set_cached_with_auth(store, cached_auth, CachedStoreOrigin::Persistent);
        tracing::warn!(
            store_account_id,
            failed_reason = ?error.reason,
            switched_to_store_account_id,
            "removed saved ChatGPT account after terminal refresh-token failure"
        );
        Ok(TerminalRefreshFailureAccountRemoval::Removed {
            switched_to_store_account_id,
        })
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
        if self.has_external_api_key_auth() {
            return Some(ApiAuthMode::ApiKey);
        }
        self.auth_cached().as_ref().map(CodexAuth::api_auth_mode)
    }

    pub fn get_auth_mode(&self) -> Option<ApiAuthMode> {
        self.get_api_auth_mode()
    }

    fn has_saved_chatgpt_accounts(&self) -> bool {
        self.inner
            .read()
            .ok()
            .is_some_and(|cached| !cached.store.accounts.is_empty())
    }

    pub fn auth_mode(&self) -> Option<AuthMode> {
        if self.has_external_api_key_auth() {
            return Some(AuthMode::ApiKey);
        }
        self.get_internal_auth_mode()
    }

    pub fn get_internal_auth_mode(&self) -> Option<AuthMode> {
        self.auth_cached()
            .as_ref()
            .map(CodexAuth::internal_auth_mode)
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
        let forced_chatgpt_workspace_id = self.forced_chatgpt_workspace_id();
        let previous_account_id = self
            .auth_cached()
            .as_ref()
            .and_then(CodexAuth::get_account_id);
        let active_store_account_id = self
            .auth_cached()
            .as_ref()
            .and_then(CodexAuth::active_chatgpt_account_summary)
            .map(|summary| summary.store_account_id);
        let context = ExternalAuthRefreshContext {
            reason,
            previous_account_id,
        };

        let refreshed = match external_auth.refresh(context).await {
            Ok(refreshed) => refreshed,
            Err(error) => {
                return self.finish_external_auth_refresh_failure(
                    active_store_account_id.as_deref(),
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

    fn finish_external_auth_refresh_failure(
        &self,
        active_store_account_id: Option<&str>,
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
            &error,
        )? {
            TerminalRefreshFailureAccountRemoval::Removed {
                switched_to_store_account_id,
            } => {
                let active_store_account_id_after_removal = self
                    .auth_cached()
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
    pub last_rate_limits: Option<RateLimitSnapshot>,
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

    if store
        .active_account_id
        .as_ref()
        .is_some_and(|active_account_id| {
            !store
                .accounts
                .iter()
                .any(|account| &account.id == active_account_id)
        })
    {
        store.active_account_id = None;
    }

    removed_account_ids
}

fn exhausted_until(
    resets_at: Option<DateTime<Utc>>,
    snapshot: Option<&RateLimitSnapshot>,
    now: DateTime<Utc>,
) -> DateTime<Utc> {
    let from_snapshot = snapshot.and_then(|snapshot| {
        exhausted_until_from_snapshot(snapshot, now)
            .or_else(|| snapshot_next_reset_at(snapshot, now))
    });
    resets_at
        .or(from_snapshot)
        .unwrap_or_else(|| now + chrono::Duration::minutes(15))
}

fn exhausted_until_from_snapshot(
    snapshot: &RateLimitSnapshot,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    if rate_limit_window_blocked(snapshot.secondary.as_ref(), now) {
        return Some(
            rate_limit_window_reset_at(snapshot.secondary.as_ref())
                .unwrap_or_else(|| now + chrono::Duration::minutes(15)),
        );
    }
    if rate_limit_window_blocked(snapshot.primary.as_ref(), now) {
        return Some(
            rate_limit_window_reset_at(snapshot.primary.as_ref())
                .unwrap_or_else(|| now + chrono::Duration::minutes(15)),
        );
    }
    None
}

fn rate_limit_window_blocked(window: Option<&RateLimitWindow>, now: DateTime<Utc>) -> bool {
    let Some(window) = window else {
        return false;
    };

    window.is_depleted_at(now.timestamp())
}

fn rate_limit_window_reset_at(window: Option<&RateLimitWindow>) -> Option<DateTime<Utc>> {
    let window = window?;
    let resets_at_seconds = window.resets_at?;
    DateTime::<Utc>::from_timestamp(resets_at_seconds, 0)
}

fn snapshot_next_reset_at(
    snapshot: &RateLimitSnapshot,
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

pub fn usage_limit_auto_switch_removes_plan_type(plan_type: Option<&AccountPlanType>) -> bool {
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

fn account_selectable_for_auto_switch(
    account: &StoredAccount,
    required_workspace_id: Option<&str>,
    now: DateTime<Utc>,
    selection_scope: UsageLimitAutoSwitchSelectionScope<'_>,
) -> bool {
    if !account_selectable(account, required_workspace_id, now) {
        return false;
    }

    match selection_scope {
        UsageLimitAutoSwitchSelectionScope::PersistedTruth => true,
        UsageLimitAutoSwitchSelectionScope::FreshlySelectable(selectable_store_account_ids) => {
            selectable_store_account_ids.contains(&account.id)
        }
    }
}

fn select_account_for_auto_switch_from_store(
    store: &AuthStore,
    required_workspace_id: Option<&str>,
    exclude_store_account_id: Option<&str>,
    now: DateTime<Utc>,
    selection_scope: UsageLimitAutoSwitchSelectionScope<'_>,
) -> Option<String> {
    let mut candidates = store
        .accounts
        .iter()
        .filter(|account| {
            Some(account.id.as_str()) != exclude_store_account_id
                && account_selectable_for_auto_switch(
                    account,
                    required_workspace_id,
                    now,
                    selection_scope,
                )
        })
        .collect::<Vec<_>>();

    candidates.sort_by(|a, b| compare_auto_switch_candidates(a, b));
    candidates.first().map(|account| account.id.clone())
}

// Merge-safety anchor: saved-account usage-limit auto-switch ranking must prefer the fallback
// most likely to survive the immediate retry: primary-window headroom first, then weekly
// headroom, then bounded tie-breakers.
fn compare_auto_switch_candidates(a: &StoredAccount, b: &StoredAccount) -> std::cmp::Ordering {
    let a_snapshot = a.usage.as_ref().and_then(|u| u.last_rate_limits.as_ref());
    let b_snapshot = b.usage.as_ref().and_then(|u| u.last_rate_limits.as_ref());

    let (a_primary_kind, a_primary_remaining) = primary_remaining_percent_rank(a_snapshot);
    let (b_primary_kind, b_primary_remaining) = primary_remaining_percent_rank(b_snapshot);

    let (a_weekly_kind, a_weekly_remaining) = weekly_remaining_percent_rank(a_snapshot);
    let (b_weekly_kind, b_weekly_remaining) = weekly_remaining_percent_rank(b_snapshot);

    let (a_credit_kind, a_balance) = credits_balance_rank(a_snapshot);
    let (b_credit_kind, b_balance) = credits_balance_rank(b_snapshot);

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
        a_primary_kind,
        a_primary_remaining,
        a_weekly_kind,
        a_weekly_remaining,
        a_credit_kind,
        a_balance,
        a_last_seen,
        a.id.as_str(),
    )
        .cmp(&(
            b_primary_kind,
            b_primary_remaining,
            b_weekly_kind,
            b_weekly_remaining,
            b_credit_kind,
            b_balance,
            b_last_seen,
            b.id.as_str(),
        ))
}

fn credits_balance_rank(snapshot: Option<&RateLimitSnapshot>) -> (u8, i64) {
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

fn weekly_remaining_percent_rank(snapshot: Option<&RateLimitSnapshot>) -> (u8, i64) {
    let Some(snapshot) = snapshot else {
        return (1, 0);
    };
    let Some(window) = snapshot.secondary.as_ref() else {
        return (1, 0);
    };
    if window.window_minutes.is_some() {
        return (1, 0);
    }
    (0, -percent_basis_points(window.remaining_percent))
}

fn primary_remaining_percent_rank(snapshot: Option<&RateLimitSnapshot>) -> (u8, i64) {
    let Some(snapshot) = snapshot else {
        return (1, 0);
    };
    let Some(window) = snapshot.primary.as_ref() else {
        return (1, 0);
    };
    (0, -percent_basis_points(window.remaining_percent))
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
