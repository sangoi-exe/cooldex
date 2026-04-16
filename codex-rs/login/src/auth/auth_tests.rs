use super::*;
use crate::auth::storage::FileAuthStorage;
use crate::auth::storage::get_auth_file;
use crate::token_data::IdTokenInfo;
use async_trait::async_trait;
use codex_account_state::AccountStateStore;
use codex_account_state::accounts_db_path;
use codex_app_server_protocol::AuthMode;
use codex_protocol::account::PlanType as AccountPlanType;
use codex_protocol::auth::KnownPlan as InternalKnownPlan;
use codex_protocol::auth::PlanType as InternalPlanType;
use pretty_assertions::assert_eq;

use base64::Engine;
use codex_protocol::config_types::ForcedLoginMethod;
use codex_protocol::config_types::ModelProviderAuthInfo;
use rusqlite::Connection;
use serde::Serialize;
use serde_json::json;
use std::collections::HashSet;
use std::sync::Arc;
use tempfile::TempDir;
use tempfile::tempdir;

// Merge-safety anchor: auth test fixtures must keep workspace account/token contracts aligned
// with the customized ChatGPT account persistence and refresh semantics.

struct FailingExternalChatgptAuth {
    error: RefreshTokenFailedError,
}

#[async_trait]
impl ExternalAuth for FailingExternalChatgptAuth {
    fn auth_mode(&self) -> AuthMode {
        AuthMode::Chatgpt
    }

    async fn refresh(
        &self,
        _context: ExternalAuthRefreshContext,
    ) -> std::io::Result<ExternalAuthTokens> {
        Err(std::io::Error::other(self.error.clone()))
    }
}

#[tokio::test]
async fn refresh_without_id_token() {
    let codex_home = tempdir().unwrap();
    let store_account_id = "chatgpt-user:user-12345";
    let fake_jwt = write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("pro".to_string()),
            chatgpt_account_id: None,
        },
        codex_home.path(),
    )
    .expect("failed to write auth file");

    let storage = create_auth_storage(
        codex_home.path().to_path_buf(),
        AuthCredentialsStoreMode::File,
    );
    let updated = super::update_tokens(
        codex_home.path(),
        &storage,
        store_account_id,
        /*id_token*/ None,
        Some("new-access-token".to_string()),
        Some("new-refresh-token".to_string()),
        super::PersistedActiveAccountWriteMode::Preserve,
    )
    .await
    .expect("update_tokens should succeed");

    assert_eq!(updated.active_account_id.as_deref(), Some(store_account_id));
    let tokens = updated
        .accounts
        .into_iter()
        .find(|account| account.id == store_account_id)
        .expect("updated account should exist")
        .tokens;
    assert_eq!(tokens.id_token.raw_jwt, fake_jwt);
    assert_eq!(tokens.access_token, "new-access-token");
    assert_eq!(tokens.refresh_token, "new-refresh-token");
}

#[tokio::test]
async fn update_tokens_strip_mode_keeps_persisted_active_account_cleared() {
    let codex_home = tempdir().unwrap();
    let store_account_id = "chatgpt-user:user-12345";
    write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("pro".to_string()),
            chatgpt_account_id: None,
        },
        codex_home.path(),
    )
    .expect("failed to write auth file");

    let storage = create_auth_storage(
        codex_home.path().to_path_buf(),
        AuthCredentialsStoreMode::File,
    );
    let updated = super::update_tokens(
        codex_home.path(),
        &storage,
        store_account_id,
        /*id_token*/ None,
        Some("new-access-token".to_string()),
        Some("new-refresh-token".to_string()),
        super::PersistedActiveAccountWriteMode::Strip,
    )
    .await
    .expect("update_tokens should succeed");

    assert_eq!(updated.active_account_id.as_deref(), Some(store_account_id));
    let persisted_store = load_auth_store(codex_home.path(), AuthCredentialsStoreMode::File)
        .expect("load persisted auth store")
        .expect("auth store should exist");
    assert_eq!(persisted_store.active_account_id, None);
}

#[test]
fn login_with_api_key_overwrites_existing_auth_json() {
    let dir = tempdir().unwrap();
    let auth_path = dir.path().join("auth.json");
    let stale_auth = json!({
        "OPENAI_API_KEY": "sk-old",
        "tokens": {
            "id_token": "stale.header.payload",
            "access_token": "stale-access",
            "refresh_token": "stale-refresh",
            "account_id": "stale-acc"
        }
    });
    std::fs::write(
        &auth_path,
        serde_json::to_string_pretty(&stale_auth).unwrap(),
    )
    .unwrap();

    super::login_with_api_key(dir.path(), "sk-new", AuthCredentialsStoreMode::File)
        .expect("login_with_api_key should succeed");

    let storage = FileAuthStorage::new(dir.path().to_path_buf());
    let auth = storage
        .try_read_auth_store(&auth_path)
        .expect("auth.json should parse");
    assert_eq!(auth.openai_api_key.as_deref(), Some("sk-new"));
    assert!(
        auth.accounts.is_empty(),
        "ChatGPT accounts should be cleared from the auth store"
    );
    assert_eq!(auth.active_account_id, None);
}

#[test]
fn missing_auth_json_returns_none() {
    let dir = tempdir().unwrap();
    let auth = CodexAuth::from_auth_storage(dir.path(), AuthCredentialsStoreMode::File)
        .expect("call should succeed");
    assert_eq!(auth, None);
}

#[tokio::test]
#[serial(codex_api_key)]
async fn pro_account_with_no_api_key_uses_chatgpt_auth() {
    let codex_home = tempdir().unwrap();
    let fake_jwt = write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("pro".to_string()),
            chatgpt_account_id: None,
        },
        codex_home.path(),
    )
    .expect("failed to write auth file");

    let auth = super::load_auth(
        codex_home.path(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
    )
    .unwrap()
    .unwrap();
    assert_eq!(None, auth.api_key());
    assert_eq!(AuthMode::Chatgpt, auth.auth_mode());
    assert_eq!(auth.get_chatgpt_user_id().as_deref(), Some("user-12345"));

    let active_account = auth
        .current_chatgpt_account_snapshot()
        .expect("active account snapshot should exist");
    let last_refresh = active_account
        .last_refresh
        .expect("last_refresh should be recorded");
    assert_eq!(
        auth.active_chatgpt_account_summary(),
        Some(ActiveChatgptAccountSummary {
            store_account_id: "chatgpt-user:user-12345".to_string(),
            label: None,
            email: Some("user@example.com".to_string()),
            auth_mode: AuthMode::Chatgpt,
        })
    );

    assert_eq!(
        ActiveChatgptAccountSnapshot {
            store_account_id: "chatgpt-user:user-12345".to_string(),
            label: None,
            tokens: TokenData {
                id_token: IdTokenInfo {
                    email: Some("user@example.com".to_string()),
                    chatgpt_plan_type: Some(InternalPlanType::Known(InternalKnownPlan::Pro)),
                    chatgpt_user_id: Some("user-12345".to_string()),
                    chatgpt_account_id: None,
                    raw_jwt: fake_jwt,
                },
                access_token: "test-access-token".to_string(),
                refresh_token: "test-refresh-token".to_string(),
                account_id: None,
            },
            last_refresh: Some(last_refresh),
            auth_mode: AuthMode::Chatgpt,
        },
        active_account.clone()
    );
}

#[tokio::test]
#[serial(codex_api_key)]
async fn loads_api_key_from_auth_json() {
    let dir = tempdir().unwrap();
    let auth_file = dir.path().join("auth.json");
    std::fs::write(
        auth_file,
        r#"{"OPENAI_API_KEY":"sk-test-key","tokens":null,"last_refresh":null}"#,
    )
    .unwrap();

    let auth = super::load_auth(
        dir.path(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
    )
    .unwrap()
    .unwrap();
    assert_eq!(auth.auth_mode(), AuthMode::ApiKey);
    assert_eq!(auth.api_key(), Some("sk-test-key"));

    assert!(auth.get_token_data().is_err());
}

#[test]
fn logout_removes_auth_file() -> Result<(), std::io::Error> {
    let dir = tempdir()?;
    let auth_store = AuthStore {
        openai_api_key: Some("sk-test-key".to_string()),
        ..AuthStore::default()
    };
    super::save_auth(dir.path(), &auth_store, AuthCredentialsStoreMode::File)?;
    let auth_file = get_auth_file(dir.path());
    assert!(auth_file.exists());
    assert!(logout(dir.path(), AuthCredentialsStoreMode::File)?);
    assert!(!auth_file.exists());
    Ok(())
}

#[test]
fn unauthorized_recovery_reports_mode_and_step_names() {
    let dir = tempdir().unwrap();
    let manager = AuthManager::shared(
        dir.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
    );
    let managed = UnauthorizedRecovery {
        manager: Arc::clone(&manager),
        step: UnauthorizedRecoveryStep::Reload,
        expected_store_account_id: None,
        mode: UnauthorizedRecoveryMode::Managed,
    };
    assert_eq!(managed.mode_name(), "managed");
    assert_eq!(managed.step_name(), "reload");

    let external = UnauthorizedRecovery {
        manager,
        step: UnauthorizedRecoveryStep::ExternalRefresh,
        expected_store_account_id: None,
        mode: UnauthorizedRecoveryMode::External,
    };
    assert_eq!(external.mode_name(), "external");
    assert_eq!(external.step_name(), "external_refresh");
}

#[test]
#[serial(codex_api_key)]
fn reload_if_store_account_id_matches_prefers_chatgpt_when_store_also_has_api_key() {
    let codex_home = tempdir().unwrap();
    write_auth_file(
        AuthFileParams {
            openai_api_key: Some("sk-test".to_string()),
            chatgpt_plan_type: Some("pro".to_string()),
            chatgpt_account_id: Some("org_workspace".to_string()),
        },
        codex_home.path(),
    )
    .expect("failed to write auth file");

    let manager = AuthManager::new(
        codex_home.path().to_path_buf(),
        false,
        AuthCredentialsStoreMode::File,
    );
    let expected_store_account_id = manager
        .active_chatgpt_account_summary()
        .expect("ChatGPT auth summary should exist")
        .store_account_id;
    let outcome = manager.reload_if_store_account_id_matches(Some(&expected_store_account_id));
    assert!(
        matches!(
            outcome,
            ReloadOutcome::ReloadedChanged | ReloadOutcome::ReloadedNoChange
        ),
        "reload should not be skipped when saved account ids match"
    );
    let auth = manager.auth_cached().expect("auth should be cached");
    assert_eq!(auth.internal_auth_mode(), crate::AuthMode::Chatgpt);
}

#[test]
fn unauthorized_recovery_tracks_expected_store_account_id() {
    let codex_home = tempdir().unwrap();
    let active_store_account_id =
        persist_test_chatgpt_accounts(codex_home.path(), &["org-primary"], 0);
    let manager = AuthManager::shared(
        codex_home.path().to_path_buf(),
        false,
        AuthCredentialsStoreMode::File,
    );

    let recovery = manager.unauthorized_recovery();
    let cached_account_id = manager
        .auth_cached()
        .and_then(|auth| auth.get_account_id())
        .expect("workspace account id should exist");

    assert_eq!(
        recovery.expected_store_account_id.as_deref(),
        Some(active_store_account_id.as_str())
    );
    assert_ne!(
        recovery.expected_store_account_id.as_deref(),
        Some(cached_account_id.as_str())
    );
}

#[test]
#[serial(codex_api_key)]
fn auth_manager_new_keeps_business_accounts_in_store() {
    let codex_home = tempdir().unwrap();
    write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("business".to_string()),
            chatgpt_account_id: Some("org_workspace".to_string()),
        },
        codex_home.path(),
    )
    .expect("failed to write auth file");

    let manager = AuthManager::new(
        codex_home.path().to_path_buf(),
        false,
        AuthCredentialsStoreMode::File,
    );
    assert_eq!(manager.list_accounts().len(), 1);

    let auth = manager.auth_cached().expect("auth should remain cached");
    assert_eq!(auth.account_plan_type(), Some(AccountPlanType::Business));
    assert_eq!(
        auth.get_token_data()
            .expect("token data should exist")
            .id_token
            .chatgpt_account_id
            .as_deref(),
        Some("org_workspace")
    );
}

#[test]
fn login_with_chatgpt_auth_tokens_rejects_required_workspace_claim_mismatch() {
    let codex_home = tempdir().unwrap();
    let access_token =
        make_test_chatgpt_jwt(Some("pro".to_string()), Some("org-token".to_string())).expect("jwt");

    let err = super::login_with_chatgpt_auth_tokens(
        codex_home.path(),
        &access_token,
        "org-token",
        Some("pro"),
        Some("org-required"),
    )
    .expect_err("required workspace mismatch should be rejected");
    assert!(matches!(
        err,
        super::ExternalAuthLoginError::MetadataMismatch(_)
    ));
    assert!(
        err.to_string()
            .contains("does not match required workspace \"org-required\""),
        "unexpected error: {err}"
    );
}

#[test]
#[serial(codex_api_key)]
fn load_auth_purges_unsupported_external_chatgpt_tokens_without_fallback() {
    let codex_home = tempdir().unwrap();
    let access_token =
        make_test_chatgpt_jwt(Some("free".to_string()), Some("org_workspace".to_string()))
            .expect("jwt");
    let auth_dot_json =
        AuthDotJson::from_external_access_token(&access_token, "org_workspace", Some("free"), None)
            .expect("external auth dot json");
    let store = AuthStore::from_legacy(auth_dot_json);
    super::save_auth(
        codex_home.path(),
        &store,
        AuthCredentialsStoreMode::Ephemeral,
    )
    .expect("save stale external auth");

    let auth = super::load_auth(codex_home.path(), false, AuthCredentialsStoreMode::File)
        .expect("load auth");
    assert_eq!(auth, None);

    let store = super::load_auth_store(codex_home.path(), AuthCredentialsStoreMode::Ephemeral)
        .expect("load external auth store")
        .expect("sanitized external auth store should remain present");
    assert_eq!(store.accounts, Vec::new());
    assert_eq!(store.active_account_id, None);
}

#[test]
#[serial(codex_api_key)]
fn load_auth_falls_back_to_persisted_chatgpt_auth_when_external_tokens_are_unsupported() {
    let codex_home = tempdir().unwrap();
    write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("pro".to_string()),
            chatgpt_account_id: Some("persisted_workspace".to_string()),
        },
        codex_home.path(),
    )
    .expect("write persisted pro auth");

    let access_token = make_test_chatgpt_jwt(
        Some("free".to_string()),
        Some("external_workspace".to_string()),
    )
    .expect("jwt");
    let auth_dot_json = AuthDotJson::from_external_access_token(
        &access_token,
        "external_workspace",
        Some("free"),
        None,
    )
    .expect("external auth dot json");
    let store = AuthStore::from_legacy(auth_dot_json);
    super::save_auth(
        codex_home.path(),
        &store,
        AuthCredentialsStoreMode::Ephemeral,
    )
    .expect("save stale external auth");

    let auth = super::load_auth(codex_home.path(), false, AuthCredentialsStoreMode::File)
        .expect("load auth")
        .expect("persisted auth should remain available");
    assert_eq!(auth.internal_auth_mode(), crate::AuthMode::Chatgpt);
    assert_eq!(auth.account_plan_type(), Some(AccountPlanType::Pro));
    assert_eq!(
        auth.get_token_data()
            .expect("token data should exist")
            .id_token
            .chatgpt_account_id
            .as_deref(),
        Some("persisted_workspace")
    );

    let store = super::load_auth_store(codex_home.path(), AuthCredentialsStoreMode::Ephemeral)
        .expect("load external auth store")
        .expect("sanitized external auth store should remain present");
    assert_eq!(store.accounts, Vec::new());
    assert_eq!(store.active_account_id, None);
}

#[tokio::test]
#[serial(codex_api_key)]
async fn auth_manager_reload_prefers_external_ephemeral_chatgpt_tokens() {
    let codex_home = tempdir().unwrap();
    let manager = AuthManager::shared(
        codex_home.path().to_path_buf(),
        false,
        AuthCredentialsStoreMode::File,
    );
    let access_token =
        make_test_chatgpt_jwt(Some("pro".to_string()), Some("org-external".to_string()))
            .expect("jwt");

    assert_eq!(manager.auth_cached(), None);

    super::login_with_chatgpt_auth_tokens(
        codex_home.path(),
        &access_token,
        "org-external",
        Some("pro"),
        None,
    )
    .expect("external auth should save");

    assert!(manager.reload(), "reload should observe external auth");

    let cached = manager
        .auth_cached()
        .expect("external auth should be cached after reload");
    assert_eq!(cached.api_auth_mode(), AuthMode::ChatgptAuthTokens);
    assert_eq!(
        cached
            .active_chatgpt_account_summary()
            .expect("external account summary")
            .store_account_id,
        test_store_account_id("org-external").expect("store account id")
    );
    assert_eq!(
        manager.auth().await.map(|auth| auth.api_auth_mode()),
        Some(AuthMode::ChatgptAuthTokens)
    );
}

#[test]
fn usage_limit_auto_switch_accepts_only_free_as_explicit_unsupported_proof() {
    assert!(super::usage_limit_auto_switch_removes_plan_type(Some(
        &AccountPlanType::Free
    )));
    assert!(!super::usage_limit_auto_switch_removes_plan_type(Some(
        &AccountPlanType::Unknown
    )));
    assert!(!super::usage_limit_auto_switch_removes_plan_type(Some(
        &AccountPlanType::Go
    )));
    assert!(!super::usage_limit_auto_switch_removes_plan_type(Some(
        &AccountPlanType::Plus
    )));
    assert!(!super::usage_limit_auto_switch_removes_plan_type(Some(
        &AccountPlanType::Pro
    )));
    assert!(!super::usage_limit_auto_switch_removes_plan_type(Some(
        &AccountPlanType::Team
    )));
    assert!(!super::usage_limit_auto_switch_removes_plan_type(Some(
        &AccountPlanType::Business
    )));
    assert!(!super::usage_limit_auto_switch_removes_plan_type(Some(
        &AccountPlanType::Enterprise
    )));
    assert!(!super::usage_limit_auto_switch_removes_plan_type(Some(
        &AccountPlanType::SelfServeBusinessUsageBased
    )));
    assert!(!super::usage_limit_auto_switch_removes_plan_type(Some(
        &AccountPlanType::EnterpriseCbpUsageBased
    )));
    assert!(!super::usage_limit_auto_switch_removes_plan_type(Some(
        &AccountPlanType::Edu
    )));
    assert!(!super::usage_limit_auto_switch_removes_plan_type(None));
}

#[test]
fn classify_refresh_token_failure_treats_token_revoked_as_revoked() {
    let error =
        super::classify_refresh_token_failure(r#"{ "error": { "code": "token_revoked" } }"#);

    assert_eq!(error.reason, RefreshTokenFailedReason::Revoked);
    assert_eq!(
        error.message,
        super::REFRESH_TOKEN_INVALIDATED_MESSAGE.to_string()
    );
}

#[test]
fn mark_usage_limit_reached_defaults_primary_window_to_zero_when_error_reset_is_missing() {
    let codex_home = tempdir().unwrap();
    persist_test_chatgpt_accounts(codex_home.path(), &["org-primary"], 0);
    let manager = AuthManager::shared(
        codex_home.path().to_path_buf(),
        false,
        AuthCredentialsStoreMode::File,
    );
    let primary_reset_at = Utc::now() + chrono::Duration::minutes(15);
    let secondary_reset_at = Utc::now() + chrono::Duration::days(7);

    manager
        .mark_usage_limit_reached(
            None,
            Some(RateLimitSnapshot {
                limit_id: Some("codex".to_string()),
                limit_name: None,
                primary: Some(RateLimitWindow {
                    remaining_percent: 2.0,
                    window_minutes: Some(15),
                    resets_at: Some(primary_reset_at.timestamp()),
                }),
                secondary: Some(RateLimitWindow {
                    remaining_percent: 69.0,
                    window_minutes: None,
                    resets_at: Some(secondary_reset_at.timestamp()),
                }),
                credits: None,
                plan_type: Some(AccountPlanType::Pro),
            }),
        )
        .expect("mark usage limit reached");

    let active_account = manager
        .list_accounts()
        .into_iter()
        .find(|account| account.is_active)
        .expect("active account should exist");
    assert_eq!(
        active_account
            .exhausted_until
            .map(|until| until.timestamp()),
        Some(primary_reset_at.timestamp())
    );
    assert_eq!(
        active_account
            .last_rate_limits
            .as_ref()
            .and_then(|snapshot| snapshot.primary.as_ref())
            .map(|window| window.remaining_percent),
        Some(0.0)
    );
    assert_eq!(
        active_account
            .last_rate_limits
            .as_ref()
            .and_then(|snapshot| snapshot.secondary.as_ref())
            .map(|window| window.remaining_percent),
        Some(69.0)
    );
}

#[test]
fn mark_usage_limit_reached_clamps_matched_weekly_window_to_zero() {
    let codex_home = tempdir().unwrap();
    persist_test_chatgpt_accounts(codex_home.path(), &["org-weekly"], 0);
    let manager = AuthManager::shared(
        codex_home.path().to_path_buf(),
        false,
        AuthCredentialsStoreMode::File,
    );
    let primary_reset_at = Utc::now() + chrono::Duration::minutes(15);
    let secondary_reset_at = Utc::now() + chrono::Duration::days(7);

    manager
        .mark_usage_limit_reached(
            Some(secondary_reset_at),
            Some(RateLimitSnapshot {
                limit_id: Some("codex".to_string()),
                limit_name: None,
                primary: Some(RateLimitWindow {
                    remaining_percent: 54.0,
                    window_minutes: Some(15),
                    resets_at: Some(primary_reset_at.timestamp()),
                }),
                secondary: Some(RateLimitWindow {
                    remaining_percent: 62.0,
                    window_minutes: None,
                    resets_at: Some(secondary_reset_at.timestamp()),
                }),
                credits: None,
                plan_type: Some(AccountPlanType::Pro),
            }),
        )
        .expect("mark weekly usage limit reached");

    let active_account = manager
        .list_accounts()
        .into_iter()
        .find(|account| account.is_active)
        .expect("active account should exist");
    assert_eq!(
        active_account
            .exhausted_until
            .map(|until| until.timestamp()),
        Some(secondary_reset_at.timestamp())
    );
    assert_eq!(
        active_account
            .last_rate_limits
            .as_ref()
            .and_then(|snapshot| snapshot.primary.as_ref())
            .map(|window| window.remaining_percent),
        Some(54.0)
    );
    assert_eq!(
        active_account
            .last_rate_limits
            .as_ref()
            .and_then(|snapshot| snapshot.secondary.as_ref())
            .map(|window| window.remaining_percent),
        Some(0.0)
    );
}

#[test]
fn cooldown_does_not_purge_ambiguous_unknown_fallbacks_and_marks_failing_account() {
    let codex_home = tempdir().unwrap();
    let active_store_account_id =
        persist_test_chatgpt_accounts(codex_home.path(), &["org-primary", "org-fallback"], 0);
    let fallback_store_account_id =
        test_store_account_id("org-fallback").expect("fallback store account id");
    let manager = AuthManager::shared(
        codex_home.path().to_path_buf(),
        false,
        AuthCredentialsStoreMode::File,
    );
    *manager
        .usage_limit_auto_switch_cooldown_until
        .lock()
        .expect("cooldown lock") = Some(Utc::now() + chrono::Duration::seconds(30));

    let switched_to = manager
        .switch_account_on_usage_limit(UsageLimitAutoSwitchRequest {
            required_workspace_id: None,
            failing_store_account_id: Some(active_store_account_id.as_str()),
            resets_at: None,
            snapshot: Some(RateLimitSnapshot {
                limit_id: Some("codex".to_string()),
                limit_name: None,
                primary: Some(RateLimitWindow {
                    remaining_percent: 100.0,
                    window_minutes: Some(15),
                    resets_at: Some((Utc::now() + chrono::Duration::minutes(15)).timestamp()),
                }),
                secondary: None,
                credits: None,
                plan_type: Some(AccountPlanType::Pro),
            }),
            freshly_unsupported_store_account_ids: &HashSet::new(),
            protected_store_account_id: None,
            selection_scope: UsageLimitAutoSwitchSelectionScope::PersistedTruth,
            fallback_selection_mode:
                UsageLimitAutoSwitchFallbackSelectionMode::AllowFallbackSelection,
        })
        .expect("cooldown path should succeed");

    assert_eq!(
        switched_to, None,
        "cooldown should still suppress switching"
    );
    let accounts = manager.list_accounts();
    assert_eq!(
        accounts
            .iter()
            .find(|account| account.is_active)
            .map(|account| account.id.as_str()),
        Some(active_store_account_id.as_str())
    );
    assert!(
        accounts
            .iter()
            .any(|account| account.id == fallback_store_account_id),
        "ambiguous fallback accounts must survive when no explicit unsupported proof was refreshed"
    );
    assert!(
        accounts
            .iter()
            .find(|account| account.id == active_store_account_id)
            .and_then(|account| account.exhausted_until)
            .is_some(),
        "failing account should still be marked exhausted during cooldown"
    );
}

#[test]
fn cooldown_still_purges_freshly_free_fallbacks_and_marks_failing_account() {
    let codex_home = tempdir().unwrap();
    let active_store_account_id =
        persist_test_chatgpt_accounts(codex_home.path(), &["org-primary", "org-fallback"], 0);
    let fallback_store_account_id =
        test_store_account_id("org-fallback").expect("fallback store account id");
    let manager = AuthManager::shared(
        codex_home.path().to_path_buf(),
        false,
        AuthCredentialsStoreMode::File,
    );
    *manager
        .usage_limit_auto_switch_cooldown_until
        .lock()
        .expect("cooldown lock") = Some(Utc::now() + chrono::Duration::seconds(30));

    let switched_to = manager
        .switch_account_on_usage_limit(UsageLimitAutoSwitchRequest {
            required_workspace_id: None,
            failing_store_account_id: Some(active_store_account_id.as_str()),
            resets_at: None,
            snapshot: Some(RateLimitSnapshot {
                limit_id: Some("codex".to_string()),
                limit_name: None,
                primary: Some(RateLimitWindow {
                    remaining_percent: 100.0,
                    window_minutes: Some(15),
                    resets_at: Some((Utc::now() + chrono::Duration::minutes(15)).timestamp()),
                }),
                secondary: None,
                credits: None,
                plan_type: Some(AccountPlanType::Pro),
            }),
            freshly_unsupported_store_account_ids: &HashSet::from([
                fallback_store_account_id.clone()
            ]),
            protected_store_account_id: None,
            selection_scope: UsageLimitAutoSwitchSelectionScope::PersistedTruth,
            fallback_selection_mode:
                UsageLimitAutoSwitchFallbackSelectionMode::AllowFallbackSelection,
        })
        .expect("cooldown path should succeed");

    assert_eq!(
        switched_to, None,
        "cooldown should still suppress switching"
    );
    let accounts = manager.list_accounts();
    assert_eq!(
        accounts
            .iter()
            .find(|account| account.is_active)
            .map(|account| account.id.as_str()),
        Some(active_store_account_id.as_str())
    );
    assert!(
        accounts
            .iter()
            .all(|account| account.id != fallback_store_account_id),
        "fresh explicit unsupported proof should still purge the fallback even during cooldown"
    );
    assert!(
        accounts
            .iter()
            .find(|account| account.id == active_store_account_id)
            .and_then(|account| account.exhausted_until)
            .is_some(),
        "failing account should still be marked exhausted during cooldown"
    );
}

#[test]
fn select_account_for_auto_switch_prefers_higher_primary_headroom_when_weekly_ties() {
    let now = Utc::now();
    let stronger_store_account_id =
        test_store_account_id("org-stronger").expect("stronger store account id");
    let store = AuthStore {
        accounts: vec![
            stored_test_chatgpt_account_with_usage("org-weaker", 20.0, 20.0, now),
            stored_test_chatgpt_account_with_usage("org-stronger", 80.0, 20.0, now),
        ],
        ..AuthStore::default()
    };

    assert_eq!(
        super::select_account_for_auto_switch_from_store(
            &store,
            None,
            None,
            now,
            UsageLimitAutoSwitchSelectionScope::PersistedTruth,
        )
        .as_deref(),
        Some(stronger_store_account_id.as_str())
    );
}

#[test]
fn select_account_for_auto_switch_prefers_higher_weekly_headroom_when_primary_ties() {
    let now = Utc::now();
    let stronger_store_account_id =
        test_store_account_id("org-stronger").expect("stronger store account id");
    let store = AuthStore {
        accounts: vec![
            stored_test_chatgpt_account_with_usage("org-weaker", 20.0, 20.0, now),
            stored_test_chatgpt_account_with_usage("org-stronger", 20.0, 80.0, now),
        ],
        ..AuthStore::default()
    };

    assert_eq!(
        super::select_account_for_auto_switch_from_store(
            &store,
            None,
            None,
            now,
            UsageLimitAutoSwitchSelectionScope::PersistedTruth,
        )
        .as_deref(),
        Some(stronger_store_account_id.as_str())
    );
}

#[test]
fn select_account_for_auto_switch_prefers_primary_headroom_before_weekly_headroom() {
    let now = Utc::now();
    let stronger_store_account_id =
        test_store_account_id("org-primary-favored").expect("stronger store account id");
    let store = AuthStore {
        accounts: vec![
            stored_test_chatgpt_account_with_usage("org-weekly-favored", 5.0, 90.0, now),
            stored_test_chatgpt_account_with_usage("org-primary-favored", 10.0, 20.0, now),
        ],
        ..AuthStore::default()
    };

    assert_eq!(
        super::select_account_for_auto_switch_from_store(
            &store,
            None,
            None,
            now,
            UsageLimitAutoSwitchSelectionScope::PersistedTruth,
        )
        .as_deref(),
        Some(stronger_store_account_id.as_str())
    );
}

#[test]
fn select_account_for_auto_switch_respects_freshly_selectable_scope() {
    let now = Utc::now();
    let stronger_store_account_id =
        test_store_account_id("org-stronger").expect("stronger store account id");
    let weaker_store_account_id =
        test_store_account_id("org-weaker").expect("weaker store account id");
    let store = AuthStore {
        accounts: vec![
            stored_test_chatgpt_account_with_usage("org-weaker", 80.0, 20.0, now),
            stored_test_chatgpt_account_with_usage("org-stronger", 20.0, 20.0, now),
        ],
        ..AuthStore::default()
    };
    let freshly_selectable_store_account_ids = HashSet::from([weaker_store_account_id.clone()]);

    assert_eq!(
        super::select_account_for_auto_switch_from_store(
            &store,
            None,
            None,
            now,
            UsageLimitAutoSwitchSelectionScope::FreshlySelectable(
                &freshly_selectable_store_account_ids,
            ),
        )
        .as_deref(),
        Some(weaker_store_account_id.as_str())
    );
    assert_ne!(weaker_store_account_id, stronger_store_account_id);
}

#[test]
fn exhausted_until_from_snapshot_does_not_block_near_empty_window_without_429() {
    let now = Utc::now();
    let snapshot = test_rate_limit_snapshot(1.4, 69.0, now);

    assert_eq!(super::exhausted_until_from_snapshot(&snapshot, now), None);
}

#[test]
fn exhausted_until_from_snapshot_does_not_block_fractional_window_without_429() {
    let now = Utc::now();
    let snapshot = test_rate_limit_snapshot(0.4, 69.0, now);

    assert_eq!(super::exhausted_until_from_snapshot(&snapshot, now), None);
}

#[test]
fn update_rate_limits_for_accounts_clears_stale_exhaustion_for_unblocked_saved_account() {
    let now = Utc::now();
    let codex_home = tempdir().unwrap();
    let store_account_id = test_store_account_id("org-stale-exhausted").expect("store account id");
    let mut account = stored_test_chatgpt_account_with_usage("org-stale-exhausted", 0.0, 20.0, now);
    account.usage.as_mut().expect("usage cache").exhausted_until =
        Some(now + chrono::Duration::minutes(15));
    let store = AuthStore {
        active_account_id: Some(store_account_id.clone()),
        accounts: vec![account],
        ..AuthStore::default()
    };
    save_auth(codex_home.path(), &store, AuthCredentialsStoreMode::File).expect("save auth store");

    let manager = AuthManager::shared(
        codex_home.path().to_path_buf(),
        false,
        AuthCredentialsStoreMode::File,
    );
    let updated = manager
        .update_rate_limits_for_accounts([(
            store_account_id.clone(),
            test_rate_limit_snapshot(90.0, 20.0, now),
        )])
        .expect("update rate limits");

    assert_eq!(updated, 1);
    let accounts = manager.list_accounts();
    assert_eq!(
        accounts
            .iter()
            .find(|account| account.id == store_account_id)
            .and_then(|account| account.exhausted_until),
        None
    );
}

#[test]
fn reconcile_account_rate_limit_refresh_outcomes_clears_stale_usage_for_attempted_account_without_snapshot()
 {
    let now = Utc::now();
    let codex_home = tempdir().unwrap();
    let store_account_id =
        test_store_account_id("org-stale-refresh").expect("stale-refresh store account id");
    let mut account = stored_test_chatgpt_account_with_usage("org-stale-refresh", 92.0, 44.0, now);
    account.usage.as_mut().expect("usage cache").exhausted_until =
        Some(now + chrono::Duration::minutes(30));
    let store = AuthStore {
        active_account_id: Some(store_account_id.clone()),
        accounts: vec![account],
        ..AuthStore::default()
    };
    save_auth(codex_home.path(), &store, AuthCredentialsStoreMode::File).expect("save auth store");

    let manager = AuthManager::shared(
        codex_home.path().to_path_buf(),
        false,
        AuthCredentialsStoreMode::File,
    );
    let updated = manager
        .reconcile_account_rate_limit_refresh_outcomes([(
            store_account_id.clone(),
            AccountRateLimitRefreshOutcome::NoUsableSnapshot,
        )])
        .expect("reconcile refresh outcomes");

    assert_eq!(updated, 1);
    let refreshed_account = manager
        .list_accounts()
        .into_iter()
        .find(|account| account.id == store_account_id)
        .expect("refreshed account should exist");
    assert_eq!(refreshed_account.last_rate_limits, None);
    assert_eq!(refreshed_account.exhausted_until, None);
}

#[test]
fn auth_manager_strips_legacy_usage_cache_without_backfilling_sqlite_during_v2_cutover() {
    let now = Utc::now();
    let codex_home = tempdir().unwrap();
    let store_account_id =
        test_store_account_id("org-legacy-usage").expect("legacy-usage store account id");
    let snapshot = test_rate_limit_snapshot(42.0, 17.0, now);
    let exhausted_until = Some(now + chrono::Duration::minutes(20));
    let account = StoredAccount {
        usage: Some(AccountUsageCache {
            last_rate_limits: Some(snapshot),
            exhausted_until,
            last_seen_at: Some(now),
        }),
        ..stored_test_chatgpt_account("org-legacy-usage", Some(now))
    };
    let store = AuthStore {
        active_account_id: Some(store_account_id.clone()),
        accounts: vec![account],
        ..AuthStore::default()
    };
    save_auth(codex_home.path(), &store, AuthCredentialsStoreMode::File).expect("save auth store");

    let manager = AuthManager::shared(
        codex_home.path().to_path_buf(),
        false,
        AuthCredentialsStoreMode::File,
    );
    let account = manager
        .list_accounts()
        .into_iter()
        .find(|account| account.id == store_account_id)
        .expect("account summary should exist");
    assert_eq!(account.last_rate_limits, None);
    assert_eq!(account.exhausted_until, None);

    let stripped_store = load_auth_store(codex_home.path(), AuthCredentialsStoreMode::File)
        .expect("load stripped auth store")
        .expect("auth store should exist");
    assert_eq!(stripped_store.active_account_id, None);
    assert_eq!(stripped_store.accounts[0].usage, None);

    let account_state_store =
        AccountStateStore::open(codex_home.path().to_path_buf()).expect("open account state store");
    let usage_by_account = account_state_store
        .load_usage_states_for_accounts(std::slice::from_ref(&store_account_id))
        .expect("load sqlite usage states");
    assert!(
        !usage_by_account.contains_key(&store_account_id),
        "WS12-C should stop backfilling legacy auth-store usage into sqlite"
    );
}

#[test]
fn list_accounts_reads_latest_usage_truth_from_sqlite_across_managers() {
    let now = Utc::now();
    let codex_home = tempdir().unwrap();
    let active_store_account_id =
        persist_test_chatgpt_accounts(codex_home.path(), &["org-a", "org-b"], 0);
    let snapshot = test_rate_limit_snapshot(30.0, 15.0, now);

    let writer = AuthManager::shared(
        codex_home.path().to_path_buf(),
        false,
        AuthCredentialsStoreMode::File,
    );
    let reader = AuthManager::shared(
        codex_home.path().to_path_buf(),
        false,
        AuthCredentialsStoreMode::File,
    );

    writer
        .update_rate_limits_for_account(&active_store_account_id, snapshot.clone())
        .expect("persist usage truth");

    let active_account = reader
        .list_accounts()
        .into_iter()
        .find(|account| account.id == active_store_account_id)
        .expect("active account summary should exist");
    assert_eq!(active_account.last_rate_limits, Some(snapshot));

    let stripped_store = load_auth_store(codex_home.path(), AuthCredentialsStoreMode::File)
        .expect("load stripped auth store")
        .expect("auth store should exist");
    assert_eq!(
        stripped_store
            .accounts
            .into_iter()
            .find(|account| account.id == active_store_account_id)
            .expect("stored account should exist")
            .usage,
        None
    );
}

#[test]
fn accounts_rate_limits_cache_expires_at_reads_latest_usage_truth_from_sqlite_across_managers() {
    let codex_home = tempdir().unwrap();
    let active_store_account_id =
        persist_test_chatgpt_accounts(codex_home.path(), &["org-a", "org-b"], 0);
    let reader = AuthManager::shared(
        codex_home.path().to_path_buf(),
        false,
        AuthCredentialsStoreMode::File,
    );
    let writer = AuthManager::shared(
        codex_home.path().to_path_buf(),
        false,
        AuthCredentialsStoreMode::File,
    );

    assert_eq!(
        reader.accounts_rate_limits_cache_expires_at(Utc::now()),
        None
    );

    let reset_at = Utc::now() + chrono::Duration::minutes(45);
    writer
        .update_rate_limits_for_account(
            &active_store_account_id,
            RateLimitSnapshot {
                limit_id: Some("codex".to_string()),
                limit_name: None,
                primary: Some(RateLimitWindow {
                    remaining_percent: 42.0,
                    window_minutes: Some(300),
                    resets_at: Some(reset_at.timestamp()),
                }),
                secondary: None,
                credits: None,
                plan_type: Some(AccountPlanType::Pro),
            },
        )
        .expect("persist usage truth");

    assert_eq!(
        reader.accounts_rate_limits_cache_expires_at(Utc::now()),
        DateTime::from_timestamp(reset_at.timestamp(), 0)
    );
}

#[test]
fn auth_manager_preserves_legacy_usage_when_sqlite_sync_fails() {
    let now = Utc::now();
    let codex_home = tempdir().unwrap();
    let store_account_id =
        test_store_account_id("org-legacy-failure").expect("legacy-failure store account id");
    let snapshot = test_rate_limit_snapshot(42.0, 17.0, now);
    let exhausted_until = Some(now + chrono::Duration::minutes(20));
    let account = StoredAccount {
        usage: Some(AccountUsageCache {
            last_rate_limits: Some(snapshot.clone()),
            exhausted_until,
            last_seen_at: Some(now),
        }),
        ..stored_test_chatgpt_account("org-legacy-failure", Some(now))
    };
    let store = AuthStore {
        active_account_id: Some(store_account_id.clone()),
        accounts: vec![account],
        ..AuthStore::default()
    };
    save_auth(codex_home.path(), &store, AuthCredentialsStoreMode::File).expect("save auth store");

    let sqlite_path = accounts_db_path(codex_home.path());
    let connection = Connection::open(sqlite_path).expect("open raw sqlite db");
    connection
        .execute_batch(
            r#"
CREATE TABLE IF NOT EXISTS account_usage_state (
    account_id TEXT PRIMARY KEY,
    rate_limits_json TEXT,
    exhausted_until INTEGER,
    last_seen_at INTEGER,
    updated_at INTEGER NOT NULL
);
            "#,
        )
        .expect("create usage state table");
    connection
        .execute(
            "INSERT INTO account_usage_state (account_id, rate_limits_json, exhausted_until, last_seen_at, updated_at) VALUES (?, ?, ?, ?, ?)",
            (
                store_account_id.as_str(),
                "{not-valid-json",
                exhausted_until.map(|value| value.timestamp()),
                Some(now.timestamp()),
                now.timestamp(),
            ),
        )
        .expect("seed corrupted usage state row");

    let manager = AuthManager::shared(
        codex_home.path().to_path_buf(),
        false,
        AuthCredentialsStoreMode::File,
    );

    let account = manager
        .list_accounts()
        .into_iter()
        .find(|account| account.id == store_account_id)
        .expect("account summary should exist");
    assert_eq!(account.last_rate_limits, Some(snapshot));

    let persisted_store = load_auth_store(codex_home.path(), AuthCredentialsStoreMode::File)
        .expect("load auth store after failed sync")
        .expect("auth store should exist");
    assert_eq!(
        persisted_store.accounts[0]
            .usage
            .as_ref()
            .expect("legacy usage should remain persisted on sync failure")
            .last_rate_limits,
        store.accounts[0]
            .usage
            .as_ref()
            .expect("original legacy usage should exist")
            .last_rate_limits
    );
}

#[test]
fn auth_manager_falls_back_to_legacy_usage_when_sqlite_home_is_invalid() {
    let now = Utc::now();
    let codex_home = tempdir().unwrap();
    let sqlite_home_parent = tempdir().unwrap();
    let sqlite_home = sqlite_home_parent.path().join("sqlite-home-file");
    std::fs::write(sqlite_home.as_path(), "not a directory").expect("seed invalid sqlite home");
    let store_account_id =
        test_store_account_id("org-invalid-sqlite-home").expect("invalid sqlite home account id");
    let snapshot = test_rate_limit_snapshot(22.0, 11.0, now);
    let exhausted_until = Some(now + chrono::Duration::minutes(30));
    let account = StoredAccount {
        usage: Some(AccountUsageCache {
            last_rate_limits: Some(snapshot.clone()),
            exhausted_until,
            last_seen_at: Some(now),
        }),
        ..stored_test_chatgpt_account("org-invalid-sqlite-home", Some(now))
    };
    let store = AuthStore {
        active_account_id: Some(store_account_id.clone()),
        accounts: vec![account],
        ..AuthStore::default()
    };
    save_auth(codex_home.path(), &store, AuthCredentialsStoreMode::File).expect("save auth store");

    let manager = AuthManager::new_with_sqlite_home(
        codex_home.path().to_path_buf(),
        sqlite_home,
        false,
        AuthCredentialsStoreMode::File,
    );

    let account = manager
        .list_accounts()
        .into_iter()
        .find(|account| account.id == store_account_id)
        .expect("account summary should exist");
    assert_eq!(account.last_rate_limits, Some(snapshot.clone()));
    assert!(!account.is_active);
    assert_eq!(
        account
            .exhausted_until
            .map(|exhausted_until| exhausted_until.timestamp()),
        exhausted_until.map(|exhausted_until| exhausted_until.timestamp())
    );
    assert!(manager.auth_cached().is_none());
    assert_eq!(manager.active_chatgpt_account_summary(), None);
    assert_eq!(
        load_auth_preflight_state(
            codex_home.path(),
            /*enable_codex_api_key_env*/ false,
            AuthCredentialsStoreMode::File,
            None,
        )
        .expect("load auth preflight state"),
        PreflightAuthState::Chatgpt {
            has_matching_workspace: true
        }
    );

    let persisted_store = load_auth_store(codex_home.path(), AuthCredentialsStoreMode::File)
        .expect("load auth store after sqlite fallback")
        .expect("auth store should exist");
    assert_eq!(
        persisted_store
            .accounts
            .into_iter()
            .find(|account| account.id == store_account_id)
            .expect("stored account should exist")
            .usage
            .as_ref()
            .expect("legacy usage should stay persisted when sqlite is unavailable")
            .last_rate_limits,
        Some(snapshot)
    );
}

#[test]
fn distinct_managers_bootstrap_distinct_session_active_accounts_and_ignore_stale_auth_store_active()
{
    let codex_home = tempdir().unwrap();
    let primary_store_account_id =
        persist_test_chatgpt_accounts(codex_home.path(), &["org-a", "org-b"], 0);
    let secondary_store_account_id =
        test_store_account_id("org-b").expect("secondary store account id");
    let manager_a = AuthManager::shared(
        codex_home.path().to_path_buf(),
        false,
        AuthCredentialsStoreMode::File,
    );
    let manager_b = AuthManager::shared(
        codex_home.path().to_path_buf(),
        false,
        AuthCredentialsStoreMode::File,
    );

    assert_eq!(
        manager_a
            .active_chatgpt_account_summary()
            .expect("primary session active account")
            .store_account_id,
        primary_store_account_id
    );
    assert_eq!(
        manager_b
            .active_chatgpt_account_summary()
            .expect("secondary session active account")
            .store_account_id,
        secondary_store_account_id
    );

    let accounts_a = manager_a.list_accounts();
    assert!(
        accounts_a
            .iter()
            .find(|account| account.id == primary_store_account_id)
            .expect("primary account should exist")
            .is_active
    );
    assert!(
        !accounts_a
            .iter()
            .find(|account| account.id == secondary_store_account_id)
            .expect("secondary account should exist")
            .is_active
    );

    let accounts_b = manager_b.list_accounts();
    assert!(
        !accounts_b
            .iter()
            .find(|account| account.id == primary_store_account_id)
            .expect("primary account should exist")
            .is_active
    );
    assert!(
        accounts_b
            .iter()
            .find(|account| account.id == secondary_store_account_id)
            .expect("secondary account should exist")
            .is_active
    );

    let stripped_store = load_auth_store(codex_home.path(), AuthCredentialsStoreMode::File)
        .expect("load stripped auth store")
        .expect("auth store should exist");
    assert_eq!(stripped_store.active_account_id, None);

    let stale_store = AuthStore {
        active_account_id: Some(primary_store_account_id),
        ..stripped_store
    };
    save_auth(
        codex_home.path(),
        &stale_store,
        AuthCredentialsStoreMode::File,
    )
    .expect("persist stale auth store active account");

    manager_b.reload_strict().expect("reload secondary manager");

    assert_eq!(
        manager_b
            .active_chatgpt_account_summary()
            .expect("secondary manager should keep its leased account")
            .store_account_id,
        secondary_store_account_id
    );
}

#[test]
fn set_active_account_rejects_account_leased_by_other_live_session() {
    let codex_home = tempdir().unwrap();
    let primary_store_account_id =
        persist_test_chatgpt_accounts(codex_home.path(), &["org-a", "org-b"], 0);
    let secondary_store_account_id =
        test_store_account_id("org-b").expect("secondary store account id");
    let manager_a = AuthManager::shared(
        codex_home.path().to_path_buf(),
        false,
        AuthCredentialsStoreMode::File,
    );
    let manager_b = AuthManager::shared(
        codex_home.path().to_path_buf(),
        false,
        AuthCredentialsStoreMode::File,
    );

    assert_eq!(
        manager_a
            .active_chatgpt_account_summary()
            .expect("primary session active account")
            .store_account_id,
        primary_store_account_id
    );
    assert_eq!(
        manager_b
            .active_chatgpt_account_summary()
            .expect("secondary session active account")
            .store_account_id,
        secondary_store_account_id
    );

    let err = manager_b
        .set_active_account(&primary_store_account_id)
        .expect_err("leased account should fail loud");

    assert!(
        err.to_string()
            .contains("is currently leased by another live session"),
        "unexpected error: {err}"
    );
    assert_eq!(
        manager_b
            .active_chatgpt_account_summary()
            .expect("secondary session should keep its original active account")
            .store_account_id,
        secondary_store_account_id
    );
}

#[test]
fn switching_active_account_releases_previous_lease_for_other_sessions() {
    let codex_home = tempdir().unwrap();
    let primary_store_account_id =
        persist_test_chatgpt_accounts(codex_home.path(), &["org-a", "org-b", "org-c"], 0);
    let secondary_store_account_id =
        test_store_account_id("org-b").expect("secondary store account id");
    let tertiary_store_account_id =
        test_store_account_id("org-c").expect("tertiary store account id");
    let manager_a = AuthManager::shared(
        codex_home.path().to_path_buf(),
        false,
        AuthCredentialsStoreMode::File,
    );
    let manager_b = AuthManager::shared(
        codex_home.path().to_path_buf(),
        false,
        AuthCredentialsStoreMode::File,
    );

    assert_eq!(
        manager_a
            .active_chatgpt_account_summary()
            .expect("primary session active account")
            .store_account_id,
        primary_store_account_id
    );
    assert_eq!(
        manager_b
            .active_chatgpt_account_summary()
            .expect("secondary session active account")
            .store_account_id,
        secondary_store_account_id
    );

    manager_a
        .set_active_account(&tertiary_store_account_id)
        .expect("switch primary session to tertiary account");
    manager_b
        .set_active_account(&primary_store_account_id)
        .expect("released primary account should become selectable");

    assert_eq!(
        manager_a
            .active_chatgpt_account_summary()
            .expect("primary session should now hold the tertiary account")
            .store_account_id,
        tertiary_store_account_id
    );
    assert_eq!(
        manager_b
            .active_chatgpt_account_summary()
            .expect("secondary session should now hold the released primary account")
            .store_account_id,
        primary_store_account_id
    );
}

#[test]
fn guarded_reload_strips_persisted_active_account_after_supported_plan_prune() {
    let codex_home = tempdir().unwrap();
    let active_store_account_id = persist_test_chatgpt_accounts(codex_home.path(), &["org-a"], 0);
    let manager = AuthManager::shared(
        codex_home.path().to_path_buf(),
        false,
        AuthCredentialsStoreMode::File,
    );
    assert_eq!(
        manager
            .active_chatgpt_account_summary()
            .expect("active account should be available")
            .store_account_id,
        active_store_account_id
    );

    let unsupported_access_token =
        make_test_chatgpt_jwt(Some("free".to_string()), Some("org-free".to_string()))
            .expect("free-plan jwt");
    let unsupported_tokens = TokenData {
        id_token: crate::token_data::parse_chatgpt_jwt_claims(&unsupported_access_token)
            .expect("unsupported id token"),
        access_token: unsupported_access_token,
        refresh_token: "refresh-org-free".to_string(),
        account_id: Some("org-free".to_string()),
    };
    let unsupported_account = StoredAccount {
        id: unsupported_tokens
            .preferred_store_account_id()
            .expect("unsupported store account id"),
        label: None,
        tokens: unsupported_tokens,
        last_refresh: Some(Utc::now()),
        usage: None,
    };

    let mut reintroduced_store = load_auth_store(codex_home.path(), AuthCredentialsStoreMode::File)
        .expect("load auth store")
        .expect("auth store should exist");
    reintroduced_store.active_account_id = Some(active_store_account_id.clone());
    reintroduced_store.accounts.push(unsupported_account);
    save_auth(
        codex_home.path(),
        &reintroduced_store,
        AuthCredentialsStoreMode::File,
    )
    .expect("persist reintroduced unsupported account");

    let outcome =
        manager.reload_if_store_account_id_matches(Some(active_store_account_id.as_str()));
    assert!(matches!(
        outcome,
        ReloadOutcome::ReloadedChanged | ReloadOutcome::ReloadedNoChange
    ));

    let persisted_store = load_auth_store(codex_home.path(), AuthCredentialsStoreMode::File)
        .expect("load persisted auth store after guarded reload")
        .expect("auth store should exist");
    assert_eq!(persisted_store.active_account_id, None);
    assert_eq!(persisted_store.accounts.len(), 1);
    assert_eq!(persisted_store.accounts[0].id, active_store_account_id);
}

#[test]
fn refresh_failure_is_scoped_to_the_matching_auth_snapshot() {
    let codex_home = tempdir().unwrap();
    write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("pro".to_string()),
            chatgpt_account_id: Some("org_mine".to_string()),
        },
        codex_home.path(),
    )
    .expect("failed to write auth file");

    let auth = super::load_auth(
        codex_home.path(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
    )
    .expect("load auth")
    .expect("auth available");
    let mut updated_active_account = auth
        .current_chatgpt_account_snapshot()
        .expect("active account snapshot should exist")
        .clone();
    updated_active_account.tokens.access_token = "new-access-token".to_string();
    updated_active_account.tokens.refresh_token = "new-refresh-token".to_string();
    let storage = create_auth_storage(
        codex_home.path().to_path_buf(),
        AuthCredentialsStoreMode::File,
    );
    let updated_auth =
        CodexAuth::from_chatgpt_active_account_snapshot(updated_active_account, Some(storage))
            .expect("updated auth should parse");

    let manager = AuthManager::from_auth_for_testing(auth.clone());
    let error = RefreshTokenFailedError::new(
        RefreshTokenFailedReason::Exhausted,
        "refresh token already used",
    );
    manager.record_permanent_refresh_failure_if_unchanged(&auth, &error);

    assert_eq!(manager.refresh_failure_for_auth(&auth), Some(error));
    assert_eq!(manager.refresh_failure_for_auth(&updated_auth), None);
}

#[test]
fn active_chatgpt_account_summary_comes_from_runtime_snapshot() {
    let codex_home = tempdir().unwrap();
    let account = StoredAccount {
        label: Some("Primary workspace".to_string()),
        ..stored_test_chatgpt_account("org_workspace", Some(Utc::now()))
    };
    let expected_summary = ActiveChatgptAccountSummary {
        store_account_id: account.id.clone(),
        label: account.label.clone(),
        email: account.tokens.id_token.email.clone(),
        auth_mode: AuthMode::Chatgpt,
    };
    let store = AuthStore {
        active_account_id: Some(account.id.clone()),
        accounts: vec![account],
        ..AuthStore::default()
    };
    save_auth(codex_home.path(), &store, AuthCredentialsStoreMode::File).expect("save auth store");

    let manager = AuthManager::new(
        codex_home.path().to_path_buf(),
        false,
        AuthCredentialsStoreMode::File,
    );
    let auth = manager.auth_cached().expect("auth should be cached");

    assert_eq!(
        auth.active_chatgpt_account_summary(),
        Some(expected_summary.clone())
    );
    assert_eq!(
        manager.active_chatgpt_account_summary(),
        Some(expected_summary)
    );
}

#[tokio::test]
async fn resolve_chatgpt_auth_for_store_account_id_removes_terminal_refresh_failure_account() {
    let codex_home = tempdir().unwrap();
    let store_account_id = persist_test_chatgpt_accounts(codex_home.path(), &["org-primary"], 0);
    let manager = AuthManager::shared(
        codex_home.path().to_path_buf(),
        false,
        AuthCredentialsStoreMode::File,
    );
    let auth = manager.auth_cached().expect("auth should be cached");
    let error = RefreshTokenFailedError::new(
        RefreshTokenFailedReason::Revoked,
        "refresh token invalidated",
    );
    manager.record_permanent_refresh_failure_if_unchanged(&auth, &error);

    let resolution = manager
        .resolve_chatgpt_auth_for_store_account_id(
            &store_account_id,
            ChatgptAccountRefreshMode::Force,
        )
        .await
        .expect("resolution should succeed");

    assert_eq!(
        resolution,
        ChatgptAccountAuthResolution::Removed {
            error,
            switched_to_store_account_id: None,
        }
    );
    assert_eq!(manager.list_accounts(), Vec::new());
    assert_eq!(manager.auth_cached(), None);
}

#[tokio::test]
async fn unauthorized_recovery_drops_invalidated_active_account_and_switches_to_fallback() {
    let codex_home = tempdir().unwrap();
    let active_store_account_id =
        persist_test_chatgpt_accounts(codex_home.path(), &["org-primary", "org-fallback"], 0);
    let fallback_store_account_id =
        test_store_account_id("org-fallback").expect("fallback store account id");
    let manager = AuthManager::shared(
        codex_home.path().to_path_buf(),
        false,
        AuthCredentialsStoreMode::File,
    );
    let auth = manager.auth_cached().expect("auth should be cached");
    let error = RefreshTokenFailedError::new(
        RefreshTokenFailedReason::Revoked,
        "refresh token invalidated",
    );
    manager.record_permanent_refresh_failure_if_unchanged(&auth, &error);

    let mut recovery = manager.unauthorized_recovery();
    let reload_result = recovery.next().await.expect("reload step should succeed");
    assert!(reload_result.auth_state_changed().is_some());

    let refresh_result = recovery
        .next()
        .await
        .expect("refresh step should switch to fallback");
    assert_eq!(refresh_result.auth_state_changed(), Some(true));

    let accounts = manager.list_accounts();
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].id, fallback_store_account_id);
    assert!(accounts[0].is_active);
    assert!(
        accounts
            .iter()
            .all(|account| account.id != active_store_account_id),
        "the invalidated account should be removed from the saved store"
    );
}

#[tokio::test]
async fn refresh_token_from_authority_succeeds_when_terminal_failure_switches_to_fallback_store_account()
 {
    let codex_home = tempdir().unwrap();
    let active_store_account_id =
        persist_test_chatgpt_accounts(codex_home.path(), &["org-primary", "org-fallback"], 0);
    let fallback_store_account_id =
        test_store_account_id("org-fallback").expect("fallback store account id");
    let manager = AuthManager::shared(
        codex_home.path().to_path_buf(),
        false,
        AuthCredentialsStoreMode::File,
    );
    let auth = manager.auth_cached().expect("auth should be cached");
    let error = RefreshTokenFailedError::new(
        RefreshTokenFailedReason::Revoked,
        "refresh token invalidated",
    );
    manager.record_permanent_refresh_failure_if_unchanged(&auth, &error);

    manager
        .refresh_token_from_authority()
        .await
        .expect("refresh should succeed by switching to fallback");

    let cached_auth = manager
        .auth_cached()
        .expect("fallback auth should be cached");
    let active_account = cached_auth
        .active_chatgpt_account_summary()
        .expect("fallback account should be active");
    assert_eq!(active_account.store_account_id, fallback_store_account_id);
    assert_eq!(
        cached_auth.get_account_id().as_deref(),
        Some("org-fallback")
    );
    assert_ne!(
        cached_auth.get_account_id().as_deref(),
        Some(fallback_store_account_id.as_str())
    );

    let accounts = manager.list_accounts();
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].id, fallback_store_account_id);
    assert!(accounts[0].is_active);
    assert!(
        accounts
            .iter()
            .all(|account| account.id != active_store_account_id),
        "the invalidated account should be removed from the saved store"
    );
    assert_eq!(manager.refresh_failure_for_auth(&cached_auth), None);
}

#[tokio::test]
async fn refresh_token_from_authority_succeeds_when_external_terminal_failure_switches_to_fallback_store_account()
 {
    let codex_home = tempdir().unwrap();
    let active_store_account_id =
        persist_test_chatgpt_accounts(codex_home.path(), &["org-primary", "org-fallback"], 0);
    let fallback_store_account_id =
        test_store_account_id("org-fallback").expect("fallback store account id");
    let external_store = AuthStore {
        active_account_id: Some(active_store_account_id.clone()),
        accounts: vec![stored_test_chatgpt_account("org-primary", Some(Utc::now()))],
        ..AuthStore::default()
    };
    save_auth(
        codex_home.path(),
        &external_store,
        AuthCredentialsStoreMode::Ephemeral,
    )
    .expect("save external auth store");

    let manager = AuthManager::shared(
        codex_home.path().to_path_buf(),
        false,
        AuthCredentialsStoreMode::File,
    );
    manager.set_external_auth(Arc::new(FailingExternalChatgptAuth {
        error: RefreshTokenFailedError::new(
            RefreshTokenFailedReason::Revoked,
            "refresh token invalidated",
        ),
    }));

    assert_eq!(manager.auth_mode(), Some(AuthMode::ChatgptAuthTokens));

    manager
        .refresh_token_from_authority()
        .await
        .expect("external refresh should succeed by switching to fallback");

    let cached_auth = manager
        .auth_cached()
        .expect("fallback auth should be cached");
    let active_account = cached_auth
        .active_chatgpt_account_summary()
        .expect("fallback account should be active");
    assert_eq!(active_account.store_account_id, fallback_store_account_id);
    assert_eq!(
        cached_auth.get_account_id().as_deref(),
        Some("org-fallback")
    );

    let accounts = manager.list_accounts();
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].id, fallback_store_account_id);
    assert!(accounts[0].is_active);
    assert!(
        accounts
            .iter()
            .all(|account| account.id != active_store_account_id),
        "the invalidated account should be removed from the saved store"
    );
}

#[tokio::test]
async fn unauthorized_recovery_succeeds_when_external_terminal_failure_switches_to_fallback() {
    let codex_home = tempdir().unwrap();
    let active_store_account_id =
        persist_test_chatgpt_accounts(codex_home.path(), &["org-primary", "org-fallback"], 0);
    let fallback_store_account_id =
        test_store_account_id("org-fallback").expect("fallback store account id");
    let external_store = AuthStore {
        active_account_id: Some(active_store_account_id.clone()),
        accounts: vec![stored_test_chatgpt_account("org-primary", Some(Utc::now()))],
        ..AuthStore::default()
    };
    save_auth(
        codex_home.path(),
        &external_store,
        AuthCredentialsStoreMode::Ephemeral,
    )
    .expect("save external auth store");

    let manager = AuthManager::shared(
        codex_home.path().to_path_buf(),
        false,
        AuthCredentialsStoreMode::File,
    );
    manager.set_external_auth(Arc::new(FailingExternalChatgptAuth {
        error: RefreshTokenFailedError::new(
            RefreshTokenFailedReason::Revoked,
            "refresh token invalidated",
        ),
    }));

    let mut recovery = manager.unauthorized_recovery();
    let refresh_result = recovery
        .next()
        .await
        .expect("external refresh step should switch to fallback");
    assert_eq!(refresh_result.auth_state_changed(), Some(true));

    let accounts = manager.list_accounts();
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].id, fallback_store_account_id);
    assert!(accounts[0].is_active);
    assert!(
        accounts
            .iter()
            .all(|account| account.id != active_store_account_id),
        "the invalidated account should be removed from the saved store"
    );
}

#[tokio::test]
async fn auth_does_not_revive_removed_auth_after_terminal_refresh_failure() {
    let codex_home = tempdir().unwrap();
    persist_test_chatgpt_accounts_with_last_refresh(
        codex_home.path(),
        &["org-primary"],
        0,
        Some(Utc::now() - chrono::Duration::days(30)),
    );
    let manager = AuthManager::shared(
        codex_home.path().to_path_buf(),
        false,
        AuthCredentialsStoreMode::File,
    );
    let auth = manager.auth_cached().expect("auth should be cached");
    let error = RefreshTokenFailedError::new(
        RefreshTokenFailedReason::Revoked,
        "refresh token invalidated",
    );
    manager.record_permanent_refresh_failure_if_unchanged(&auth, &error);

    assert_eq!(manager.auth().await, None);
    assert_eq!(manager.auth_cached(), None);
    assert_eq!(manager.list_accounts(), Vec::new());
}

#[tokio::test]
async fn terminal_refresh_failure_does_not_switch_to_api_key_fallback() {
    let codex_home = tempdir().unwrap();
    let active_store_account_id = test_store_account_id("org-primary").expect("store account id");
    let store = AuthStore {
        openai_api_key: Some("sk-test".to_string()),
        active_account_id: Some(active_store_account_id.clone()),
        accounts: vec![stored_test_chatgpt_account(
            "org-primary",
            Some(Utc::now() - chrono::Duration::days(30)),
        )],
        ..AuthStore::default()
    };
    save_auth(codex_home.path(), &store, AuthCredentialsStoreMode::File).expect("save auth store");
    let manager = AuthManager::shared(
        codex_home.path().to_path_buf(),
        false,
        AuthCredentialsStoreMode::File,
    );
    let auth = manager.auth_cached().expect("auth should be cached");
    let error = RefreshTokenFailedError::new(
        RefreshTokenFailedReason::Revoked,
        "refresh token invalidated",
    );
    manager.record_permanent_refresh_failure_if_unchanged(&auth, &error);

    let resolution = manager
        .resolve_chatgpt_auth_for_store_account_id(
            &active_store_account_id,
            ChatgptAccountRefreshMode::Force,
        )
        .await
        .expect("resolution should succeed");

    assert_eq!(
        resolution,
        ChatgptAccountAuthResolution::Removed {
            error,
            switched_to_store_account_id: None,
        }
    );
    assert_eq!(manager.auth_cached(), None);
    assert_eq!(manager.auth().await, None);
}

#[tokio::test]
async fn terminal_refresh_failure_does_not_switch_to_wrong_workspace_fallback() {
    let codex_home = tempdir().unwrap();
    let active_store_account_id =
        persist_test_chatgpt_accounts(codex_home.path(), &["org-primary", "org-fallback"], 0);
    let fallback_store_account_id =
        test_store_account_id("org-fallback").expect("fallback store account id");
    let manager = AuthManager::shared(
        codex_home.path().to_path_buf(),
        false,
        AuthCredentialsStoreMode::File,
    );
    manager.set_forced_chatgpt_workspace_id(Some("org-primary".to_string()));
    let auth = manager.auth_cached().expect("auth should be cached");
    let error = RefreshTokenFailedError::new(
        RefreshTokenFailedReason::Revoked,
        "refresh token invalidated",
    );
    manager.record_permanent_refresh_failure_if_unchanged(&auth, &error);

    let resolution = manager
        .resolve_chatgpt_auth_for_store_account_id(
            &active_store_account_id,
            ChatgptAccountRefreshMode::Force,
        )
        .await
        .expect("resolution should succeed");

    assert_eq!(
        resolution,
        ChatgptAccountAuthResolution::Removed {
            error,
            switched_to_store_account_id: None,
        }
    );
    let accounts = manager.list_accounts();
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].id, fallback_store_account_id);
    assert!(!accounts[0].is_active);
    assert_eq!(manager.auth_cached(), None);
    manager.reload_strict().expect("reload should succeed");
    assert_eq!(manager.auth_cached(), None);
}

#[test]
fn set_active_account_rejects_mismatched_forced_workspace() {
    let codex_home = tempdir().unwrap();
    let active_store_account_id =
        persist_test_chatgpt_accounts(codex_home.path(), &["org-primary", "org-fallback"], 0);
    let fallback_store_account_id =
        test_store_account_id("org-fallback").expect("fallback store account id");
    let manager = AuthManager::shared(
        codex_home.path().to_path_buf(),
        false,
        AuthCredentialsStoreMode::File,
    );
    manager.set_forced_chatgpt_workspace_id(Some("org-primary".to_string()));

    let err = manager
        .set_active_account(&fallback_store_account_id)
        .expect_err("workspace-mismatched account should be rejected");

    assert!(
        err.to_string()
            .contains("does not match required workspace \"org-primary\""),
        "unexpected error: {err}"
    );
    let active_summary = manager
        .active_chatgpt_account_summary()
        .expect("active account should remain available");
    assert_eq!(active_summary.store_account_id, active_store_account_id);
}

#[test]
fn external_auth_tokens_without_chatgpt_metadata_cannot_seed_chatgpt_auth() {
    let err = AuthDotJson::from_external_tokens(
        &ExternalAuthTokens::access_token_only("test-access-token"),
        None,
    )
    .expect_err("bearer-only external auth should not seed ChatGPT auth");

    assert_eq!(
        err.to_string(),
        "external auth tokens are missing ChatGPT metadata"
    );
}

#[tokio::test]
async fn external_bearer_only_auth_manager_uses_cached_provider_token() {
    let script = ProviderAuthScript::new(&["provider-token", "next-token"]).unwrap();
    let manager = AuthManager::external_bearer_only(script.auth_config());

    let first = manager
        .auth()
        .await
        .and_then(|auth| auth.api_key().map(str::to_string));
    let second = manager
        .auth()
        .await
        .and_then(|auth| auth.api_key().map(str::to_string));

    assert_eq!(first.as_deref(), Some("provider-token"));
    assert_eq!(second.as_deref(), Some("provider-token"));
    assert_eq!(manager.auth_mode(), Some(AuthMode::ApiKey));
    assert_eq!(manager.get_api_auth_mode(), Some(ApiAuthMode::ApiKey));
}

#[tokio::test]
async fn external_bearer_only_auth_manager_disables_auto_refresh_when_interval_is_zero() {
    let script = ProviderAuthScript::new(&["provider-token", "next-token"]).unwrap();
    let mut auth_config = script.auth_config();
    auth_config.refresh_interval_ms = 0;
    let manager = AuthManager::external_bearer_only(auth_config);

    let first = manager
        .auth()
        .await
        .and_then(|auth| auth.api_key().map(str::to_string));
    let second = manager
        .auth()
        .await
        .and_then(|auth| auth.api_key().map(str::to_string));

    assert_eq!(first.as_deref(), Some("provider-token"));
    assert_eq!(second.as_deref(), Some("provider-token"));
}

#[tokio::test]
async fn external_bearer_only_auth_manager_returns_none_when_command_fails() {
    let script = ProviderAuthScript::new_failing().unwrap();
    let manager = AuthManager::external_bearer_only(script.auth_config());

    assert_eq!(manager.auth().await, None);
}

#[tokio::test]
async fn unauthorized_recovery_uses_external_refresh_for_bearer_manager() {
    let script = ProviderAuthScript::new(&["provider-token", "refreshed-provider-token"]).unwrap();
    let mut auth_config = script.auth_config();
    auth_config.refresh_interval_ms = 0;
    let manager = AuthManager::external_bearer_only(auth_config);
    let initial_token = manager
        .auth()
        .await
        .and_then(|auth| auth.api_key().map(str::to_string));
    let mut recovery = manager.unauthorized_recovery();

    assert!(recovery.has_next());
    assert_eq!(recovery.mode_name(), "external");
    assert_eq!(recovery.step_name(), "external_refresh");

    let result = recovery
        .next()
        .await
        .expect("external refresh should succeed");

    assert_eq!(result.auth_state_changed(), Some(true));
    let refreshed_token = manager
        .auth()
        .await
        .and_then(|auth| auth.api_key().map(str::to_string));
    assert_eq!(initial_token.as_deref(), Some("provider-token"));
    assert_eq!(refreshed_token.as_deref(), Some("refreshed-provider-token"));
}

struct ProviderAuthScript {
    tempdir: TempDir,
    command: String,
    args: Vec<String>,
}

impl ProviderAuthScript {
    fn new(tokens: &[&str]) -> std::io::Result<Self> {
        let tempdir = tempfile::tempdir()?;
        let token_file = tempdir.path().join("tokens.txt");
        // `cmd.exe`'s `set /p` treats LF-only input as one line, so use CRLF on Windows.
        let token_line_ending = if cfg!(windows) { "\r\n" } else { "\n" };
        let mut token_file_contents = String::new();
        for token in tokens {
            token_file_contents.push_str(token);
            token_file_contents.push_str(token_line_ending);
        }
        std::fs::write(&token_file, token_file_contents)?;

        #[cfg(unix)]
        let (command, args) = {
            let script_path = tempdir.path().join("print-token.sh");
            std::fs::write(
                &script_path,
                r#"#!/bin/sh
first_line=$(sed -n '1p' tokens.txt)
printf '%s\n' "$first_line"
tail -n +2 tokens.txt > tokens.next
mv tokens.next tokens.txt
"#,
            )?;
            let mut permissions = std::fs::metadata(&script_path)?.permissions();
            {
                use std::os::unix::fs::PermissionsExt;
                permissions.set_mode(0o755);
            }
            std::fs::set_permissions(&script_path, permissions)?;
            ("./print-token.sh".to_string(), Vec::new())
        };

        #[cfg(windows)]
        let (command, args) = {
            let script_path = tempdir.path().join("print-token.cmd");
            std::fs::write(
                &script_path,
                r#"@echo off
setlocal EnableExtensions DisableDelayedExpansion
set "first_line="
<tokens.txt set /p "first_line="
if not defined first_line exit /b 1
setlocal EnableDelayedExpansion
echo(!first_line!
endlocal
more +1 tokens.txt > tokens.next
move /y tokens.next tokens.txt >nul
"#,
            )?;
            (
                "cmd.exe".to_string(),
                vec![
                    "/d".to_string(),
                    "/s".to_string(),
                    "/c".to_string(),
                    ".\\print-token.cmd".to_string(),
                ],
            )
        };

        Ok(Self {
            tempdir,
            command,
            args,
        })
    }

    fn new_failing() -> std::io::Result<Self> {
        let tempdir = tempfile::tempdir()?;

        #[cfg(unix)]
        let (command, args) = {
            let script_path = tempdir.path().join("fail.sh");
            std::fs::write(
                &script_path,
                r#"#!/bin/sh
exit 1
"#,
            )?;
            let mut permissions = std::fs::metadata(&script_path)?.permissions();
            {
                use std::os::unix::fs::PermissionsExt;
                permissions.set_mode(0o755);
            }
            std::fs::set_permissions(&script_path, permissions)?;
            ("./fail.sh".to_string(), Vec::new())
        };

        #[cfg(windows)]
        let (command, args) = (
            "cmd.exe".to_string(),
            vec![
                "/d".to_string(),
                "/s".to_string(),
                "/c".to_string(),
                "exit /b 1".to_string(),
            ],
        );

        Ok(Self {
            tempdir,
            command,
            args,
        })
    }

    fn auth_config(&self) -> ModelProviderAuthInfo {
        serde_json::from_value(json!({
            "command": self.command,
            "args": self.args,
            // Process startup can be slow on loaded Windows CI workers, so leave enough slack to
            // avoid turning these auth-cache assertions into a process-launch timing test.
            "timeout_ms": 10_000,
            "refresh_interval_ms": 60000,
            "cwd": self.tempdir.path(),
        }))
        .expect("provider auth config should deserialize")
    }
}

struct AuthFileParams {
    openai_api_key: Option<String>,
    chatgpt_plan_type: Option<String>,
    chatgpt_account_id: Option<String>,
}

fn make_test_chatgpt_jwt(
    chatgpt_plan_type: Option<String>,
    chatgpt_account_id: Option<String>,
) -> std::io::Result<String> {
    fake_jwt_for_auth_file_params(&AuthFileParams {
        openai_api_key: None,
        chatgpt_plan_type,
        chatgpt_account_id,
    })
}

fn write_auth_file(params: AuthFileParams, codex_home: &Path) -> std::io::Result<String> {
    let fake_jwt = fake_jwt_for_auth_file_params(&params)?;
    let auth_file = get_auth_file(codex_home);
    let auth_json_data = json!({
        "OPENAI_API_KEY": params.openai_api_key,
        "tokens": {
            "id_token": fake_jwt,
            "access_token": "test-access-token",
            "refresh_token": "test-refresh-token",
            "account_id": params.chatgpt_account_id,
        },
        "last_refresh": Utc::now(),
    });
    let auth_json = serde_json::to_string_pretty(&auth_json_data)?;
    std::fs::write(auth_file, auth_json)?;
    Ok(fake_jwt)
}

fn fake_jwt_for_auth_file_params(params: &AuthFileParams) -> std::io::Result<String> {
    #[derive(Serialize)]
    struct Header {
        alg: &'static str,
        typ: &'static str,
    }

    let header = Header {
        alg: "none",
        typ: "JWT",
    };
    let mut auth_payload = serde_json::json!({
        "chatgpt_user_id": "user-12345",
        "user_id": "user-12345",
    });

    if let Some(chatgpt_plan_type) = params.chatgpt_plan_type.as_ref() {
        auth_payload["chatgpt_plan_type"] = serde_json::Value::String(chatgpt_plan_type.clone());
    }

    if let Some(chatgpt_account_id) = params.chatgpt_account_id.as_ref() {
        auth_payload["chatgpt_account_id"] = serde_json::Value::String(chatgpt_account_id.clone());
    }

    let payload = serde_json::json!({
        "email": "user@example.com",
        "email_verified": true,
        "https://api.openai.com/auth": auth_payload,
    });
    let b64 = |b: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b);
    let header_b64 = b64(&serde_json::to_vec(&header)?);
    let payload_b64 = b64(&serde_json::to_vec(&payload)?);
    let signature_b64 = b64(b"sig");
    Ok(format!("{header_b64}.{payload_b64}.{signature_b64}"))
}

fn test_chatgpt_token_data(chatgpt_account_id: &str) -> TokenData {
    let access_token = make_test_chatgpt_jwt(
        Some("pro".to_string()),
        Some(chatgpt_account_id.to_string()),
    )
    .expect("jwt");
    TokenData {
        id_token: crate::token_data::parse_chatgpt_jwt_claims(&access_token).expect("id token"),
        access_token,
        refresh_token: format!("refresh-{chatgpt_account_id}"),
        account_id: Some(chatgpt_account_id.to_string()),
    }
}

fn test_store_account_id(chatgpt_account_id: &str) -> Option<String> {
    test_chatgpt_token_data(chatgpt_account_id).preferred_store_account_id()
}

fn stored_test_chatgpt_account(
    chatgpt_account_id: &str,
    last_refresh: Option<DateTime<Utc>>,
) -> StoredAccount {
    let tokens = test_chatgpt_token_data(chatgpt_account_id);
    let store_account_id = tokens
        .preferred_store_account_id()
        .expect("store account id");
    StoredAccount {
        id: store_account_id,
        label: None,
        tokens,
        last_refresh,
        usage: None,
    }
}

fn stored_test_chatgpt_account_with_usage(
    chatgpt_account_id: &str,
    primary_remaining_percent: f64,
    weekly_remaining_percent: f64,
    captured_at: DateTime<Utc>,
) -> StoredAccount {
    let mut account = stored_test_chatgpt_account(chatgpt_account_id, Some(captured_at));
    account.usage = Some(AccountUsageCache {
        last_rate_limits: Some(test_rate_limit_snapshot(
            primary_remaining_percent,
            weekly_remaining_percent,
            captured_at,
        )),
        exhausted_until: None,
        last_seen_at: Some(captured_at),
    });
    account
}

fn test_rate_limit_snapshot(
    primary_remaining_percent: f64,
    weekly_remaining_percent: f64,
    captured_at: DateTime<Utc>,
) -> RateLimitSnapshot {
    RateLimitSnapshot {
        limit_id: Some("codex".to_string()),
        limit_name: None,
        primary: Some(RateLimitWindow {
            remaining_percent: primary_remaining_percent,
            window_minutes: Some(300),
            resets_at: Some((captured_at + chrono::Duration::hours(5)).timestamp()),
        }),
        secondary: Some(RateLimitWindow {
            remaining_percent: weekly_remaining_percent,
            window_minutes: None,
            resets_at: Some((captured_at + chrono::Duration::days(7)).timestamp()),
        }),
        credits: None,
        plan_type: Some(AccountPlanType::Pro),
    }
}

fn persist_test_chatgpt_accounts(
    codex_home: &Path,
    chatgpt_account_ids: &[&str],
    active_index: usize,
) -> String {
    persist_test_chatgpt_accounts_with_last_refresh(
        codex_home,
        chatgpt_account_ids,
        active_index,
        Some(Utc::now()),
    )
}

fn persist_test_chatgpt_accounts_with_last_refresh(
    codex_home: &Path,
    chatgpt_account_ids: &[&str],
    active_index: usize,
    last_refresh: Option<DateTime<Utc>>,
) -> String {
    let accounts = chatgpt_account_ids
        .iter()
        .map(|chatgpt_account_id| stored_test_chatgpt_account(chatgpt_account_id, last_refresh))
        .collect::<Vec<_>>();
    let active_account_id = accounts
        .get(active_index)
        .map(|account| account.id.clone())
        .expect("active account id");
    let store = AuthStore {
        active_account_id: Some(active_account_id.clone()),
        accounts,
        ..AuthStore::default()
    };
    save_auth(codex_home, &store, AuthCredentialsStoreMode::File).expect("save auth store");
    active_account_id
}

async fn build_config(
    codex_home: &Path,
    forced_login_method: Option<ForcedLoginMethod>,
    forced_chatgpt_workspace_id: Option<String>,
) -> AuthConfig {
    AuthConfig {
        codex_home: codex_home.to_path_buf(),
        auth_credentials_store_mode: AuthCredentialsStoreMode::File,
        forced_login_method,
        forced_chatgpt_workspace_id,
    }
}

/// Use sparingly.
/// TODO (gpeal): replace this with an injectable env var provider.
#[cfg(test)]
struct EnvVarGuard {
    key: &'static str,
    original: Option<std::ffi::OsString>,
}

#[cfg(test)]
impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let original = env::var_os(key);
        unsafe {
            env::set_var(key, value);
        }
        Self { key, original }
    }
}

#[cfg(test)]
impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        unsafe {
            match &self.original {
                Some(value) => env::set_var(self.key, value),
                None => env::remove_var(self.key),
            }
        }
    }
}

#[tokio::test]
async fn enforce_login_restrictions_logs_out_for_method_mismatch() {
    let codex_home = tempdir().unwrap();
    login_with_api_key(codex_home.path(), "sk-test", AuthCredentialsStoreMode::File)
        .expect("seed api key");

    let config = build_config(
        codex_home.path(),
        Some(ForcedLoginMethod::Chatgpt),
        /*forced_chatgpt_workspace_id*/ None,
    )
    .await;

    let err =
        super::enforce_login_restrictions(&config).expect_err("expected method mismatch to error");
    assert!(err.to_string().contains("ChatGPT login is required"));
    assert!(
        !codex_home.path().join("auth.json").exists(),
        "auth.json should be removed on mismatch"
    );
}

#[tokio::test]
#[serial(codex_api_key)]
async fn enforce_login_restrictions_logs_out_for_workspace_mismatch() {
    let codex_home = tempdir().unwrap();
    let _jwt = write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("pro".to_string()),
            chatgpt_account_id: Some("org_another_org".to_string()),
        },
        codex_home.path(),
    )
    .expect("failed to write auth file");

    let config = build_config(
        codex_home.path(),
        /*forced_login_method*/ None,
        Some("org_mine".to_string()),
    )
    .await;

    let err = super::enforce_login_restrictions(&config)
        .expect_err("expected workspace mismatch to error");
    assert!(err.to_string().contains("workspace org_mine"));
    assert!(
        !codex_home.path().join("auth.json").exists(),
        "auth.json should be removed on mismatch"
    );
}

#[tokio::test]
#[serial(codex_api_key)]
async fn enforce_login_restrictions_allows_matching_workspace() {
    let codex_home = tempdir().unwrap();
    let _jwt = write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("pro".to_string()),
            chatgpt_account_id: Some("org_mine".to_string()),
        },
        codex_home.path(),
    )
    .expect("failed to write auth file");

    let config = build_config(
        codex_home.path(),
        /*forced_login_method*/ None,
        Some("org_mine".to_string()),
    )
    .await;

    super::enforce_login_restrictions(&config).expect("matching workspace should succeed");
    assert!(
        codex_home.path().join("auth.json").exists(),
        "auth.json should remain when restrictions pass"
    );
}

#[tokio::test]
async fn enforce_login_restrictions_allows_api_key_if_login_method_not_set_but_forced_chatgpt_workspace_id_is_set()
 {
    let codex_home = tempdir().unwrap();
    login_with_api_key(codex_home.path(), "sk-test", AuthCredentialsStoreMode::File)
        .expect("seed api key");

    let config = build_config(
        codex_home.path(),
        /*forced_login_method*/ None,
        Some("org_mine".to_string()),
    )
    .await;

    super::enforce_login_restrictions(&config).expect("matching workspace should succeed");
    assert!(
        codex_home.path().join("auth.json").exists(),
        "auth.json should remain when restrictions pass"
    );
}

#[tokio::test]
#[serial(codex_api_key)]
async fn enforce_login_restrictions_blocks_env_api_key_when_chatgpt_required() {
    let _guard = EnvVarGuard::set(CODEX_API_KEY_ENV_VAR, "sk-env");
    let codex_home = tempdir().unwrap();

    let config = build_config(
        codex_home.path(),
        Some(ForcedLoginMethod::Chatgpt),
        /*forced_chatgpt_workspace_id*/ None,
    )
    .await;

    let err = super::enforce_login_restrictions(&config)
        .expect_err("environment API key should not satisfy forced ChatGPT login");
    assert!(
        err.to_string()
            .contains("ChatGPT login is required, but an API key is currently being used.")
    );
}

#[test]
fn plan_type_maps_known_plan() {
    let codex_home = tempdir().unwrap();
    let _jwt = write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("pro".to_string()),
            chatgpt_account_id: None,
        },
        codex_home.path(),
    )
    .expect("failed to write auth file");

    let auth = super::load_auth(
        codex_home.path(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
    )
    .expect("load auth")
    .expect("auth available");

    pretty_assertions::assert_eq!(auth.account_plan_type(), Some(AccountPlanType::Pro));
}

#[test]
fn plan_type_maps_self_serve_business_usage_based_plan() {
    let codex_home = tempdir().unwrap();
    let _jwt = write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("self_serve_business_usage_based".to_string()),
            chatgpt_account_id: None,
        },
        codex_home.path(),
    )
    .expect("failed to write auth file");

    let auth = super::load_auth(
        codex_home.path(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
    )
    .expect("load auth")
    .expect("auth available");

    pretty_assertions::assert_eq!(
        auth.account_plan_type(),
        Some(AccountPlanType::SelfServeBusinessUsageBased)
    );
}

#[test]
fn plan_type_maps_enterprise_cbp_usage_based_plan() {
    let codex_home = tempdir().unwrap();
    let _jwt = write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("enterprise_cbp_usage_based".to_string()),
            chatgpt_account_id: None,
        },
        codex_home.path(),
    )
    .expect("failed to write auth file");

    let auth = super::load_auth(
        codex_home.path(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
    )
    .expect("load auth")
    .expect("auth available");

    pretty_assertions::assert_eq!(
        auth.account_plan_type(),
        Some(AccountPlanType::EnterpriseCbpUsageBased)
    );
}

#[test]
fn plan_type_maps_unknown_to_unknown() {
    let codex_home = tempdir().unwrap();
    let _jwt = write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("mystery-tier".to_string()),
            chatgpt_account_id: None,
        },
        codex_home.path(),
    )
    .expect("failed to write auth file");

    let auth = super::load_auth(
        codex_home.path(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
    )
    .expect("load auth")
    .expect("auth available");

    pretty_assertions::assert_eq!(auth.account_plan_type(), Some(AccountPlanType::Unknown));
}

#[test]
fn missing_plan_type_maps_to_unknown() {
    let codex_home = tempdir().unwrap();
    let _jwt = write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: None,
            chatgpt_account_id: None,
        },
        codex_home.path(),
    )
    .expect("failed to write auth file");

    let auth = super::load_auth(
        codex_home.path(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
    )
    .expect("load auth")
    .expect("auth available");

    pretty_assertions::assert_eq!(auth.account_plan_type(), Some(AccountPlanType::Unknown));
}
