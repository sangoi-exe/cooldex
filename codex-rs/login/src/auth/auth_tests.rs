use super::*;
use crate::auth::storage::FileAuthStorage;
use crate::auth::storage::get_auth_file;
use crate::token_data::IdTokenInfo;
use codex_app_server_protocol::AuthMode;
use codex_protocol::account::PlanType as AccountPlanType;
use codex_protocol::auth::KnownPlan as InternalKnownPlan;
use codex_protocol::auth::PlanType as InternalPlanType;

use base64::Engine;
use codex_protocol::config_types::ForcedLoginMethod;
use codex_protocol::config_types::ModelProviderAuthInfo;
use pretty_assertions::assert_eq;
use serde::Serialize;
use serde_json::json;
use std::collections::HashSet;
use std::sync::Arc;
use tempfile::TempDir;
use tempfile::tempdir;

// Merge-safety anchor: auth test fixtures must keep workspace account/token contracts aligned
// with the customized ChatGPT account persistence and refresh semantics.

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

#[test]
fn usage_limit_auto_switch_removes_only_free_and_unknown_plans() {
    assert!(super::usage_limit_auto_switch_removes_plan_type(Some(
        &AccountPlanType::Free
    )));
    assert!(super::usage_limit_auto_switch_removes_plan_type(Some(
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
        &AccountPlanType::Edu
    )));
    assert!(!super::usage_limit_auto_switch_removes_plan_type(None));
}

#[test]
fn mark_usage_limit_reached_prefers_blocked_primary_reset_when_error_reset_is_missing() {
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
                    used_percent: 100.0,
                    window_minutes: Some(15),
                    resets_at: Some(primary_reset_at.timestamp()),
                }),
                secondary: Some(RateLimitWindow {
                    used_percent: 10.0,
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
}

#[test]
fn cooldown_still_purges_freshly_unsupported_fallbacks_and_marks_failing_account() {
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
        .switch_account_on_usage_limit(
            None,
            Some(active_store_account_id.as_str()),
            None,
            Some(RateLimitSnapshot {
                limit_id: Some("codex".to_string()),
                limit_name: None,
                primary: Some(RateLimitWindow {
                    used_percent: 100.0,
                    window_minutes: Some(15),
                    resets_at: Some((Utc::now() + chrono::Duration::minutes(15)).timestamp()),
                }),
                secondary: None,
                credits: None,
                plan_type: Some(AccountPlanType::Pro),
            }),
            &HashSet::from([fallback_store_account_id.clone()]),
            None,
        )
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
        "freshly unsupported fallback should be purged even during cooldown"
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
fn select_account_for_auto_switch_prefers_lower_primary_usage_when_weekly_ties() {
    let now = Utc::now();
    let stronger_store_account_id =
        test_store_account_id("org-stronger").expect("stronger store account id");
    let store = AuthStore {
        accounts: vec![
            stored_test_chatgpt_account_with_usage("org-weaker", 80.0, 20.0, now),
            stored_test_chatgpt_account_with_usage("org-stronger", 20.0, 20.0, now),
        ],
        ..AuthStore::default()
    };

    assert_eq!(
        super::select_account_for_auto_switch_from_store(&store, None, None, now).as_deref(),
        Some(stronger_store_account_id.as_str())
    );
}

#[test]
fn select_account_for_auto_switch_prefers_lower_weekly_usage_when_primary_ties() {
    let now = Utc::now();
    let stronger_store_account_id =
        test_store_account_id("org-stronger").expect("stronger store account id");
    let store = AuthStore {
        accounts: vec![
            stored_test_chatgpt_account_with_usage("org-weaker", 20.0, 80.0, now),
            stored_test_chatgpt_account_with_usage("org-stronger", 20.0, 20.0, now),
        ],
        ..AuthStore::default()
    };

    assert_eq!(
        super::select_account_for_auto_switch_from_store(&store, None, None, now).as_deref(),
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
            stored_test_chatgpt_account_with_usage("org-weekly-favored", 95.0, 90.0, now),
            stored_test_chatgpt_account_with_usage("org-primary-favored", 10.0, 20.0, now),
        ],
        ..AuthStore::default()
    };

    assert_eq!(
        super::select_account_for_auto_switch_from_store(&store, None, None, now).as_deref(),
        Some(stronger_store_account_id.as_str())
    );
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
    primary_used_percent: f64,
    weekly_used_percent: f64,
    captured_at: DateTime<Utc>,
) -> StoredAccount {
    let mut account = stored_test_chatgpt_account(chatgpt_account_id, Some(captured_at));
    account.usage = Some(AccountUsageCache {
        last_rate_limits: Some(test_rate_limit_snapshot(
            primary_used_percent,
            weekly_used_percent,
            captured_at,
        )),
        exhausted_until: None,
        last_seen_at: Some(captured_at),
    });
    account
}

fn test_rate_limit_snapshot(
    primary_used_percent: f64,
    weekly_used_percent: f64,
    captured_at: DateTime<Utc>,
) -> RateLimitSnapshot {
    RateLimitSnapshot {
        limit_id: Some("codex".to_string()),
        limit_name: None,
        primary: Some(RateLimitWindow {
            used_percent: primary_used_percent,
            window_minutes: Some(300),
            resets_at: Some((captured_at + chrono::Duration::hours(5)).timestamp()),
        }),
        secondary: Some(RateLimitWindow {
            used_percent: weekly_used_percent,
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
