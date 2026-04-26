use chrono::DateTime;
use chrono::Utc;
use codex_account_state::AccountLeaseConflict;
use codex_account_state::AccountStateStore;
use codex_account_state::AccountUsageState;
use codex_account_state::ForceReleaseAccountOutcome;
use codex_account_state::SessionActiveAccountRefresh;
use codex_account_state::SessionActiveAccountSetOutcome;
use codex_app_server_protocol::AuthMode;
use codex_app_server_protocol::AuthMode as ApiAuthMode;
use codex_config::types::AuthCredentialsStoreMode;
use codex_protocol::account::PlanType as AccountPlanType;
use codex_protocol::auth::PlanType as InternalPlanType;
use codex_protocol::protocol::RateLimitSnapshot;
use codex_protocol::protocol::RateLimitWindow;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::RwLock;

use super::account_runtime_context::AccountRuntimeContext;
use crate::auth::storage::AccountUsageCache;
use crate::auth::storage::AuthStorageBackend;
use crate::auth::storage::AuthStore;
use crate::auth::storage::StoredAccount;
use crate::auth::storage::create_auth_storage;
use crate::auth::storage::lock_auth_store;
use crate::token_data::TokenData;

const USAGE_LIMIT_AUTO_SWITCH_COOLDOWN_SECONDS: i64 = 2;
pub(super) const ACTIVE_ACCOUNT_LEASE_TTL_SECONDS: i64 = 5 * 60;

// Merge-safety anchor: active account projections belong to AccountManager
// because it owns runtime-active account selection, saved-account roster truth,
// and auth-store-origin interpretation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveChatgptAccountSummary {
    pub store_account_id: String,
    pub label: Option<String>,
    pub email: Option<String>,
    pub auth_mode: AuthMode,
}

// Merge-safety anchor: loaded-store origin is the AccountManager loader
// contract; AuthManager consumes the selected store snapshot but does not own
// origin selection or external-ephemeral admission semantics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LoadedStoreOrigin {
    Persistent,
    ExternalEphemeral,
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

pub(super) fn strip_runtime_active_account_from_store(store: &mut AuthStore) {
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

fn lease_conflict_error(account_id: &str, conflict: &AccountLeaseConflict) -> std::io::Error {
    std::io::Error::other(format!(
        "account '{account_id}' is currently leased by another live session until {}",
        conflict.lease_until
    ))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ChatgptAuthAdmissionPolicy {
    Persisted,
    ExternalStrict,
}

fn is_admitted_chatgpt_auth_account(
    account: &StoredAccount,
    admission_policy: ChatgptAuthAdmissionPolicy,
) -> bool {
    match admission_policy {
        ChatgptAuthAdmissionPolicy::Persisted => match account.tokens.id_token.chatgpt_plan_type {
            None | Some(InternalPlanType::Unknown(_)) => true,
            Some(_) => account.tokens.id_token.is_supported_chatgpt_auth_plan(),
        },
        ChatgptAuthAdmissionPolicy::ExternalStrict => {
            account.tokens.id_token.is_supported_chatgpt_auth_plan()
        }
    }
}

pub(super) fn enforce_chatgpt_auth_accounts(
    store: &mut AuthStore,
    admission_policy: ChatgptAuthAdmissionPolicy,
) -> Vec<String> {
    let mut removed_account_ids = Vec::new();
    store.accounts.retain(|account| {
        let keep_account = is_admitted_chatgpt_auth_account(account, admission_policy);
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

pub(super) fn account_matches_required_workspace(
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
    matches!(plan_type, Some(AccountPlanType::Free))
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

#[derive(Clone)]
pub(super) struct LoadedAuthStore {
    pub(super) store: AuthStore,
    pub(super) store_origin: LoadedStoreOrigin,
}

struct LoadedStoreCandidate {
    store: AuthStore,
    store_origin: LoadedStoreOrigin,
}

pub(super) struct GuardedReloadLoadedStore {
    pub(super) loaded: LoadedAuthStore,
    pub(super) removed_account_ids: Vec<String>,
}

struct GuardedReloadStorePreparation {
    loaded_had_persisted_active_account: bool,
    removed_account_ids: Vec<String>,
}

pub(super) struct TerminalRefreshFailureStoreMutation {
    pub(super) switched_to_store_account_id: Option<String>,
}

#[derive(Debug)]
pub struct AccountManager {
    codex_home: PathBuf,
    storage: Arc<dyn AuthStorageBackend>,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    pub(super) account_state_store: Option<AccountStateStore>,
    runtime_session_id: String,
    linked_codex_session_id: RwLock<Option<String>>,
    forced_chatgpt_workspace_id: RwLock<Option<String>>,
    pub(super) usage_limit_auto_switch_cooldown_until: Mutex<Option<DateTime<Utc>>>,
}

impl AccountManager {
    pub(super) fn new(
        codex_home: PathBuf,
        storage: Arc<dyn AuthStorageBackend>,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
        account_state_store: Option<AccountStateStore>,
        runtime_session_id: String,
        forced_chatgpt_workspace_id: Option<String>,
    ) -> Self {
        Self::new_with_runtime_context(
            codex_home,
            storage,
            auth_credentials_store_mode,
            account_state_store,
            runtime_session_id,
            /*linked_codex_session_id*/ None,
            forced_chatgpt_workspace_id,
        )
    }

    pub(super) fn new_with_runtime_context(
        codex_home: PathBuf,
        storage: Arc<dyn AuthStorageBackend>,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
        account_state_store: Option<AccountStateStore>,
        runtime_session_id: String,
        linked_codex_session_id: Option<String>,
        forced_chatgpt_workspace_id: Option<String>,
    ) -> Self {
        Self {
            codex_home,
            storage,
            auth_credentials_store_mode,
            account_state_store,
            runtime_session_id,
            linked_codex_session_id: RwLock::new(linked_codex_session_id),
            forced_chatgpt_workspace_id: RwLock::new(forced_chatgpt_workspace_id),
            usage_limit_auto_switch_cooldown_until: Mutex::new(None),
        }
    }

    pub fn linked_codex_session_id(&self) -> Option<String> {
        self.linked_codex_session_id
            .read()
            .ok()
            .and_then(|guard| guard.clone())
    }

    pub(super) fn set_linked_codex_session_id(
        &self,
        codex_session_id: Option<String>,
    ) -> std::io::Result<bool> {
        if let Ok(mut guard) = self.linked_codex_session_id.write() {
            if *guard == codex_session_id {
                Ok(false)
            } else {
                *guard = codex_session_id;
                Ok(true)
            }
        } else {
            Err(std::io::Error::other(
                "failed to update linked Codex session id",
            ))
        }
    }

    pub fn forced_chatgpt_workspace_id(&self) -> Option<String> {
        self.forced_chatgpt_workspace_id
            .read()
            .ok()
            .and_then(|guard| guard.clone())
    }

    pub(super) fn set_forced_chatgpt_workspace_id(&self, workspace_id: Option<String>) -> bool {
        if let Ok(mut guard) = self.forced_chatgpt_workspace_id.write()
            && *guard != workspace_id
        {
            *guard = workspace_id;
            true
        } else {
            false
        }
    }

    pub fn has_account_state_store(&self) -> bool {
        self.account_state_store.is_some()
    }

    pub fn runtime_session_id(&self) -> &str {
        self.runtime_session_id.as_str()
    }

    fn runtime_context(&self) -> AccountRuntimeContext {
        AccountRuntimeContext {
            linked_codex_session_id: self.linked_codex_session_id(),
            forced_chatgpt_workspace_id: self.forced_chatgpt_workspace_id(),
        }
    }

    pub(super) fn bootstrap_runtime_active_account_candidates(
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

    pub(super) fn hydrate_runtime_active_account_for_runtime(
        account_state_store: Option<&AccountStateStore>,
        runtime_session_id: &str,
        linked_codex_session_id: Option<&str>,
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
        // Merge-safety anchor: WS12 missing-bearer fail-loud only stays honest when a foreign
        // lease takeover blocks the stolen account itself, not the whole bootstrap pass; this
        // session must still try every other eligible saved account before surfacing no bearer.
        let mut blocked_bootstrap_account_id = None;
        let mut current_active_account_id = match account_state_store
            .refresh_session_active_account(
                runtime_session_id,
                linked_codex_session_id,
                now,
                ACTIVE_ACCOUNT_LEASE_TTL_SECONDS,
            ) {
            Ok(SessionActiveAccountRefresh::Active(account_id)) => Some(account_id),
            Ok(SessionActiveAccountRefresh::None) => None,
            Ok(SessionActiveAccountRefresh::LostToOtherSession {
                account_id,
                owner_session_id,
                owner_codex_session_id,
                lease_until,
            }) => {
                tracing::info!(
                    account_id,
                    owner_session_id,
                    owner_codex_session_id,
                    lease_until = ?lease_until,
                    "runtime session lost its active-account lease to another live session"
                );
                blocked_bootstrap_account_id = Some(account_id);
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

        if current_active_account_id.is_none() {
            let bootstrap_candidate_ids =
                Self::bootstrap_runtime_active_account_candidates(store, required_workspace_id);
            let bootstrap_candidate_count = bootstrap_candidate_ids.len();
            let mut bootstrap_conflict_count = 0usize;
            let mut bootstrap_skipped_lost_lease_count = 0usize;
            for candidate_account_id in bootstrap_candidate_ids {
                if blocked_bootstrap_account_id.as_deref() == Some(candidate_account_id.as_str()) {
                    bootstrap_skipped_lost_lease_count += 1;
                    tracing::debug!(
                        candidate_account_id,
                        "skipping runtime active-account bootstrap candidate that was just lost to another live session"
                    );
                    continue;
                }
                match account_state_store.set_session_active_account(
                    runtime_session_id,
                    linked_codex_session_id,
                    candidate_account_id.as_str(),
                    now,
                    ACTIVE_ACCOUNT_LEASE_TTL_SECONDS,
                ) {
                    Ok(SessionActiveAccountSetOutcome::Assigned) => {
                        current_active_account_id = Some(candidate_account_id);
                        break;
                    }
                    Ok(SessionActiveAccountSetOutcome::Conflict(conflict)) => {
                        bootstrap_conflict_count += 1;
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
            if current_active_account_id.is_none() {
                tracing::warn!(
                    runtime_session_id,
                    linked_codex_session_id,
                    required_workspace_id,
                    saved_account_count = store.accounts.len(),
                    bootstrap_candidate_count,
                    bootstrap_conflict_count,
                    bootstrap_skipped_lost_lease_count,
                    blocked_bootstrap_account_id,
                    "runtime active-account hydration found no eligible saved account; auth will remain bearerless for this runtime"
                );
            }
        }

        store.active_account_id = current_active_account_id;
        Ok(())
    }

    pub(super) fn hydrate_runtime_active_account(
        &self,
        store: &mut AuthStore,
    ) -> std::io::Result<()> {
        Self::hydrate_runtime_active_account_for_runtime(
            self.account_state_store.as_ref(),
            self.runtime_session_id(),
            self.linked_codex_session_id().as_deref(),
            self.forced_chatgpt_workspace_id().as_deref(),
            store,
        )
    }

    pub(super) fn reconcile_runtime_active_account(
        &self,
        store: &AuthStore,
    ) -> std::io::Result<()> {
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
                    self.runtime_session_id(),
                    self.linked_codex_session_id().as_deref(),
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
                .clear_session_active_account(self.runtime_session_id())
                .map(|_| ())
                .map_err(|error| {
                    std::io::Error::other(format!(
                        "failed to clear runtime active-account state: {error}"
                    ))
                }),
        }
    }

    pub fn force_release_account(&self, id: &str) -> std::io::Result<ForceReleaseAccountOutcome> {
        let Some(account_state_store) = self.account_state_store.as_ref() else {
            return Err(std::io::Error::other(
                "account lease management is unavailable in this auth mode",
            ));
        };
        account_state_store
            .force_release_account(id)
            .map_err(std::io::Error::other)
    }

    pub(super) fn account_ids_leased_by_other(
        &self,
        account_ids: &[String],
        now: DateTime<Utc>,
    ) -> std::io::Result<Option<HashSet<String>>> {
        let Some(account_state_store) = self.account_state_store.as_ref() else {
            return Ok(None);
        };
        account_state_store
            .account_ids_leased_by_other(
                self.runtime_session_id(),
                self.linked_codex_session_id().as_deref(),
                account_ids,
                now,
            )
            .map(Some)
            .map_err(std::io::Error::other)
    }

    fn prepare_loaded_store_candidate(
        &self,
        mut store: AuthStore,
        persist_storage: Arc<dyn AuthStorageBackend>,
        store_origin: LoadedStoreOrigin,
        admission_policy: ChatgptAuthAdmissionPolicy,
        runtime_context: &AccountRuntimeContext,
    ) -> LoadedStoreCandidate {
        let loaded_had_active_account = store.active_account_id.is_some();
        let removed_account_ids = enforce_chatgpt_auth_accounts(&mut store, admission_policy);
        let mut sync_outcome = UsageStateSyncOutcome::default();
        if let Some(account_state_store) = self.account_state_store.as_ref() {
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
        if let Err(error) = Self::hydrate_runtime_active_account_for_runtime(
            self.account_state_store.as_ref(),
            self.runtime_session_id(),
            runtime_context.linked_codex_session_id.as_deref(),
            runtime_context.forced_chatgpt_workspace_id.as_deref(),
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
            || (self.account_state_store.is_some() && loaded_had_active_account))
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

        LoadedStoreCandidate {
            store,
            store_origin,
        }
    }

    pub(super) fn chatgpt_auth_admission_policy_for_store_origin(
        store_origin: LoadedStoreOrigin,
    ) -> ChatgptAuthAdmissionPolicy {
        match store_origin {
            LoadedStoreOrigin::Persistent => ChatgptAuthAdmissionPolicy::Persisted,
            LoadedStoreOrigin::ExternalEphemeral => ChatgptAuthAdmissionPolicy::ExternalStrict,
        }
    }

    fn preflight_loaded_store_candidate(
        mut store: AuthStore,
        persist_storage: Arc<dyn AuthStorageBackend>,
        store_origin: LoadedStoreOrigin,
    ) -> LoadedStoreCandidate {
        let removed_account_ids = enforce_chatgpt_auth_accounts(
            &mut store,
            Self::chatgpt_auth_admission_policy_for_store_origin(store_origin),
        );
        if !removed_account_ids.is_empty() {
            tracing::info!(
                removed_account_ids = ?removed_account_ids,
                ?store_origin,
                "removed accounts with unsupported ChatGPT plans while preflighting auth store"
            );
            if let Err(error) = persist_storage.save(&store) {
                tracing::warn!(
                    error = %error,
                    ?store_origin,
                    "failed to persist stripped auth store after preflighting unsupported plans"
                );
            }
        }
        LoadedStoreCandidate {
            store,
            store_origin,
        }
    }

    pub(super) fn store_has_selectable_auth_material(store: &AuthStore) -> bool {
        !store.accounts.is_empty() || store.openai_api_key.is_some()
    }

    pub(super) fn configured_store_origin(&self) -> LoadedStoreOrigin {
        if self.auth_credentials_store_mode == AuthCredentialsStoreMode::Ephemeral {
            LoadedStoreOrigin::ExternalEphemeral
        } else {
            LoadedStoreOrigin::Persistent
        }
    }

    pub(super) fn load_store_from_storage(&self) -> LoadedAuthStore {
        // Merge-safety anchor: live store loading keeps AccountManager as the
        // owner of auth-store selection, admission filtering, sqlite-backed
        // usage/runtime hydration, and persisted-active stripping; AuthManager
        // remains a delegation seam for auth materialization only.
        let runtime_context = self.runtime_context();
        let external_storage =
            (self.auth_credentials_store_mode != AuthCredentialsStoreMode::Ephemeral).then(|| {
                create_auth_storage(self.codex_home.clone(), AuthCredentialsStoreMode::Ephemeral)
            });
        let persistent_store = match self.storage.load() {
            Ok(Some(store)) => store,
            Ok(None) => AuthStore::default(),
            Err(err) => {
                tracing::warn!("Failed to load auth store: {err}");
                AuthStore::default()
            }
        };
        let persistent_origin = self.configured_store_origin();
        let mut selected_store = persistent_store;
        let mut selected_storage = Arc::clone(&self.storage);
        let mut selected_origin = persistent_origin;
        if let Some(external_storage) = external_storage.as_ref() {
            match external_storage.load() {
                Ok(Some(store)) if !store.accounts.is_empty() || store.openai_api_key.is_some() => {
                    let external_candidate = Self::preflight_loaded_store_candidate(
                        store,
                        Arc::clone(external_storage),
                        LoadedStoreOrigin::ExternalEphemeral,
                    );
                    if Self::store_has_selectable_auth_material(&external_candidate.store) {
                        selected_store = external_candidate.store;
                        selected_storage = Arc::clone(external_storage);
                        selected_origin = external_candidate.store_origin;
                    }
                }
                Ok(Some(_)) | Ok(None) => {}
                Err(err) => {
                    tracing::warn!("Failed to load external auth store: {err}");
                }
            }
        }
        let selected = self.prepare_loaded_store_candidate(
            selected_store,
            selected_storage,
            selected_origin,
            Self::chatgpt_auth_admission_policy_for_store_origin(selected_origin),
            &runtime_context,
        );

        LoadedAuthStore {
            store: selected.store,
            store_origin: selected.store_origin,
        }
    }

    pub fn has_saved_chatgpt_accounts(&self) -> bool {
        // Merge-safety anchor: saved-account presence checks must read the same
        // runtime-prepared store snapshot owner as `/accounts` and autoswitch,
        // not a stale auth/cache follower.
        !self.load_store_from_storage().store.accounts.is_empty()
    }

    pub(super) fn active_chatgpt_account_summary(
        &self,
        store: &AuthStore,
        store_origin: LoadedStoreOrigin,
    ) -> Option<ActiveChatgptAccountSummary> {
        // Merge-safety anchor: active ChatGPT account summaries must come from
        // the same runtime-prepared store snapshot owner as saved-account
        // presence and autoswitch, not a stale auth-cache follower.
        let active_account = store.active_account()?;
        let auth_mode = match store_origin {
            LoadedStoreOrigin::Persistent => ApiAuthMode::Chatgpt,
            LoadedStoreOrigin::ExternalEphemeral => ApiAuthMode::ChatgptAuthTokens,
        };
        Some(ActiveChatgptAccountSummary {
            store_account_id: active_account.id.clone(),
            label: active_account.label.clone(),
            email: active_account.tokens.id_token.email.clone(),
            auth_mode,
        })
    }

    pub(super) fn load_store_for_strict_reload(&self) -> std::io::Result<LoadedAuthStore> {
        // Merge-safety anchor: strict reload lock/load/account-runtime
        // prep/persist belongs to AccountManager; AuthManager may only derive
        // and cache auth from the returned snapshot.
        let _lock = lock_auth_store(&self.codex_home)?;
        let store = self.storage.load()?.unwrap_or_default();
        let store = self.prepare_strict_loaded_store(
            store,
            &*self.storage,
            ChatgptAuthAdmissionPolicy::Persisted,
        )?;
        Ok(LoadedAuthStore {
            store,
            store_origin: self.configured_store_origin(),
        })
    }

    fn prepare_strict_loaded_store(
        &self,
        mut store: AuthStore,
        persist_storage: &dyn AuthStorageBackend,
        admission_policy: ChatgptAuthAdmissionPolicy,
    ) -> std::io::Result<AuthStore> {
        // Merge-safety anchor: strict reload persisted-account prep,
        // current-runtime hydration, and fail-loud persistence stay owned by
        // AccountManager instead of the AuthManager cache facade.
        let loaded_had_persisted_active_account = store.active_account_id.is_some();
        let removed_account_ids = enforce_chatgpt_auth_accounts(&mut store, admission_policy);
        let mut sync_outcome = UsageStateSyncOutcome::default();
        if let Some(account_state_store) = self.account_state_store.as_ref() {
            sync_outcome =
                synchronize_store_usage_from_legacy_and_sqlite(account_state_store, &mut store)?;
        }
        self.hydrate_runtime_active_account(&mut store)?;
        if sync_outcome.strip_persisted_auth_store
            || !removed_account_ids.is_empty()
            || (self.has_account_state_store() && loaded_had_persisted_active_account)
        {
            persist_auth_store(
                persist_storage,
                &store,
                sync_outcome.strip_persisted_auth_store,
            )?;
        }
        Ok(store)
    }

    pub(super) fn load_store_for_guarded_reload(&self) -> Option<GuardedReloadLoadedStore> {
        // Merge-safety anchor: guarded reload lock/load/account-runtime
        // prep/persist belongs to AccountManager; AuthManager may only compare,
        // derive, and cache auth from the returned snapshot.
        let _lock = match lock_auth_store(&self.codex_home) {
            Ok(lock) => lock,
            Err(error) => {
                tracing::warn!(
                    "Skipping auth reload because auth store lock could not be acquired: {error}"
                );
                return None;
            }
        };
        let mut store = match self.storage.load() {
            Ok(Some(store)) => store,
            Ok(None) => {
                tracing::info!("Skipping auth reload because auth store is missing.");
                return None;
            }
            Err(error) => {
                tracing::warn!(
                    "Skipping auth reload because auth store could not be loaded: {error}"
                );
                return None;
            }
        };
        let prepared = match self.prepare_guarded_reload_store(&mut store) {
            Ok(prepared) => prepared,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "skipping guarded auth reload because runtime active-account state could not be hydrated"
                );
                return None;
            }
        };
        if (prepared.loaded_had_persisted_active_account
            || !prepared.removed_account_ids.is_empty())
            && let Err(error) = self.persist_guarded_reload_store(&store)
        {
            tracing::warn!(
                error = %error,
                "failed to persist auth store during guarded auth reload"
            );
            return None;
        }
        Some(GuardedReloadLoadedStore {
            loaded: LoadedAuthStore {
                store,
                store_origin: self.configured_store_origin(),
            },
            removed_account_ids: prepared.removed_account_ids,
        })
    }

    fn persist_guarded_reload_store(&self, store: &AuthStore) -> std::io::Result<()> {
        store.validate()?;
        if self.has_account_state_store() {
            let mut stripped_store = store.clone();
            strip_runtime_active_account_from_store(&mut stripped_store);
            self.storage.save(&stripped_store)
        } else {
            self.storage.save(store)
        }
    }

    fn prepare_guarded_reload_store(
        &self,
        store: &mut AuthStore,
    ) -> std::io::Result<GuardedReloadStorePreparation> {
        // Merge-safety anchor: guarded reload persisted-account filtering and
        // current-runtime active-account hydration stay owned by AccountManager
        // instead of the AuthManager cache facade.
        let loaded_had_persisted_active_account = store.active_account_id.is_some();
        let removed_account_ids =
            enforce_chatgpt_auth_accounts(store, ChatgptAuthAdmissionPolicy::Persisted);
        if !removed_account_ids.is_empty() {
            tracing::info!(
                removed_account_ids = ?removed_account_ids,
                "removed accounts with unsupported ChatGPT plans during guarded auth reload"
            );
        }
        self.hydrate_runtime_active_account(store)?;
        Ok(GuardedReloadStorePreparation {
            loaded_had_persisted_active_account,
            removed_account_ids,
        })
    }

    pub(super) fn mutate_store<T>(
        &self,
        mutator: impl FnOnce(&mut AuthStore) -> std::io::Result<T>,
    ) -> std::io::Result<(T, LoadedAuthStore)> {
        // Merge-safety anchor: account-runtime mutations own the auth-store
        // lock/load/persist transaction here; AuthManager may only refresh its
        // derived auth cache from the returned selected store snapshot.
        let _lock = lock_auth_store(&self.codex_home)?;
        let mut store = self.storage.load()?.unwrap_or_default();
        let out = self.mutate_loaded_store(&mut store, &*self.storage, mutator)?;
        Ok((
            out,
            LoadedAuthStore {
                store,
                store_origin: self.configured_store_origin(),
            },
        ))
    }

    pub(super) fn remove_store_account_after_terminal_refresh_failure_from_store_origin(
        &self,
        store_account_id: &str,
        store_origin: LoadedStoreOrigin,
    ) -> std::io::Result<Option<(TerminalRefreshFailureStoreMutation, LoadedAuthStore)>> {
        // Merge-safety anchor: terminal refresh-token account eviction is an
        // account-runtime mutation transaction against the selected store origin;
        // keep auth-store lock/load/persist here and let AuthManager only refresh
        // derived auth from the returned snapshot.
        let storage = match store_origin {
            LoadedStoreOrigin::Persistent => Arc::clone(&self.storage),
            LoadedStoreOrigin::ExternalEphemeral
                if self.auth_credentials_store_mode == AuthCredentialsStoreMode::Ephemeral =>
            {
                Arc::clone(&self.storage)
            }
            LoadedStoreOrigin::ExternalEphemeral => {
                create_auth_storage(self.codex_home.clone(), AuthCredentialsStoreMode::Ephemeral)
            }
        };
        let admission_policy = Self::chatgpt_auth_admission_policy_for_store_origin(store_origin);
        let _lock = lock_auth_store(&self.codex_home)?;
        let mut store = storage.load()?.unwrap_or_default();
        let Some(mutation) = self.remove_store_account_after_terminal_refresh_failure(
            &mut store,
            &*storage,
            store_account_id,
            admission_policy,
        )?
        else {
            return Ok(None);
        };
        Ok(Some((
            mutation,
            LoadedAuthStore {
                store,
                store_origin,
            },
        )))
    }

    pub(super) fn mutate_loaded_store<T>(
        &self,
        store: &mut AuthStore,
        persist_storage: &dyn AuthStorageBackend,
        mutator: impl FnOnce(&mut AuthStore) -> std::io::Result<T>,
    ) -> std::io::Result<T> {
        // Merge-safety anchor: AccountManager owns saved-account mutation prep,
        // active-lease reconciliation, validation, and persistence for any
        // caller-provided loaded store.
        let removed_before_mutation =
            enforce_chatgpt_auth_accounts(store, ChatgptAuthAdmissionPolicy::Persisted);
        if !removed_before_mutation.is_empty() {
            tracing::info!(
                removed_account_ids = ?removed_before_mutation,
                "removed accounts with unsupported ChatGPT plans before auth store mutation"
            );
        }
        if let Some(account_state_store) = self.account_state_store.as_ref() {
            synchronize_store_usage_from_legacy_and_sqlite(account_state_store, store)?;
        }
        self.hydrate_runtime_active_account(store)?;

        let out = mutator(store)?;
        let removed_after_mutation =
            enforce_chatgpt_auth_accounts(store, ChatgptAuthAdmissionPolicy::Persisted);
        if !removed_after_mutation.is_empty() {
            tracing::info!(
                removed_account_ids = ?removed_after_mutation,
                "removed accounts with unsupported ChatGPT plans after auth store mutation"
            );
        }
        self.reconcile_runtime_active_account(store)?;
        store.validate()?;
        if let Some(account_state_store) = self.account_state_store.as_ref() {
            persist_usage_state_from_store(account_state_store, store)?;
            persist_stripped_auth_store(persist_storage, store)?;
        } else {
            persist_storage.save(store)?;
        }
        Ok(out)
    }

    pub(super) fn remove_store_account_after_terminal_refresh_failure(
        &self,
        store: &mut AuthStore,
        persist_storage: &dyn AuthStorageBackend,
        store_account_id: &str,
        admission_policy: ChatgptAuthAdmissionPolicy,
    ) -> std::io::Result<Option<TerminalRefreshFailureStoreMutation>> {
        // Merge-safety anchor: terminal refresh-token eviction is AccountManager-owned
        // for the caller-provided store; keep admission-policy enforcement, fallback
        // selection, lease diagnostics, validation, and persistence here. The selected
        // store-origin wrapper owns lock/load and AuthManager only consumes its returned
        // snapshot for derived auth/cache refresh.
        let removed_before_mutation = enforce_chatgpt_auth_accounts(store, admission_policy);
        if !removed_before_mutation.is_empty() {
            tracing::info!(
                removed_account_ids = ?removed_before_mutation,
                "removed accounts with unsupported ChatGPT plans before terminal refresh-token eviction"
            );
        }
        self.hydrate_runtime_active_account(store)?;

        let was_active = store.active_account_id.as_deref() == Some(store_account_id);
        let previous_account_count = store.accounts.len();
        store
            .accounts
            .retain(|account| account.id != store_account_id);
        if store.accounts.len() == previous_account_count {
            return Ok(None);
        }

        let required_workspace_id = self.forced_chatgpt_workspace_id();
        let switched_to_store_account_id = if was_active {
            let selection_now = Utc::now();
            let next_store_account_id = self.select_account_for_auto_switch_with_leases(
                store,
                required_workspace_id.as_deref(),
                /*exclude_store_account_id*/ None,
                selection_now,
                UsageLimitAutoSwitchSelectionScope::PersistedTruth,
            );
            if next_store_account_id.is_none() {
                let fallback_workspace_candidate_count = store
                    .accounts
                    .iter()
                    .filter(|account| {
                        account_matches_required_workspace(
                            account,
                            required_workspace_id.as_deref(),
                        )
                    })
                    .count();
                let fallback_selectable_candidate_count = store
                    .accounts
                    .iter()
                    .filter(|account| {
                        account_selectable_for_auto_switch(
                            account,
                            required_workspace_id.as_deref(),
                            selection_now,
                            UsageLimitAutoSwitchSelectionScope::PersistedTruth,
                        )
                    })
                    .count();
                let fallback_exhausted_candidate_count = store
                    .accounts
                    .iter()
                    .filter(|account| {
                        account_matches_required_workspace(
                            account,
                            required_workspace_id.as_deref(),
                        ) && account
                            .usage
                            .as_ref()
                            .and_then(|usage| usage.exhausted_until)
                            .is_some_and(|until| until > selection_now)
                    })
                    .count();
                let (fallback_leased_by_other_count, fallback_lease_state_unavailable_count) =
                    if let Some(account_state_store) = self.account_state_store.as_ref() {
                        store
                            .accounts
                            .iter()
                            .filter(|account| {
                                account_matches_required_workspace(
                                    account,
                                    required_workspace_id.as_deref(),
                                )
                            })
                            .fold((0usize, 0usize), |(leased, unavailable), account| {
                                match account_state_store.account_is_leased_by_other(
                                    self.runtime_session_id.as_str(),
                                    self.linked_codex_session_id().as_deref(),
                                    account.id.as_str(),
                                    selection_now,
                                ) {
                                    Ok(true) => (leased + 1, unavailable),
                                    Ok(false) => (leased, unavailable),
                                    Err(_) => (leased, unavailable + 1),
                                }
                            })
                    } else {
                        (0usize, fallback_workspace_candidate_count)
                    };
                tracing::warn!(
                    failed_store_account_id = store_account_id,
                    runtime_session_id = self.runtime_session_id,
                    linked_codex_session_id = ?self.linked_codex_session_id().as_deref(),
                    required_workspace_id = ?required_workspace_id,
                    remaining_saved_account_count = store.accounts.len(),
                    fallback_workspace_candidate_count,
                    fallback_selectable_candidate_count,
                    fallback_exhausted_candidate_count,
                    fallback_leased_by_other_count,
                    fallback_lease_state_unavailable_count,
                    "terminal refresh-token failure removed the active account without selecting a fallback; auth will become bearerless"
                );
            }
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

        let removed_after_mutation = enforce_chatgpt_auth_accounts(store, admission_policy);
        if !removed_after_mutation.is_empty() {
            tracing::info!(
                removed_account_ids = ?removed_after_mutation,
                "removed accounts with unsupported ChatGPT plans after terminal refresh-token eviction"
            );
        }

        self.reconcile_runtime_active_account(store)?;
        store.validate()?;
        if let Some(account_state_store) = self.account_state_store.as_ref() {
            persist_usage_state_from_store(account_state_store, store)?;
            persist_stripped_auth_store(persist_storage, store)?;
        } else {
            persist_storage.save(store)?;
        }

        Ok(Some(TerminalRefreshFailureStoreMutation {
            switched_to_store_account_id,
        }))
    }

    pub fn account_rate_limit_refresh_roster(&self) -> AccountRateLimitRefreshRoster {
        // Merge-safety anchor: rate-limit refresh rosters must use the current
        // AccountManager-loaded runtime snapshot, not the AuthManager cache, so
        // pre-refresh candidates track live saved accounts and leases.
        let store = self.load_store_from_storage().store;
        let required_workspace_id = self.forced_chatgpt_workspace_id();
        let workspace_filtered_store_account_ids =
            workspace_filtered_store_account_ids(&store, required_workspace_id.as_deref());

        match self.account_ids_leased_by_other(&workspace_filtered_store_account_ids, Utc::now()) {
            Ok(Some(leased_by_other_store_account_ids)) => {
                account_rate_limit_refresh_roster_from_workspace_filtered_ids(
                    workspace_filtered_store_account_ids,
                    AccountRateLimitRefreshLeaseState::LeaseManaged(
                        &leased_by_other_store_account_ids,
                    ),
                )
            }
            Ok(None) => account_rate_limit_refresh_roster_from_workspace_filtered_ids(
                workspace_filtered_store_account_ids,
                AccountRateLimitRefreshLeaseState::NoLeaseOwner,
            ),
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "failed to load lease-aware rate-limit refresh roster"
                );
                account_rate_limit_refresh_roster_from_workspace_filtered_ids(
                    workspace_filtered_store_account_ids,
                    AccountRateLimitRefreshLeaseState::LeaseReadFailed,
                )
            }
        }
    }

    pub(super) fn account_lease_states(
        &self,
        store: &AuthStore,
    ) -> HashMap<String, AccountLeaseState> {
        let account_ids = store
            .accounts
            .iter()
            .map(|account| account.id.clone())
            .collect::<Vec<_>>();
        let mut lease_states = HashMap::with_capacity(account_ids.len());
        let active_account_id = store.active_account_id.as_deref();
        let foreign_leases = self.account_ids_leased_by_other(&account_ids, Utc::now());

        for account_id in account_ids {
            let lease_state = match foreign_leases.as_ref() {
                Ok(Some(foreign_leases)) if foreign_leases.contains(account_id.as_str()) => {
                    AccountLeaseState::LeasedByOtherSession
                }
                Ok(Some(_)) if active_account_id == Some(account_id.as_str()) => {
                    AccountLeaseState::LeasedByCurrentSession
                }
                Ok(Some(_)) => AccountLeaseState::NotLeased,
                Err(_) if active_account_id == Some(account_id.as_str()) => {
                    AccountLeaseState::LeasedByCurrentSession
                }
                Err(_) | Ok(None) => AccountLeaseState::Unavailable,
            };
            lease_states.insert(account_id, lease_state);
        }

        lease_states
    }

    pub(super) fn select_account_for_auto_switch_with_leases(
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
                self.runtime_session_id(),
                self.linked_codex_session_id().as_deref(),
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
        let store = self.load_store_from_storage().store;
        self.select_account_for_auto_switch_with_leases(
            &store,
            required_workspace_id,
            exclude_store_account_id,
            Utc::now(),
            UsageLimitAutoSwitchSelectionScope::PersistedTruth,
        )
    }

    fn update_saved_account_store<T>(
        &self,
        default: T,
        mutator: impl FnOnce(&mut AuthStore) -> std::io::Result<T>,
    ) -> std::io::Result<T> {
        // Merge-safety anchor: AccountManager-owned telemetry mutations must
        // keep this no-saved-accounts early return outside the persistence path
        // so API-key or empty stores are not loaded, persisted, or treated as
        // account-runtime state just because usage data arrived.
        if !self.has_saved_chatgpt_accounts() {
            return Ok(default);
        }
        let (out, _loaded) = self.mutate_store(mutator)?;
        Ok(out)
    }

    fn update_saved_account_store_from_map<T, V>(
        &self,
        default: T,
        entries: impl IntoIterator<Item = (String, V)>,
        mutator: impl FnOnce(&mut AuthStore, &mut HashMap<String, V>) -> std::io::Result<T>,
    ) -> std::io::Result<T> {
        // Merge-safety anchor: collection-backed telemetry mutations must keep
        // the no-saved-accounts early return before consuming the caller's
        // iterator; API-key or empty stores must not be loaded, persisted, or
        // drained by account-runtime follower updates.
        if !self.has_saved_chatgpt_accounts() {
            return Ok(default);
        }

        let mut entries = entries.into_iter().collect::<HashMap<_, _>>();
        if entries.is_empty() {
            return Ok(default);
        }

        let (out, _loaded) = self.mutate_store(|store| mutator(store, &mut entries))?;
        Ok(out)
    }

    pub fn update_usage_for_active(&self, snapshot: RateLimitSnapshot) -> std::io::Result<()> {
        // Merge-safety anchor: usage telemetry writes are AccountManager-owned
        // account-runtime mutations and intentionally do not refresh
        // AuthManager's derived auth cache because they do not change selected
        // credentials.
        self.update_saved_account_store((), |store| {
            Self::update_usage_for_active_in_store(store, snapshot)
        })
    }

    pub fn update_rate_limits_for_account(
        &self,
        store_account_id: &str,
        snapshot: RateLimitSnapshot,
    ) -> std::io::Result<()> {
        self.update_saved_account_store((), |store| {
            Self::update_rate_limits_for_account_in_store(store, store_account_id, snapshot)
        })
    }

    pub fn update_rate_limits_for_accounts(
        &self,
        updates: impl IntoIterator<Item = (String, RateLimitSnapshot)>,
    ) -> std::io::Result<usize> {
        self.update_saved_account_store_from_map(0, updates, |store, updates| {
            Ok(Self::update_rate_limits_for_accounts_in_store(
                store, updates,
            ))
        })
    }

    pub fn reconcile_account_rate_limit_refresh_outcomes(
        &self,
        outcomes: impl IntoIterator<Item = (String, AccountRateLimitRefreshOutcome)>,
    ) -> std::io::Result<usize> {
        self.update_saved_account_store_from_map(0, outcomes, |store, outcomes| {
            Ok(Self::reconcile_account_rate_limit_refresh_outcomes_in_store(store, outcomes))
        })
    }

    pub fn mark_usage_limit_reached(
        &self,
        resets_at: Option<DateTime<Utc>>,
        snapshot: Option<RateLimitSnapshot>,
    ) -> std::io::Result<()> {
        self.update_saved_account_store((), |store| {
            Self::mark_usage_limit_reached_in_store(store, resets_at, snapshot)
        })
    }

    pub(super) fn set_active_account(
        &self,
        store: &mut AuthStore,
        id: &str,
    ) -> std::io::Result<()> {
        let required_workspace_id = self.forced_chatgpt_workspace_id();
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
    }

    pub(super) fn upsert_account(
        &self,
        store: &mut AuthStore,
        tokens: TokenData,
        label: Option<String>,
        make_active: bool,
    ) -> String {
        let account_id = tokens
            .preferred_store_account_id()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
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
        account_id
    }

    pub(super) fn remove_account(&self, store: &mut AuthStore, id: &str) -> std::io::Result<bool> {
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
    }

    pub fn list_accounts(&self) -> Vec<AccountSummary> {
        let store = self.load_store_from_storage().store;
        let active_account_id = store.active_account_id.as_deref();
        let lease_states = self.account_lease_states(&store);
        store
            .accounts
            .iter()
            .map(|account| {
                let lease_state = lease_states
                    .get(account.id.as_str())
                    .copied()
                    .unwrap_or(AccountLeaseState::Unavailable);
                AccountSummary::from_stored(account, active_account_id, lease_state)
            })
            .collect()
    }

    fn reconcile_account_rate_limit_refresh_outcomes_in_store(
        store: &mut AuthStore,
        outcomes: &mut HashMap<String, AccountRateLimitRefreshOutcome>,
    ) -> usize {
        let now = Utc::now();
        let mut updated = 0usize;
        for account in &mut store.accounts {
            if let Some(outcome) = outcomes.remove(&account.id) {
                let usage = account.usage.get_or_insert_with(AccountUsageCache::default);
                apply_rate_limit_refresh_outcome(usage, outcome, now);
                updated = updated.saturating_add(1);
            }
        }
        updated
    }

    fn mark_usage_limit_reached_in_store(
        store: &mut AuthStore,
        resets_at: Option<DateTime<Utc>>,
        snapshot: Option<RateLimitSnapshot>,
    ) -> std::io::Result<()> {
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
    }

    fn update_usage_for_active_in_store(
        store: &mut AuthStore,
        snapshot: RateLimitSnapshot,
    ) -> std::io::Result<()> {
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
    }

    fn update_rate_limits_for_account_in_store(
        store: &mut AuthStore,
        store_account_id: &str,
        snapshot: RateLimitSnapshot,
    ) -> std::io::Result<()> {
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
    }

    fn update_rate_limits_for_accounts_in_store(
        store: &mut AuthStore,
        updates: &mut HashMap<String, RateLimitSnapshot>,
    ) -> usize {
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
        updated
    }

    pub fn accounts_rate_limits_cache_expires_at(
        &self,
        now: DateTime<Utc>,
    ) -> Option<DateTime<Utc>> {
        let store = self.load_store_from_storage().store;
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

    pub(super) fn switch_account_on_usage_limit_with_cooldown(
        &self,
        request: &UsageLimitAutoSwitchRequest<'_>,
        switch_store: impl FnOnce(bool) -> std::io::Result<Option<String>>,
    ) -> std::io::Result<Option<String>> {
        // Merge-safety anchor: usage-limit auto-switch cooldown orchestration
        // belongs to AccountManager, and this method must keep the historical
        // lock order of cooldown mutex before the account-store mutation
        // transaction.
        let cooldown_check_now = Utc::now();
        let mut cooldown_until = self
            .usage_limit_auto_switch_cooldown_until
            .lock()
            .map_err(|_| std::io::Error::other("auto-switch cooldown lock poisoned"))?;
        let cooldown_active = cooldown_until.is_some_and(|until| until > cooldown_check_now);
        if cooldown_active {
            tracing::debug!(
                cooldown_until = ?cooldown_until,
                "skipping usage-limit auto-switch during cooldown"
            );
        }

        let switched_to = switch_store(cooldown_active)?;
        let should_start_cooldown = switched_to
            .as_deref()
            .is_some_and(|switched_to| Some(switched_to) != request.protected_store_account_id);
        if !should_start_cooldown {
            return Ok(switched_to);
        }

        let cooldown_started_at = Utc::now();
        *cooldown_until = Some(
            cooldown_started_at
                + chrono::Duration::seconds(USAGE_LIMIT_AUTO_SWITCH_COOLDOWN_SECONDS),
        );
        Ok(switched_to)
    }

    pub(super) fn switch_account_on_usage_limit(
        &self,
        store: &mut AuthStore,
        request: &UsageLimitAutoSwitchRequest<'_>,
        cooldown_active: bool,
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

        let mutation_now = Utc::now();
        let failing_store_account_id = match failing_store_account_id {
            Some(store_account_id) => {
                if store
                    .accounts
                    .iter()
                    .any(|account| account.id == *store_account_id)
                {
                    Some((*store_account_id).to_string())
                } else {
                    return Ok(None);
                }
            }
            None => store.active_account_id.clone(),
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
            *resets_at,
            usage.last_rate_limits.as_ref(),
            mutation_now,
        ));

        // Merge-safety anchor: usage-limit auto-switch must purge fallback accounts only when
        // the shared pre-refresh fan-in already proved an explicit unsupported plan; ambiguous
        // `Unknown` accounts must stay fail-soft in saved accounts instead of being erased.
        let removed_fallback_account_ids = store
            .accounts
            .iter()
            .filter(|account| {
                account.id != failing_store_account_id
                    && Some(account.id.as_str()) != *protected_store_account_id
                    && freshly_unsupported_store_account_ids.contains(&account.id)
                    && account_matches_required_workspace(account, *required_workspace_id)
            })
            .map(|account| account.id.clone())
            .collect::<Vec<_>>();
        if !removed_fallback_account_ids.is_empty() {
            store.accounts.retain(|account| {
                account.id == failing_store_account_id
                    || Some(account.id.as_str()) == *protected_store_account_id
                    || !freshly_unsupported_store_account_ids.contains(&account.id)
                    || !account_matches_required_workspace(account, *required_workspace_id)
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
                account.id == *protected_store_account_id
                    && account_selectable_for_auto_switch(
                        account,
                        *required_workspace_id,
                        mutation_now,
                        *selection_scope,
                    )
            })
        {
            store.active_account_id = Some((*protected_store_account_id).to_string());
            if let Some(next_account) = store
                .accounts
                .iter_mut()
                .find(|account| account.id == *protected_store_account_id)
            {
                let usage = next_account
                    .usage
                    .get_or_insert_with(AccountUsageCache::default);
                usage.last_seen_at = Some(mutation_now);
            }
            return Ok(Some((*protected_store_account_id).to_string()));
        }

        if *fallback_selection_mode
            == UsageLimitAutoSwitchFallbackSelectionMode::CancelStaleRequestFallbackSelection
        {
            return Ok(None);
        }

        let Some(next_account_id) = self.select_account_for_auto_switch_with_leases(
            store,
            *required_workspace_id,
            Some(failing_store_account_id.as_str()),
            mutation_now,
            *selection_scope,
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
    }

    pub fn release_runtime_active_account(&self) -> std::io::Result<()> {
        let Some(account_state_store) = self.account_state_store.as_ref() else {
            return Ok(());
        };
        account_state_store
            .clear_session_active_account(self.runtime_session_id())
            .map(|_| ())
            .map_err(std::io::Error::other)
    }
}

impl Drop for AccountManager {
    fn drop(&mut self) {
        // Merge-safety anchor: WS12 runtime session leases are AccountManager-owned
        // runtime state and must be released with the account-runtime owner, not
        // with the auth/token facade.
        if let Err(error) = self.release_runtime_active_account() {
            tracing::warn!(
                error = %error,
                runtime_session_id = self.runtime_session_id(),
                "failed to clear runtime active-account state while dropping account manager"
            );
        }
    }
}

/// Merge-safety anchor: `/accounts` and `/logout` render this exact
/// AccountManager-owned summary through the current AuthManager facade; keep
/// field semantics aligned with TUI account flows.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountSummary {
    pub id: String,
    pub label: Option<String>,
    pub email: Option<String>,
    pub is_active: bool,
    pub exhausted_until: Option<DateTime<Utc>>,
    pub last_rate_limits: Option<RateLimitSnapshot>,
    pub lease_state: AccountLeaseState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountLeaseState {
    NotLeased,
    LeasedByCurrentSession,
    LeasedByOtherSession,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountRateLimitRefreshRosterStatus {
    LeaseManaged,
    NoLeaseOwner,
    LeaseReadFailed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountRateLimitRefreshRoster {
    pub store_account_ids: Vec<String>,
    pub status: AccountRateLimitRefreshRosterStatus,
}

impl AccountSummary {
    pub(super) fn from_stored(
        account: &StoredAccount,
        active_id: Option<&str>,
        lease_state: AccountLeaseState,
    ) -> Self {
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
            lease_state,
        }
    }
}

enum AccountRateLimitRefreshLeaseState<'a> {
    LeaseManaged(&'a HashSet<String>),
    NoLeaseOwner,
    LeaseReadFailed,
}

fn workspace_filtered_store_account_ids(
    store: &AuthStore,
    required_workspace_id: Option<&str>,
) -> Vec<String> {
    store
        .accounts
        .iter()
        .filter(|account| account_matches_required_workspace(account, required_workspace_id))
        .map(|account| account.id.clone())
        .collect()
}

fn account_rate_limit_refresh_roster_from_workspace_filtered_ids(
    workspace_filtered_store_account_ids: Vec<String>,
    lease_state: AccountRateLimitRefreshLeaseState<'_>,
) -> AccountRateLimitRefreshRoster {
    let status = match lease_state {
        AccountRateLimitRefreshLeaseState::LeaseManaged(_) => {
            AccountRateLimitRefreshRosterStatus::LeaseManaged
        }
        AccountRateLimitRefreshLeaseState::NoLeaseOwner => {
            AccountRateLimitRefreshRosterStatus::NoLeaseOwner
        }
        AccountRateLimitRefreshLeaseState::LeaseReadFailed => {
            AccountRateLimitRefreshRosterStatus::LeaseReadFailed
        }
    };
    let store_account_ids = match lease_state {
        AccountRateLimitRefreshLeaseState::LeaseManaged(leased_by_other_store_account_ids) => {
            workspace_filtered_store_account_ids
                .into_iter()
                .filter(|account_id| !leased_by_other_store_account_ids.contains(account_id))
                .collect()
        }
        AccountRateLimitRefreshLeaseState::NoLeaseOwner => workspace_filtered_store_account_ids,
        AccountRateLimitRefreshLeaseState::LeaseReadFailed => Vec::new(),
    };
    AccountRateLimitRefreshRoster {
        store_account_ids,
        status,
    }
}
