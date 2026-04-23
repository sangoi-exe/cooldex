use super::*;
use crate::auth::storage::FileAuthStorage;
use crate::auth::storage::get_auth_file;
use crate::token_data::IdTokenInfo;
use chrono::Utc;
use codex_account_state::AccountUsageState;
use codex_app_server_protocol::AuthMode;
use codex_protocol::account::PlanType as AccountPlanType;
use codex_protocol::auth::KnownPlan as InternalKnownPlan;
use codex_protocol::auth::PlanType as InternalPlanType;
use codex_protocol::protocol::RateLimitSnapshot;
use codex_protocol::protocol::RateLimitWindow;

use base64::Engine;
use codex_protocol::config_types::ForcedLoginMethod;
use codex_protocol::config_types::ModelProviderAuthInfo;
use pretty_assertions::assert_eq;
use serde::Serialize;
use serde_json::json;
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use tempfile::TempDir;
use tempfile::tempdir;
use tokio::time::Duration;
use tokio::time::timeout;

// Merge-safety anchor: auth tests must exercise the AuthStore-backed persistence surface
// and current constructor paths rather than removed legacy-only helpers.

#[tokio::test]
async fn refresh_without_id_token() {
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

    let storage = create_auth_storage(
        codex_home.path().to_path_buf(),
        AuthCredentialsStoreMode::File,
    );
    let store_account_id = storage
        .load()
        .expect("auth store should load")
        .expect("auth store should exist")
        .active_account()
        .expect("active account should exist")
        .id
        .clone();
    let updated = super::update_tokens(
        codex_home.path(),
        &storage,
        &store_account_id,
        /*id_token*/ None,
        Some("new-access-token".to_string()),
        Some("new-refresh-token".to_string()),
        PersistedActiveAccountWriteMode::Preserve,
    )
    .await
    .expect("update_tokens should succeed");

    let tokens = &updated
        .active_account()
        .expect("active account should exist")
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
        "stored accounts should be cleared"
    );
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

    let auth_dot_json = auth
        .get_current_auth_json()
        .expect("AuthDotJson should exist");
    let last_refresh = auth_dot_json
        .last_refresh
        .expect("last_refresh should be recorded");

    assert_eq!(
        AuthDotJson {
            auth_mode: None,
            openai_api_key: None,
            tokens: Some(TokenData {
                id_token: IdTokenInfo {
                    email: Some("user@example.com".to_string()),
                    chatgpt_plan_type: Some(InternalPlanType::Known(InternalKnownPlan::Pro)),
                    chatgpt_user_id: Some("user-12345".to_string()),
                    chatgpt_account_id: None,
                    chatgpt_account_is_fedramp: false,
                    raw_jwt: fake_jwt,
                },
                access_token: "test-access-token".to_string(),
                refresh_token: "test-refresh-token".to_string(),
                account_id: None,
            }),
            last_refresh: Some(last_refresh),
            agent_identity: None,
        },
        auth_dot_json
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
    let auth_dot_json = AuthDotJson {
        auth_mode: Some(ApiAuthMode::ApiKey),
        openai_api_key: Some("sk-test-key".to_string()),
        tokens: None,
        last_refresh: None,
        agent_identity: None,
    };
    let auth_store = AuthStore::from_legacy(auth_dot_json);
    super::save_auth(dir.path(), &auth_store, AuthCredentialsStoreMode::File)?;
    let auth_file = get_auth_file(dir.path());
    assert!(auth_file.exists());
    assert!(logout(dir.path(), AuthCredentialsStoreMode::File)?);
    assert!(!auth_file.exists());
    Ok(())
}

fn chatgpt_auth_store_for_manager_logout(
    store_account_id: &str,
    workspace_id: &str,
    access_token: &str,
    refresh_token: &str,
) -> AuthStore {
    AuthStore {
        active_account_id: Some(store_account_id.to_string()),
        accounts: vec![StoredAccount {
            id: store_account_id.to_string(),
            label: Some("Primary".to_string()),
            tokens: TokenData {
                id_token: IdTokenInfo {
                    email: Some("primary@example.com".to_string()),
                    chatgpt_plan_type: Some(InternalPlanType::Known(InternalKnownPlan::Plus)),
                    chatgpt_user_id: Some("user-12345".to_string()),
                    chatgpt_account_id: Some(workspace_id.to_string()),
                    chatgpt_account_is_fedramp: false,
                    raw_jwt: "test.header.payload".to_string(),
                },
                access_token: access_token.to_string(),
                refresh_token: refresh_token.to_string(),
                account_id: Some(workspace_id.to_string()),
            },
            last_refresh: None,
            usage: None,
        }],
        ..AuthStore::default()
    }
}

fn external_chatgpt_auth_store(store_account_id: &str, workspace_id: &str) -> AuthStore {
    external_chatgpt_auth_store_with_plan(
        store_account_id,
        workspace_id,
        Some(InternalPlanType::Known(InternalKnownPlan::Plus)),
    )
}

fn external_chatgpt_auth_store_with_plan(
    store_account_id: &str,
    workspace_id: &str,
    chatgpt_plan_type: Option<InternalPlanType>,
) -> AuthStore {
    AuthStore {
        active_account_id: Some(store_account_id.to_string()),
        accounts: vec![StoredAccount {
            id: store_account_id.to_string(),
            label: Some("External".to_string()),
            tokens: TokenData {
                id_token: IdTokenInfo {
                    email: Some("external@example.com".to_string()),
                    chatgpt_plan_type,
                    chatgpt_user_id: Some("user-12345".to_string()),
                    chatgpt_account_id: Some(workspace_id.to_string()),
                    chatgpt_account_is_fedramp: false,
                    raw_jwt: "external.header.payload".to_string(),
                },
                access_token: "external-access-token".to_string(),
                refresh_token: "external-refresh-token".to_string(),
                account_id: Some(workspace_id.to_string()),
            },
            last_refresh: None,
            usage: None,
        }],
        ..AuthStore::default()
    }
}

#[test]
fn auth_manager_logout_releases_runtime_active_account_lease() {
    let dir = tempdir().unwrap();
    let store_account_id = "store-account-a";
    save_auth(
        dir.path(),
        &chatgpt_auth_store_for_manager_logout(
            store_account_id,
            "workspace-a",
            "access-token",
            "refresh-token",
        ),
        AuthCredentialsStoreMode::File,
    )
    .expect("save auth store");
    let manager = AuthManager::new_with_sqlite_home(
        dir.path().to_path_buf(),
        dir.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
    );

    assert!(
        manager
            .account_manager
            .account_state_store
            .as_ref()
            .expect("account-state store should open")
            .account_is_leased_by_other("other-session", None, store_account_id, Utc::now())
            .expect("lease lookup should succeed")
    );

    assert!(manager.logout().expect("logout should succeed"));

    assert!(
        !manager
            .account_manager
            .account_state_store
            .as_ref()
            .expect("account-state store should remain open")
            .account_is_leased_by_other("other-session", None, store_account_id, Utc::now())
            .expect("lease lookup should succeed")
    );
}

#[tokio::test]
async fn auth_manager_logout_with_revoke_releases_runtime_active_account_lease() {
    let dir = tempdir().unwrap();
    let store_account_id = "store-account-a";
    save_auth(
        dir.path(),
        &chatgpt_auth_store_for_manager_logout(store_account_id, "workspace-a", "", ""),
        AuthCredentialsStoreMode::File,
    )
    .expect("save auth store");
    let manager = AuthManager::new_with_sqlite_home(
        dir.path().to_path_buf(),
        dir.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
    );

    assert!(
        manager
            .account_manager
            .account_state_store
            .as_ref()
            .expect("account-state store should open")
            .account_is_leased_by_other("other-session", None, store_account_id, Utc::now())
            .expect("lease lookup should succeed")
    );

    assert!(
        manager
            .logout_with_revoke()
            .await
            .expect("logout with revoke should succeed")
    );

    assert!(
        !manager
            .account_manager
            .account_state_store
            .as_ref()
            .expect("account-state store should remain open")
            .account_is_leased_by_other("other-session", None, store_account_id, Utc::now())
            .expect("lease lookup should succeed")
    );
}

#[tokio::test]
async fn resolve_chatgpt_auth_for_store_account_id_reads_latest_persisted_tokens() {
    let codex_home = tempdir().expect("create auth tempdir");
    let sqlite_home = tempdir().expect("create sqlite tempdir");
    let store_account_id = "store-account-a";
    let workspace_id = "workspace-a";
    let raw_jwt = fake_jwt_for_auth_file_params(&AuthFileParams {
        openai_api_key: None,
        chatgpt_plan_type: Some("plus".to_string()),
        chatgpt_account_id: Some(workspace_id.to_string()),
    })
    .expect("create test jwt");
    let make_store = |access_token: &str, refresh_token: &str| AuthStore {
        active_account_id: Some(store_account_id.to_string()),
        accounts: vec![StoredAccount {
            id: store_account_id.to_string(),
            label: Some("Primary".to_string()),
            tokens: TokenData {
                id_token: IdTokenInfo {
                    email: Some("primary@example.com".to_string()),
                    chatgpt_plan_type: Some(InternalPlanType::Known(InternalKnownPlan::Plus)),
                    chatgpt_user_id: Some("user-12345".to_string()),
                    chatgpt_account_id: Some(workspace_id.to_string()),
                    chatgpt_account_is_fedramp: false,
                    raw_jwt: raw_jwt.clone(),
                },
                access_token: access_token.to_string(),
                refresh_token: refresh_token.to_string(),
                account_id: Some(workspace_id.to_string()),
            },
            last_refresh: Some(Utc::now()),
            usage: None,
        }],
        ..AuthStore::default()
    };
    save_auth(
        codex_home.path(),
        &make_store("old-access-token", "old-refresh-token"),
        AuthCredentialsStoreMode::File,
    )
    .expect("save initial auth store");

    let manager = AuthManager::new_with_sqlite_home(
        codex_home.path().to_path_buf(),
        sqlite_home.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
    );
    let cached_auth = manager.auth_cached().expect("cached auth should exist");
    assert_eq!(
        cached_auth
            .get_token_data()
            .expect("cached token data should exist")
            .access_token,
        "old-access-token"
    );

    save_auth(
        codex_home.path(),
        &make_store("new-access-token", "new-refresh-token"),
        AuthCredentialsStoreMode::File,
    )
    .expect("overwrite auth store with refreshed tokens");

    let resolution = manager
        .resolve_chatgpt_auth_for_store_account_id(
            store_account_id,
            ChatgptAccountRefreshMode::Never,
        )
        .await
        .expect("stored account should still resolve");
    let ChatgptAccountAuthResolution::Auth(auth) = resolution else {
        panic!("stored account should resolve without removal");
    };
    let token_data = auth
        .get_token_data()
        .expect("resolved token data should exist");
    assert_eq!(token_data.access_token, "new-access-token");
    assert_eq!(token_data.refresh_token, "new-refresh-token");
}

#[test]
fn public_auto_switch_selector_reads_sqlite_usage_truth_after_manager_construction() {
    let codex_home = tempdir().expect("create auth tempdir");
    let sqlite_home = tempdir().expect("create sqlite tempdir");
    let workspace_id = "workspace-a";
    let primary_store_account_id = "store-account-a";
    let fallback_store_account_id = "store-account-b";
    let raw_jwt = fake_jwt_for_auth_file_params(&AuthFileParams {
        openai_api_key: None,
        chatgpt_plan_type: Some("plus".to_string()),
        chatgpt_account_id: Some(workspace_id.to_string()),
    })
    .expect("create test jwt");
    let make_account = |store_account_id: &str, label: &str| StoredAccount {
        id: store_account_id.to_string(),
        label: Some(label.to_string()),
        tokens: TokenData {
            id_token: IdTokenInfo {
                email: Some(format!("{store_account_id}@example.com")),
                chatgpt_plan_type: Some(InternalPlanType::Known(InternalKnownPlan::Plus)),
                chatgpt_user_id: Some(format!("user-{store_account_id}")),
                chatgpt_account_id: Some(workspace_id.to_string()),
                chatgpt_account_is_fedramp: false,
                raw_jwt: raw_jwt.clone(),
            },
            access_token: format!("{store_account_id}-access-token"),
            refresh_token: format!("{store_account_id}-refresh-token"),
            account_id: Some(workspace_id.to_string()),
        },
        last_refresh: None,
        usage: None,
    };
    save_auth(
        codex_home.path(),
        &AuthStore {
            active_account_id: Some(primary_store_account_id.to_string()),
            accounts: vec![
                make_account(primary_store_account_id, "Primary"),
                make_account(fallback_store_account_id, "Fallback"),
            ],
            ..AuthStore::default()
        },
        AuthCredentialsStoreMode::File,
    )
    .expect("save auth store");

    let manager = AuthManager::new_with_sqlite_home(
        codex_home.path().to_path_buf(),
        sqlite_home.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
    );

    manager
        .account_manager
        .account_state_store
        .as_ref()
        .expect("account-state store should open")
        .upsert_usage_states(&HashMap::from([(
            fallback_store_account_id.to_string(),
            AccountUsageState {
                last_rate_limits: Some(RateLimitSnapshot {
                    limit_id: Some("codex".to_string()),
                    limit_name: None,
                    primary: Some(RateLimitWindow {
                        remaining_percent: 80.0,
                        window_minutes: Some(15),
                        resets_at: Some((Utc::now() + chrono::Duration::minutes(15)).timestamp()),
                    }),
                    secondary: None,
                    credits: None,
                    plan_type: None,
                    rate_limit_reached_type: None,
                }),
                exhausted_until: None,
                last_seen_at: Some(Utc::now()),
            },
        )]))
        .expect("persist sqlite usage truth");

    assert_eq!(
        manager.select_account_for_auto_switch(None, None),
        Some(fallback_store_account_id.to_string())
    );
}

// Merge-safety anchor: rate-limit refresh rosters must use the same
// AccountManager-owned cached runtime snapshot as autoswitch readers, including
// lease-aware exclusion of accounts held by another session.
#[test]
fn rate_limit_refresh_roster_excludes_foreign_leased_accounts_from_cached_snapshot() {
    let codex_home = tempdir().expect("create auth tempdir");
    let sqlite_home = tempdir().expect("create sqlite tempdir");
    let workspace_id = "workspace-a";
    let raw_jwt = fake_jwt_for_auth_file_params(&AuthFileParams {
        openai_api_key: None,
        chatgpt_plan_type: Some("plus".to_string()),
        chatgpt_account_id: Some(workspace_id.to_string()),
    })
    .expect("create test jwt");
    let make_account = |store_account_id: &str, label: &str| StoredAccount {
        id: store_account_id.to_string(),
        label: Some(label.to_string()),
        tokens: TokenData {
            id_token: IdTokenInfo {
                email: Some(format!("{store_account_id}@example.com")),
                chatgpt_plan_type: Some(InternalPlanType::Known(InternalKnownPlan::Plus)),
                chatgpt_user_id: Some(format!("user-{store_account_id}")),
                chatgpt_account_id: Some(workspace_id.to_string()),
                chatgpt_account_is_fedramp: false,
                raw_jwt: raw_jwt.clone(),
            },
            access_token: format!("{store_account_id}-access-token"),
            refresh_token: format!("{store_account_id}-refresh-token"),
            account_id: Some(workspace_id.to_string()),
        },
        last_refresh: None,
        usage: None,
    };
    save_auth(
        codex_home.path(),
        &AuthStore {
            active_account_id: Some("store-account-a".to_string()),
            accounts: vec![
                make_account("store-account-a", "Primary"),
                make_account("store-account-b", "Fallback"),
            ],
            ..AuthStore::default()
        },
        AuthCredentialsStoreMode::File,
    )
    .expect("save auth store");

    let manager = AuthManager::new_with_sqlite_home(
        codex_home.path().to_path_buf(),
        sqlite_home.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
    );
    manager
        .account_manager
        .account_state_store
        .as_ref()
        .expect("account-state store should open")
        .set_session_active_account(
            "other-session",
            None,
            "store-account-b",
            Utc::now(),
            ACTIVE_ACCOUNT_LEASE_TTL_SECONDS,
        )
        .expect("foreign lease should persist");

    assert_eq!(
        manager.account_rate_limit_refresh_roster(),
        AccountRateLimitRefreshRoster {
            store_account_ids: vec!["store-account-a".to_string()],
            status: AccountRateLimitRefreshRosterStatus::LeaseManaged,
        }
    );
}

#[test]
fn set_active_account_respects_forced_workspace_and_updates_last_seen() {
    let codex_home = tempdir().expect("create auth tempdir");
    let sqlite_home = tempdir().expect("create sqlite tempdir");
    let workspace_a = "workspace-a";
    let workspace_b = "workspace-b";
    let make_account = |store_account_id: &str, workspace_id: &str, label: &str| {
        let raw_jwt = fake_jwt_for_auth_file_params(&AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("plus".to_string()),
            chatgpt_account_id: Some(workspace_id.to_string()),
        })
        .expect("create test jwt");
        StoredAccount {
            id: store_account_id.to_string(),
            label: Some(label.to_string()),
            tokens: TokenData {
                id_token: IdTokenInfo {
                    email: Some(format!("{store_account_id}@example.com")),
                    chatgpt_plan_type: Some(InternalPlanType::Known(InternalKnownPlan::Plus)),
                    chatgpt_user_id: Some(format!("user-{store_account_id}")),
                    chatgpt_account_id: Some(workspace_id.to_string()),
                    chatgpt_account_is_fedramp: false,
                    raw_jwt: raw_jwt.clone(),
                },
                access_token: format!("{store_account_id}-access-token"),
                refresh_token: format!("{store_account_id}-refresh-token"),
                account_id: Some(workspace_id.to_string()),
            },
            last_refresh: None,
            usage: None,
        }
    };
    save_auth(
        codex_home.path(),
        &AuthStore {
            active_account_id: Some("store-account-a".to_string()),
            accounts: vec![
                make_account("store-account-a", workspace_a, "Primary"),
                make_account("store-account-b", workspace_a, "Secondary"),
                make_account("store-account-c", workspace_b, "Blocked"),
            ],
            ..AuthStore::default()
        },
        AuthCredentialsStoreMode::File,
    )
    .expect("save auth store");

    let manager = AuthManager::new_with_sqlite_home(
        codex_home.path().to_path_buf(),
        sqlite_home.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
    );
    manager.set_forced_chatgpt_workspace_id(Some(workspace_a.to_string()));

    let err = manager
        .set_active_account("store-account-c")
        .expect_err("workspace-mismatched account should fail");
    assert!(
        err.to_string()
            .contains("does not match required workspace"),
        "unexpected error: {err}"
    );

    manager
        .set_active_account("store-account-b")
        .expect("workspace-matching account should become active");

    assert_eq!(
        manager
            .active_chatgpt_account_summary()
            .map(|summary| summary.store_account_id),
        Some("store-account-b".to_string())
    );
    assert_eq!(
        manager
            .persisted_active_store_account_id()
            .expect("load persisted active account"),
        None
    );

    let cached_store = manager
        .inner
        .read()
        .expect("cached auth should stay readable")
        .store
        .clone();
    let account = cached_store
        .accounts
        .iter()
        .find(|account| account.id == "store-account-b")
        .expect("secondary account should still exist");
    assert!(
        account
            .usage
            .as_ref()
            .and_then(|usage| usage.last_seen_at)
            .is_some(),
        "selected account should record last_seen_at"
    );
}

// Merge-safety anchor: account upsert mutation belongs to AccountManager while
// AuthManager remains the persistence/cache wrapper for saved ChatGPT accounts.
#[test]
fn upsert_account_inserts_updates_and_preserves_existing_label_without_new_label() {
    let codex_home = tempdir().expect("create auth tempdir");
    let sqlite_home = tempdir().expect("create sqlite tempdir");
    let workspace_id = "workspace-a";
    let raw_jwt = fake_jwt_for_auth_file_params(&AuthFileParams {
        openai_api_key: None,
        chatgpt_plan_type: Some("plus".to_string()),
        chatgpt_account_id: Some(workspace_id.to_string()),
    })
    .expect("create test jwt");
    let make_tokens = |access_token: &str, refresh_token: &str| TokenData {
        id_token: IdTokenInfo {
            email: Some("primary@example.com".to_string()),
            chatgpt_plan_type: Some(InternalPlanType::Known(InternalKnownPlan::Plus)),
            chatgpt_user_id: Some("user-12345".to_string()),
            chatgpt_account_id: Some(workspace_id.to_string()),
            chatgpt_account_is_fedramp: false,
            raw_jwt: raw_jwt.clone(),
        },
        access_token: access_token.to_string(),
        refresh_token: refresh_token.to_string(),
        account_id: Some(workspace_id.to_string()),
    };

    let manager = AuthManager::new_with_sqlite_home(
        codex_home.path().to_path_buf(),
        sqlite_home.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
    );
    let expected_account_id = "chatgpt-user:user-12345:workspace:workspace-a";

    assert_eq!(
        manager
            .upsert_account(
                make_tokens("first-access-token", "first-refresh-token"),
                Some("Primary".to_string()),
                true,
            )
            .expect("initial account upsert should persist"),
        expected_account_id
    );

    assert_eq!(
        manager
            .upsert_account(
                make_tokens("updated-access-token", "updated-refresh-token"),
                None,
                false,
            )
            .expect("second account upsert should persist"),
        expected_account_id
    );

    let cached_store = manager
        .inner
        .read()
        .expect("cached auth should stay readable")
        .store
        .clone();
    assert_eq!(
        cached_store.active_account_id.as_deref(),
        Some(expected_account_id)
    );
    assert_eq!(cached_store.accounts.len(), 1);
    let account = cached_store
        .accounts
        .first()
        .expect("upserted account should stay cached");
    assert_eq!(account.id, expected_account_id);
    assert_eq!(account.label.as_deref(), Some("Primary"));
    assert_eq!(account.tokens.access_token, "updated-access-token");
    assert_eq!(account.tokens.refresh_token, "updated-refresh-token");
    assert!(account.last_refresh.is_some());
}

#[test]
fn remove_account_reselects_unleased_fallback_account() {
    let codex_home = tempdir().expect("create auth tempdir");
    let sqlite_home = tempdir().expect("create sqlite tempdir");
    let workspace_id = "workspace-a";
    let raw_jwt = fake_jwt_for_auth_file_params(&AuthFileParams {
        openai_api_key: None,
        chatgpt_plan_type: Some("plus".to_string()),
        chatgpt_account_id: Some(workspace_id.to_string()),
    })
    .expect("create test jwt");
    let make_account = |store_account_id: &str, label: &str| StoredAccount {
        id: store_account_id.to_string(),
        label: Some(label.to_string()),
        tokens: TokenData {
            id_token: IdTokenInfo {
                email: Some(format!("{store_account_id}@example.com")),
                chatgpt_plan_type: Some(InternalPlanType::Known(InternalKnownPlan::Plus)),
                chatgpt_user_id: Some(format!("user-{store_account_id}")),
                chatgpt_account_id: Some(workspace_id.to_string()),
                chatgpt_account_is_fedramp: false,
                raw_jwt: raw_jwt.clone(),
            },
            access_token: format!("{store_account_id}-access-token"),
            refresh_token: format!("{store_account_id}-refresh-token"),
            account_id: Some(workspace_id.to_string()),
        },
        last_refresh: None,
        usage: None,
    };
    save_auth(
        codex_home.path(),
        &AuthStore {
            active_account_id: Some("store-account-a".to_string()),
            accounts: vec![
                make_account("store-account-a", "Primary"),
                make_account("store-account-b", "Fallback"),
                make_account("store-account-c", "Leased elsewhere"),
            ],
            ..AuthStore::default()
        },
        AuthCredentialsStoreMode::File,
    )
    .expect("save auth store");

    let manager = AuthManager::new_with_sqlite_home(
        codex_home.path().to_path_buf(),
        sqlite_home.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
    );
    manager
        .account_manager
        .account_state_store
        .as_ref()
        .expect("account-state store should open")
        .set_session_active_account(
            "other-session",
            None,
            "store-account-c",
            Utc::now(),
            ACTIVE_ACCOUNT_LEASE_TTL_SECONDS,
        )
        .expect("foreign lease should persist");

    assert!(
        manager
            .remove_account("store-account-a")
            .expect("active account removal should succeed")
    );

    let cached_store = manager
        .inner
        .read()
        .expect("cached auth should stay readable")
        .store
        .clone();
    assert_eq!(
        cached_store.active_account_id,
        Some("store-account-b".to_string())
    );

    assert_eq!(
        manager
            .active_chatgpt_account_summary()
            .map(|summary| summary.store_account_id),
        Some("store-account-b".to_string())
    );
    assert_eq!(
        manager
            .persisted_active_store_account_id()
            .expect("load persisted active account"),
        None
    );

    let accounts = manager.list_accounts();
    assert_eq!(accounts.len(), 2);
    assert_eq!(
        accounts
            .iter()
            .find(|account| account.id == "store-account-b")
            .map(|account| account.is_active),
        Some(true)
    );
    assert_eq!(
        accounts
            .iter()
            .find(|account| account.id == "store-account-c")
            .map(|account| account.lease_state),
        Some(AccountLeaseState::LeasedByOtherSession)
    );
}

// Merge-safety anchor: terminal refresh-token eviction must keep the
// active-account removal path skipping foreign-leased fallbacks while updating
// cached and persisted active-account truth together.
#[test]
fn terminal_refresh_failure_removes_active_account_and_switches_to_unleased_fallback() {
    let codex_home = tempdir().expect("create auth tempdir");
    let sqlite_home = tempdir().expect("create sqlite tempdir");
    let workspace_id = "workspace-a";
    let raw_jwt = fake_jwt_for_auth_file_params(&AuthFileParams {
        openai_api_key: None,
        chatgpt_plan_type: Some("plus".to_string()),
        chatgpt_account_id: Some(workspace_id.to_string()),
    })
    .expect("create test jwt");
    let make_account = |store_account_id: &str, label: &str| StoredAccount {
        id: store_account_id.to_string(),
        label: Some(label.to_string()),
        tokens: TokenData {
            id_token: IdTokenInfo {
                email: Some(format!("{store_account_id}@example.com")),
                chatgpt_plan_type: Some(InternalPlanType::Known(InternalKnownPlan::Plus)),
                chatgpt_user_id: Some(format!("user-{store_account_id}")),
                chatgpt_account_id: Some(workspace_id.to_string()),
                chatgpt_account_is_fedramp: false,
                raw_jwt: raw_jwt.clone(),
            },
            access_token: format!("{store_account_id}-access-token"),
            refresh_token: format!("{store_account_id}-refresh-token"),
            account_id: Some(workspace_id.to_string()),
        },
        last_refresh: None,
        usage: None,
    };
    save_auth(
        codex_home.path(),
        &AuthStore {
            active_account_id: Some("store-account-a".to_string()),
            accounts: vec![
                make_account("store-account-a", "Primary"),
                make_account("store-account-b", "Leased elsewhere"),
                make_account("store-account-c", "Fallback"),
            ],
            ..AuthStore::default()
        },
        AuthCredentialsStoreMode::File,
    )
    .expect("save auth store");

    let manager = AuthManager::new_with_sqlite_home(
        codex_home.path().to_path_buf(),
        sqlite_home.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
    );
    manager
        .account_manager
        .account_state_store
        .as_ref()
        .expect("account-state store should open")
        .set_session_active_account(
            "other-session",
            None,
            "store-account-b",
            Utc::now(),
            ACTIVE_ACCOUNT_LEASE_TTL_SECONDS,
        )
        .expect("foreign lease should persist");

    let removal = manager
        .remove_chatgpt_store_account_for_terminal_refresh_failure(
            "store-account-a",
            &RefreshTokenFailedError::new(
                RefreshTokenFailedReason::Exhausted,
                "refresh token exhausted",
            ),
        )
        .expect("terminal refresh failure should remove the active account");
    assert_eq!(
        removal,
        TerminalRefreshFailureAccountRemoval::Removed {
            switched_to_store_account_id: Some("store-account-c".to_string())
        }
    );

    let cached_store = manager
        .inner
        .read()
        .expect("cached auth should stay readable")
        .store
        .clone();
    assert_eq!(
        cached_store.active_account_id,
        Some("store-account-c".to_string())
    );
    assert!(
        cached_store
            .accounts
            .iter()
            .all(|account| account.id != "store-account-a")
    );
    assert_eq!(
        manager
            .active_chatgpt_account_summary()
            .map(|summary| summary.store_account_id),
        Some("store-account-c".to_string())
    );
    assert_eq!(
        manager
            .persisted_active_store_account_id()
            .expect("load persisted active account"),
        None
    );
}

#[test]
fn reconcile_account_rate_limit_refresh_outcomes_updates_cached_usage_truth() {
    let codex_home = tempdir().expect("create auth tempdir");
    let sqlite_home = tempdir().expect("create sqlite tempdir");
    let workspace_id = "workspace-a";
    let raw_jwt = fake_jwt_for_auth_file_params(&AuthFileParams {
        openai_api_key: None,
        chatgpt_plan_type: Some("plus".to_string()),
        chatgpt_account_id: Some(workspace_id.to_string()),
    })
    .expect("create test jwt");
    let make_account = |store_account_id: &str, label: &str| StoredAccount {
        id: store_account_id.to_string(),
        label: Some(label.to_string()),
        tokens: TokenData {
            id_token: IdTokenInfo {
                email: Some(format!("{store_account_id}@example.com")),
                chatgpt_plan_type: Some(InternalPlanType::Known(InternalKnownPlan::Plus)),
                chatgpt_user_id: Some(format!("user-{store_account_id}")),
                chatgpt_account_id: Some(workspace_id.to_string()),
                chatgpt_account_is_fedramp: false,
                raw_jwt: raw_jwt.clone(),
            },
            access_token: format!("{store_account_id}-access-token"),
            refresh_token: format!("{store_account_id}-refresh-token"),
            account_id: Some(workspace_id.to_string()),
        },
        last_refresh: None,
        usage: None,
    };
    save_auth(
        codex_home.path(),
        &AuthStore {
            active_account_id: Some("store-account-a".to_string()),
            accounts: vec![
                make_account("store-account-a", "Primary"),
                make_account("store-account-b", "Secondary"),
            ],
            ..AuthStore::default()
        },
        AuthCredentialsStoreMode::File,
    )
    .expect("save auth store");

    let manager = AuthManager::new_with_sqlite_home(
        codex_home.path().to_path_buf(),
        sqlite_home.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
    );

    let reset_at = Utc::now() + chrono::Duration::minutes(12);
    let snapshot = RateLimitSnapshot {
        limit_id: Some("codex".to_string()),
        limit_name: None,
        primary: Some(RateLimitWindow {
            remaining_percent: 0.0,
            window_minutes: Some(15),
            resets_at: Some(reset_at.timestamp()),
        }),
        secondary: None,
        credits: None,
        plan_type: None,
        rate_limit_reached_type: None,
    };
    let expected_reset_at =
        DateTime::<Utc>::from_timestamp(reset_at.timestamp(), 0).expect("valid reset timestamp");

    assert_eq!(
        manager
            .reconcile_account_rate_limit_refresh_outcomes([
                (
                    "store-account-a".to_string(),
                    AccountRateLimitRefreshOutcome::Snapshot(snapshot.clone()),
                ),
                (
                    "store-account-b".to_string(),
                    AccountRateLimitRefreshOutcome::NoUsableSnapshot,
                ),
            ])
            .expect("rate-limit refresh outcomes should apply"),
        2
    );

    let cached_store = manager
        .inner
        .read()
        .expect("cached auth should stay readable")
        .store
        .clone();
    let primary_account = cached_store
        .accounts
        .iter()
        .find(|account| account.id == "store-account-a")
        .expect("primary account should still exist");
    let primary_usage = primary_account
        .usage
        .as_ref()
        .expect("primary account usage should be set");
    assert_eq!(primary_usage.last_rate_limits, Some(snapshot));
    assert_eq!(primary_usage.exhausted_until, Some(expected_reset_at));
    assert!(primary_usage.last_seen_at.is_some());

    let secondary_account = cached_store
        .accounts
        .iter()
        .find(|account| account.id == "store-account-b")
        .expect("secondary account should still exist");
    let secondary_usage = secondary_account
        .usage
        .as_ref()
        .expect("secondary account usage should be set");
    assert_eq!(secondary_usage.last_rate_limits, None);
    assert_eq!(secondary_usage.exhausted_until, None);
    assert!(secondary_usage.last_seen_at.is_some());
}

#[test]
fn mark_usage_limit_reached_updates_active_usage_and_cache_expiry_uses_sqlite_truth() {
    let codex_home = tempdir().expect("create auth tempdir");
    let sqlite_home = tempdir().expect("create sqlite tempdir");
    let workspace_id = "workspace-a";
    let raw_jwt = fake_jwt_for_auth_file_params(&AuthFileParams {
        openai_api_key: None,
        chatgpt_plan_type: Some("plus".to_string()),
        chatgpt_account_id: Some(workspace_id.to_string()),
    })
    .expect("create test jwt");
    let make_account = |store_account_id: &str, label: &str| StoredAccount {
        id: store_account_id.to_string(),
        label: Some(label.to_string()),
        tokens: TokenData {
            id_token: IdTokenInfo {
                email: Some(format!("{store_account_id}@example.com")),
                chatgpt_plan_type: Some(InternalPlanType::Known(InternalKnownPlan::Plus)),
                chatgpt_user_id: Some(format!("user-{store_account_id}")),
                chatgpt_account_id: Some(workspace_id.to_string()),
                chatgpt_account_is_fedramp: false,
                raw_jwt: raw_jwt.clone(),
            },
            access_token: format!("{store_account_id}-access-token"),
            refresh_token: format!("{store_account_id}-refresh-token"),
            account_id: Some(workspace_id.to_string()),
        },
        last_refresh: None,
        usage: None,
    };
    save_auth(
        codex_home.path(),
        &AuthStore {
            active_account_id: Some("store-account-a".to_string()),
            accounts: vec![
                make_account("store-account-a", "Primary"),
                make_account("store-account-b", "Secondary"),
            ],
            ..AuthStore::default()
        },
        AuthCredentialsStoreMode::File,
    )
    .expect("save auth store");

    let manager = AuthManager::new_with_sqlite_home(
        codex_home.path().to_path_buf(),
        sqlite_home.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
    );

    let now = Utc::now();
    let reset_at = now + chrono::Duration::minutes(7);
    let snapshot = RateLimitSnapshot {
        limit_id: Some("codex".to_string()),
        limit_name: None,
        primary: Some(RateLimitWindow {
            remaining_percent: 0.0,
            window_minutes: Some(15),
            resets_at: Some(reset_at.timestamp()),
        }),
        secondary: None,
        credits: None,
        plan_type: None,
        rate_limit_reached_type: None,
    };
    let expected_reset_at =
        DateTime::<Utc>::from_timestamp(reset_at.timestamp(), 0).expect("valid reset timestamp");

    manager
        .mark_usage_limit_reached(Some(reset_at), Some(snapshot.clone()))
        .expect("usage-limit mark should succeed");

    let cached_store = manager
        .inner
        .read()
        .expect("cached auth should stay readable")
        .store
        .clone();
    let active_account = cached_store
        .accounts
        .iter()
        .find(|account| account.id == "store-account-a")
        .expect("active account should still exist");
    let active_usage = active_account
        .usage
        .as_ref()
        .expect("active account usage should be set");
    assert_eq!(active_usage.last_rate_limits, Some(snapshot.clone()));
    assert_eq!(active_usage.exhausted_until, Some(reset_at));
    assert!(active_usage.last_seen_at.is_some());

    {
        let mut guard = manager
            .inner
            .write()
            .expect("cached auth should stay writable");
        let account = guard
            .store
            .accounts
            .iter_mut()
            .find(|account| account.id == "store-account-a")
            .expect("active account should stay cached");
        account.usage = None;
    }

    assert_eq!(
        manager.accounts_rate_limits_cache_expires_at(now),
        Some(expected_reset_at)
    );
}

#[test]
fn update_usage_for_active_updates_cached_active_usage() {
    let codex_home = tempdir().expect("create auth tempdir");
    let sqlite_home = tempdir().expect("create sqlite tempdir");
    let workspace_id = "workspace-a";
    let raw_jwt = fake_jwt_for_auth_file_params(&AuthFileParams {
        openai_api_key: None,
        chatgpt_plan_type: Some("plus".to_string()),
        chatgpt_account_id: Some(workspace_id.to_string()),
    })
    .expect("create test jwt");
    let make_account = |store_account_id: &str, label: &str| StoredAccount {
        id: store_account_id.to_string(),
        label: Some(label.to_string()),
        tokens: TokenData {
            id_token: IdTokenInfo {
                email: Some(format!("{store_account_id}@example.com")),
                chatgpt_plan_type: Some(InternalPlanType::Known(InternalKnownPlan::Plus)),
                chatgpt_user_id: Some(format!("user-{store_account_id}")),
                chatgpt_account_id: Some(workspace_id.to_string()),
                chatgpt_account_is_fedramp: false,
                raw_jwt: raw_jwt.clone(),
            },
            access_token: format!("{store_account_id}-access-token"),
            refresh_token: format!("{store_account_id}-refresh-token"),
            account_id: Some(workspace_id.to_string()),
        },
        last_refresh: None,
        usage: None,
    };
    save_auth(
        codex_home.path(),
        &AuthStore {
            active_account_id: Some("store-account-a".to_string()),
            accounts: vec![
                make_account("store-account-a", "Primary"),
                make_account("store-account-b", "Secondary"),
            ],
            ..AuthStore::default()
        },
        AuthCredentialsStoreMode::File,
    )
    .expect("save auth store");

    let manager = AuthManager::new_with_sqlite_home(
        codex_home.path().to_path_buf(),
        sqlite_home.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
    );

    let reset_at = Utc::now() + chrono::Duration::minutes(11);
    let expected_reset_at =
        DateTime::<Utc>::from_timestamp(reset_at.timestamp(), 0).expect("valid reset timestamp");
    let snapshot = RateLimitSnapshot {
        limit_id: Some("codex".to_string()),
        limit_name: None,
        primary: Some(RateLimitWindow {
            remaining_percent: 0.0,
            window_minutes: Some(15),
            resets_at: Some(reset_at.timestamp()),
        }),
        secondary: None,
        credits: None,
        plan_type: None,
        rate_limit_reached_type: None,
    };

    manager
        .update_usage_for_active(snapshot.clone())
        .expect("active usage update should succeed");

    let cached_store = manager
        .inner
        .read()
        .expect("cached auth should stay readable")
        .store
        .clone();
    let active_account = cached_store
        .accounts
        .iter()
        .find(|account| account.id == "store-account-a")
        .expect("active account should still exist");
    let active_usage = active_account
        .usage
        .as_ref()
        .expect("active account usage should be set");
    assert_eq!(active_usage.last_rate_limits, Some(snapshot));
    assert_eq!(active_usage.exhausted_until, Some(expected_reset_at));
    assert!(active_usage.last_seen_at.is_some());

    let inactive_account = cached_store
        .accounts
        .iter()
        .find(|account| account.id == "store-account-b")
        .expect("inactive account should still exist");
    assert_eq!(inactive_account.usage, None);
}

#[test]
fn update_rate_limits_for_account_and_accounts_update_targeted_usage() {
    let codex_home = tempdir().expect("create auth tempdir");
    let sqlite_home = tempdir().expect("create sqlite tempdir");
    let workspace_id = "workspace-a";
    let raw_jwt = fake_jwt_for_auth_file_params(&AuthFileParams {
        openai_api_key: None,
        chatgpt_plan_type: Some("plus".to_string()),
        chatgpt_account_id: Some(workspace_id.to_string()),
    })
    .expect("create test jwt");
    let make_account = |store_account_id: &str, label: &str| StoredAccount {
        id: store_account_id.to_string(),
        label: Some(label.to_string()),
        tokens: TokenData {
            id_token: IdTokenInfo {
                email: Some(format!("{store_account_id}@example.com")),
                chatgpt_plan_type: Some(InternalPlanType::Known(InternalKnownPlan::Plus)),
                chatgpt_user_id: Some(format!("user-{store_account_id}")),
                chatgpt_account_id: Some(workspace_id.to_string()),
                chatgpt_account_is_fedramp: false,
                raw_jwt: raw_jwt.clone(),
            },
            access_token: format!("{store_account_id}-access-token"),
            refresh_token: format!("{store_account_id}-refresh-token"),
            account_id: Some(workspace_id.to_string()),
        },
        last_refresh: None,
        usage: None,
    };
    save_auth(
        codex_home.path(),
        &AuthStore {
            active_account_id: Some("store-account-a".to_string()),
            accounts: vec![
                make_account("store-account-a", "Primary"),
                make_account("store-account-b", "Secondary"),
                make_account("store-account-c", "Tertiary"),
            ],
            ..AuthStore::default()
        },
        AuthCredentialsStoreMode::File,
    )
    .expect("save auth store");

    let manager = AuthManager::new_with_sqlite_home(
        codex_home.path().to_path_buf(),
        sqlite_home.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
    );

    let single_reset_at = Utc::now() + chrono::Duration::minutes(13);
    let single_snapshot = RateLimitSnapshot {
        limit_id: Some("single".to_string()),
        limit_name: None,
        primary: Some(RateLimitWindow {
            remaining_percent: 10.0,
            window_minutes: Some(15),
            resets_at: Some(single_reset_at.timestamp()),
        }),
        secondary: None,
        credits: None,
        plan_type: None,
        rate_limit_reached_type: None,
    };
    manager
        .update_rate_limits_for_account("store-account-b", single_snapshot.clone())
        .expect("single-account rate-limit update should succeed");

    let bulk_a_reset_at = Utc::now() + chrono::Duration::minutes(17);
    let bulk_a_snapshot = RateLimitSnapshot {
        limit_id: Some("bulk-a".to_string()),
        limit_name: None,
        primary: Some(RateLimitWindow {
            remaining_percent: 20.0,
            window_minutes: Some(15),
            resets_at: Some(bulk_a_reset_at.timestamp()),
        }),
        secondary: None,
        credits: None,
        plan_type: None,
        rate_limit_reached_type: None,
    };
    let bulk_c_reset_at = Utc::now() + chrono::Duration::minutes(19);
    let bulk_c_snapshot = RateLimitSnapshot {
        limit_id: Some("bulk-c".to_string()),
        limit_name: None,
        primary: Some(RateLimitWindow {
            remaining_percent: 30.0,
            window_minutes: Some(15),
            resets_at: Some(bulk_c_reset_at.timestamp()),
        }),
        secondary: None,
        credits: None,
        plan_type: None,
        rate_limit_reached_type: None,
    };
    assert_eq!(
        manager
            .update_rate_limits_for_accounts(vec![
                ("store-account-a".to_string(), bulk_a_snapshot.clone()),
                ("store-account-c".to_string(), bulk_c_snapshot.clone()),
            ])
            .expect("bulk rate-limit update should succeed"),
        2
    );

    let cached_store = manager
        .inner
        .read()
        .expect("cached auth should stay readable")
        .store
        .clone();

    let account_a = cached_store
        .accounts
        .iter()
        .find(|account| account.id == "store-account-a")
        .expect("account a should still exist");
    let usage_a = account_a
        .usage
        .as_ref()
        .expect("account a usage should be set");
    assert_eq!(usage_a.last_rate_limits, Some(bulk_a_snapshot));
    assert!(usage_a.last_seen_at.is_some());

    let account_b = cached_store
        .accounts
        .iter()
        .find(|account| account.id == "store-account-b")
        .expect("account b should still exist");
    let usage_b = account_b
        .usage
        .as_ref()
        .expect("account b usage should be set");
    assert_eq!(usage_b.last_rate_limits, Some(single_snapshot));
    assert!(usage_b.last_seen_at.is_some());

    let account_c = cached_store
        .accounts
        .iter()
        .find(|account| account.id == "store-account-c")
        .expect("account c should still exist");
    let usage_c = account_c
        .usage
        .as_ref()
        .expect("account c usage should be set");
    assert_eq!(usage_c.last_rate_limits, Some(bulk_c_snapshot));
    assert!(usage_c.last_seen_at.is_some());
}

#[test]
fn switch_account_on_usage_limit_respects_cooldown_but_still_marks_failing_account_exhausted() {
    let codex_home = tempdir().expect("create auth tempdir");
    let sqlite_home = tempdir().expect("create sqlite tempdir");
    let workspace_id = "workspace-a";
    let raw_jwt = fake_jwt_for_auth_file_params(&AuthFileParams {
        openai_api_key: None,
        chatgpt_plan_type: Some("plus".to_string()),
        chatgpt_account_id: Some(workspace_id.to_string()),
    })
    .expect("create test jwt");
    let make_account = |store_account_id: &str, label: &str| StoredAccount {
        id: store_account_id.to_string(),
        label: Some(label.to_string()),
        tokens: TokenData {
            id_token: IdTokenInfo {
                email: Some(format!("{store_account_id}@example.com")),
                chatgpt_plan_type: Some(InternalPlanType::Known(InternalKnownPlan::Plus)),
                chatgpt_user_id: Some(format!("user-{store_account_id}")),
                chatgpt_account_id: Some(workspace_id.to_string()),
                chatgpt_account_is_fedramp: false,
                raw_jwt: raw_jwt.clone(),
            },
            access_token: format!("{store_account_id}-access-token"),
            refresh_token: format!("{store_account_id}-refresh-token"),
            account_id: Some(workspace_id.to_string()),
        },
        last_refresh: None,
        usage: None,
    };
    save_auth(
        codex_home.path(),
        &AuthStore {
            active_account_id: Some("store-account-a".to_string()),
            accounts: vec![
                make_account("store-account-a", "Primary"),
                make_account("store-account-b", "Fallback"),
            ],
            ..AuthStore::default()
        },
        AuthCredentialsStoreMode::File,
    )
    .expect("save auth store");

    let manager = AuthManager::new_with_sqlite_home(
        codex_home.path().to_path_buf(),
        sqlite_home.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
    );
    *manager
        .account_manager
        .usage_limit_auto_switch_cooldown_until
        .lock()
        .expect("cooldown mutex should stay writable") =
        Some(Utc::now() + chrono::Duration::seconds(60));

    let reset_at = Utc::now() + chrono::Duration::minutes(9);
    let snapshot = RateLimitSnapshot {
        limit_id: Some("codex".to_string()),
        limit_name: None,
        primary: Some(RateLimitWindow {
            remaining_percent: 0.0,
            window_minutes: Some(15),
            resets_at: Some(reset_at.timestamp()),
        }),
        secondary: None,
        credits: None,
        plan_type: None,
        rate_limit_reached_type: None,
    };

    assert_eq!(
        manager
            .switch_account_on_usage_limit(UsageLimitAutoSwitchRequest {
                required_workspace_id: None,
                failing_store_account_id: Some("store-account-a"),
                resets_at: Some(reset_at),
                snapshot: Some(snapshot.clone()),
                freshly_unsupported_store_account_ids: &HashSet::new(),
                protected_store_account_id: None,
                selection_scope: UsageLimitAutoSwitchSelectionScope::PersistedTruth,
                fallback_selection_mode:
                    UsageLimitAutoSwitchFallbackSelectionMode::AllowFallbackSelection,
            })
            .expect("autoswitch under cooldown should not error"),
        None
    );

    let cached_store = manager
        .inner
        .read()
        .expect("cached auth should stay readable")
        .store
        .clone();
    assert_eq!(
        cached_store.active_account_id,
        Some("store-account-a".to_string())
    );
    let failing_account = cached_store
        .accounts
        .iter()
        .find(|account| account.id == "store-account-a")
        .expect("failing account should still exist");
    let usage = failing_account
        .usage
        .as_ref()
        .expect("failing account usage should be written");
    assert_eq!(usage.last_rate_limits, Some(snapshot));
    assert_eq!(usage.exhausted_until, Some(reset_at));
    assert!(usage.last_seen_at.is_some());
}

// Merge-safety anchor: usage-limit auto-switch cooldown ownership must keep
// starting the cooldown only after a real persisted fallback switch succeeds.
#[test]
fn switch_account_on_usage_limit_starts_cooldown_after_switching_to_fallback() {
    let codex_home = tempdir().expect("create auth tempdir");
    let sqlite_home = tempdir().expect("create sqlite tempdir");
    let workspace_id = "workspace-a";
    let raw_jwt = fake_jwt_for_auth_file_params(&AuthFileParams {
        openai_api_key: None,
        chatgpt_plan_type: Some("plus".to_string()),
        chatgpt_account_id: Some(workspace_id.to_string()),
    })
    .expect("create test jwt");
    let make_account = |store_account_id: &str, label: &str| StoredAccount {
        id: store_account_id.to_string(),
        label: Some(label.to_string()),
        tokens: TokenData {
            id_token: IdTokenInfo {
                email: Some(format!("{store_account_id}@example.com")),
                chatgpt_plan_type: Some(InternalPlanType::Known(InternalKnownPlan::Plus)),
                chatgpt_user_id: Some(format!("user-{store_account_id}")),
                chatgpt_account_id: Some(workspace_id.to_string()),
                chatgpt_account_is_fedramp: false,
                raw_jwt: raw_jwt.clone(),
            },
            access_token: format!("{store_account_id}-access-token"),
            refresh_token: format!("{store_account_id}-refresh-token"),
            account_id: Some(workspace_id.to_string()),
        },
        last_refresh: None,
        usage: None,
    };

    save_auth(
        codex_home.path(),
        &AuthStore {
            active_account_id: Some("store-account-a".to_string()),
            accounts: vec![
                make_account("store-account-a", "Primary"),
                make_account("store-account-b", "Fallback"),
            ],
            ..AuthStore::default()
        },
        AuthCredentialsStoreMode::File,
    )
    .expect("save auth store");

    let manager = AuthManager::new_with_sqlite_home(
        codex_home.path().to_path_buf(),
        sqlite_home.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
    );

    let reset_at = Utc::now() + chrono::Duration::minutes(9);
    let snapshot = RateLimitSnapshot {
        limit_id: Some("codex".to_string()),
        limit_name: None,
        primary: Some(RateLimitWindow {
            remaining_percent: 0.0,
            window_minutes: Some(15),
            resets_at: Some(reset_at.timestamp()),
        }),
        secondary: None,
        credits: None,
        plan_type: None,
        rate_limit_reached_type: None,
    };

    assert_eq!(
        manager
            .switch_account_on_usage_limit(UsageLimitAutoSwitchRequest {
                required_workspace_id: None,
                failing_store_account_id: Some("store-account-a"),
                resets_at: Some(reset_at),
                snapshot: Some(snapshot.clone()),
                freshly_unsupported_store_account_ids: &HashSet::new(),
                protected_store_account_id: None,
                selection_scope: UsageLimitAutoSwitchSelectionScope::PersistedTruth,
                fallback_selection_mode:
                    UsageLimitAutoSwitchFallbackSelectionMode::AllowFallbackSelection,
            })
            .expect("autoswitch should succeed"),
        Some("store-account-b".to_string())
    );

    let cached_store = manager
        .inner
        .read()
        .expect("cached auth should stay readable")
        .store
        .clone();
    assert_eq!(
        cached_store.active_account_id,
        Some("store-account-b".to_string())
    );
    assert!(
        manager
            .account_manager
            .usage_limit_auto_switch_cooldown_until
            .lock()
            .expect("cooldown mutex should stay writable")
            .is_some(),
        "successful autoswitch should start cooldown"
    );

    let failing_account = cached_store
        .accounts
        .iter()
        .find(|account| account.id == "store-account-a")
        .expect("failing account should still exist");
    let usage = failing_account
        .usage
        .as_ref()
        .expect("failing account usage should be written");
    assert_eq!(usage.last_rate_limits, Some(snapshot));
    assert_eq!(usage.exhausted_until, Some(reset_at));
    assert!(usage.last_seen_at.is_some());
}

#[test]
fn has_saved_chatgpt_accounts_reads_latest_persisted_store_after_manager_construction() {
    let codex_home = tempdir().expect("create auth tempdir");
    let sqlite_home = tempdir().expect("create sqlite tempdir");
    let store_account_id = "store-account-a";
    let workspace_id = "workspace-a";
    let raw_jwt = fake_jwt_for_auth_file_params(&AuthFileParams {
        openai_api_key: None,
        chatgpt_plan_type: Some("plus".to_string()),
        chatgpt_account_id: Some(workspace_id.to_string()),
    })
    .expect("create test jwt");

    let manager = AuthManager::new_with_sqlite_home(
        codex_home.path().to_path_buf(),
        sqlite_home.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
    );
    assert!(
        !manager.has_saved_chatgpt_accounts(),
        "fresh manager should start with no saved accounts"
    );

    save_auth(
        codex_home.path(),
        &AuthStore {
            active_account_id: Some(store_account_id.to_string()),
            accounts: vec![StoredAccount {
                id: store_account_id.to_string(),
                label: Some("Primary".to_string()),
                tokens: TokenData {
                    id_token: IdTokenInfo {
                        email: Some("primary@example.com".to_string()),
                        chatgpt_plan_type: Some(InternalPlanType::Known(InternalKnownPlan::Plus)),
                        chatgpt_user_id: Some("user-12345".to_string()),
                        chatgpt_account_id: Some(workspace_id.to_string()),
                        chatgpt_account_is_fedramp: false,
                        raw_jwt,
                    },
                    access_token: "access-token".to_string(),
                    refresh_token: "refresh-token".to_string(),
                    account_id: Some(workspace_id.to_string()),
                },
                last_refresh: Some(Utc::now()),
                usage: None,
            }],
            ..AuthStore::default()
        },
        AuthCredentialsStoreMode::File,
    )
    .expect("save auth store after manager construction");

    assert!(manager.has_saved_chatgpt_accounts());
}

#[test]
fn auth_mode_and_active_summary_read_latest_persisted_store_after_manager_construction() {
    let codex_home = tempdir().expect("create auth tempdir");
    let sqlite_home = tempdir().expect("create sqlite tempdir");
    let store_account_id = "store-account-a";
    let workspace_id = "workspace-a";
    let raw_jwt = fake_jwt_for_auth_file_params(&AuthFileParams {
        openai_api_key: None,
        chatgpt_plan_type: Some("plus".to_string()),
        chatgpt_account_id: Some(workspace_id.to_string()),
    })
    .expect("create test jwt");

    let manager = AuthManager::new_with_sqlite_home(
        codex_home.path().to_path_buf(),
        sqlite_home.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
    );
    assert_eq!(manager.get_api_auth_mode(), None);
    assert_eq!(manager.get_auth_mode(), None);
    assert_eq!(manager.active_chatgpt_account_summary(), None);

    save_auth(
        codex_home.path(),
        &AuthStore {
            active_account_id: Some(store_account_id.to_string()),
            accounts: vec![StoredAccount {
                id: store_account_id.to_string(),
                label: Some("Primary".to_string()),
                tokens: TokenData {
                    id_token: IdTokenInfo {
                        email: Some("primary@example.com".to_string()),
                        chatgpt_plan_type: Some(InternalPlanType::Known(InternalKnownPlan::Plus)),
                        chatgpt_user_id: Some("user-12345".to_string()),
                        chatgpt_account_id: Some(workspace_id.to_string()),
                        chatgpt_account_is_fedramp: false,
                        raw_jwt,
                    },
                    access_token: "access-token".to_string(),
                    refresh_token: "refresh-token".to_string(),
                    account_id: Some(workspace_id.to_string()),
                },
                last_refresh: Some(Utc::now()),
                usage: None,
            }],
            ..AuthStore::default()
        },
        AuthCredentialsStoreMode::File,
    )
    .expect("save auth store after manager construction");

    assert_eq!(manager.get_api_auth_mode(), Some(ApiAuthMode::Chatgpt));
    assert_eq!(manager.get_auth_mode(), Some(ApiAuthMode::Chatgpt));
    assert_eq!(
        manager
            .active_chatgpt_account_summary()
            .map(|summary| summary.store_account_id),
        Some(store_account_id.to_string())
    );
}

#[test]
fn auth_mode_and_active_summary_read_latest_external_store_after_manager_construction() {
    let codex_home = tempdir().expect("create auth tempdir");
    let sqlite_home = tempdir().expect("create sqlite tempdir");

    let manager = AuthManager::new_with_sqlite_home(
        codex_home.path().to_path_buf(),
        sqlite_home.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
    );
    assert_eq!(manager.get_api_auth_mode(), None);
    assert_eq!(manager.get_auth_mode(), None);
    assert_eq!(manager.active_chatgpt_account_summary(), None);

    save_auth(
        codex_home.path(),
        &external_chatgpt_auth_store("external-store-account", "external-workspace"),
        AuthCredentialsStoreMode::Ephemeral,
    )
    .expect("save external auth store after manager construction");

    assert_eq!(
        manager.get_api_auth_mode(),
        Some(ApiAuthMode::ChatgptAuthTokens)
    );
    assert_eq!(
        manager.get_auth_mode(),
        Some(ApiAuthMode::ChatgptAuthTokens)
    );
    assert_eq!(
        manager
            .active_chatgpt_account_summary()
            .map(|summary| summary.store_account_id),
        Some("external-store-account".to_string())
    );
}

#[test]
fn live_auth_readers_do_not_emit_auth_state_notifications() {
    let codex_home = tempdir().expect("create auth tempdir");
    let sqlite_home = tempdir().expect("create sqlite tempdir");
    let store_account_id = "store-account-a";
    let workspace_id = "workspace-a";
    let raw_jwt = fake_jwt_for_auth_file_params(&AuthFileParams {
        openai_api_key: None,
        chatgpt_plan_type: Some("plus".to_string()),
        chatgpt_account_id: Some(workspace_id.to_string()),
    })
    .expect("create test jwt");

    let manager = AuthManager::new_with_sqlite_home(
        codex_home.path().to_path_buf(),
        sqlite_home.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
    );
    let mut auth_state_rx = manager.subscribe_auth_state();
    let _ = auth_state_rx.borrow_and_update();

    save_auth(
        codex_home.path(),
        &AuthStore {
            active_account_id: Some(store_account_id.to_string()),
            accounts: vec![StoredAccount {
                id: store_account_id.to_string(),
                label: Some("Primary".to_string()),
                tokens: TokenData {
                    id_token: IdTokenInfo {
                        email: Some("primary@example.com".to_string()),
                        chatgpt_plan_type: Some(InternalPlanType::Known(InternalKnownPlan::Plus)),
                        chatgpt_user_id: Some("user-12345".to_string()),
                        chatgpt_account_id: Some(workspace_id.to_string()),
                        chatgpt_account_is_fedramp: false,
                        raw_jwt,
                    },
                    access_token: "access-token".to_string(),
                    refresh_token: "refresh-token".to_string(),
                    account_id: Some(workspace_id.to_string()),
                },
                last_refresh: Some(Utc::now()),
                usage: None,
            }],
            ..AuthStore::default()
        },
        AuthCredentialsStoreMode::File,
    )
    .expect("save auth store after manager construction");

    assert_eq!(manager.get_api_auth_mode(), Some(ApiAuthMode::Chatgpt));
    assert_eq!(manager.get_auth_mode(), Some(ApiAuthMode::Chatgpt));
    assert_eq!(
        manager
            .active_chatgpt_account_summary()
            .map(|summary| summary.store_account_id),
        Some(store_account_id.to_string())
    );
    assert!(
        !auth_state_rx
            .has_changed()
            .expect("auth-state watch should remain open"),
        "pure live-read cache refresh should not emit auth-state notifications"
    );
}

// Merge-safety anchor: env API-key override must suppress the active ChatGPT
// summary even when the runtime-prepared saved-account snapshot still has an
// active ChatGPT account.
#[test]
#[serial(codex_api_key)]
fn active_summary_returns_none_when_env_api_key_override_is_enabled() {
    let _guard = EnvVarGuard::set(CODEX_API_KEY_ENV_VAR, "sk-env");
    let codex_home = tempdir().expect("create auth tempdir");
    let sqlite_home = tempdir().expect("create sqlite tempdir");
    let store_account_id = "store-account-a";
    let workspace_id = "workspace-a";
    let raw_jwt = fake_jwt_for_auth_file_params(&AuthFileParams {
        openai_api_key: None,
        chatgpt_plan_type: Some("plus".to_string()),
        chatgpt_account_id: Some(workspace_id.to_string()),
    })
    .expect("create test jwt");

    save_auth(
        codex_home.path(),
        &AuthStore {
            active_account_id: Some(store_account_id.to_string()),
            accounts: vec![StoredAccount {
                id: store_account_id.to_string(),
                label: Some("Primary".to_string()),
                tokens: TokenData {
                    id_token: IdTokenInfo {
                        email: Some("primary@example.com".to_string()),
                        chatgpt_plan_type: Some(InternalPlanType::Known(InternalKnownPlan::Plus)),
                        chatgpt_user_id: Some("user-12345".to_string()),
                        chatgpt_account_id: Some(workspace_id.to_string()),
                        chatgpt_account_is_fedramp: false,
                        raw_jwt,
                    },
                    access_token: "access-token".to_string(),
                    refresh_token: "refresh-token".to_string(),
                    account_id: Some(workspace_id.to_string()),
                },
                last_refresh: Some(Utc::now()),
                usage: None,
            }],
            ..AuthStore::default()
        },
        AuthCredentialsStoreMode::File,
    )
    .expect("save auth store before manager construction");

    let manager = AuthManager::new_with_sqlite_home(
        codex_home.path().to_path_buf(),
        sqlite_home.path().to_path_buf(),
        /*enable_codex_api_key_env*/ true,
        AuthCredentialsStoreMode::File,
    );

    assert_eq!(manager.get_api_auth_mode(), Some(ApiAuthMode::ApiKey));
    assert_eq!(manager.active_chatgpt_account_summary(), None);
}

#[test]
fn idempotent_reload_does_not_emit_auth_state_notifications() {
    let codex_home = tempdir().expect("create auth tempdir");
    let manager = AuthManager::new(
        codex_home.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
    );

    write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("plus".to_string()),
            chatgpt_account_id: Some("workspace-a".to_string()),
        },
        codex_home.path(),
    )
    .expect("save auth store before idempotent reload");

    assert!(
        manager.reload(),
        "initial reload should populate auth cache"
    );

    let mut auth_state_rx = manager.subscribe_auth_state();
    let _ = auth_state_rx.borrow_and_update();

    assert!(
        !manager.reload(),
        "idempotent reload should report unchanged auth"
    );
    assert!(
        !auth_state_rx
            .has_changed()
            .expect("auth-state watch should remain open"),
        "idempotent reload should not emit auth-state notifications"
    );
}

#[test]
fn chatgpt_auth_persists_agent_identity_for_workspace() {
    let codex_home = tempdir().unwrap();
    write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("pro".to_string()),
            chatgpt_account_id: Some("account-123".to_string()),
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
    let record = AgentIdentityAuthRecord {
        workspace_id: "account-123".to_string(),
        chatgpt_user_id: Some("user-123".to_string()),
        agent_runtime_id: "agent_123".to_string(),
        agent_private_key: "pkcs8-base64".to_string(),
        registered_at: "2026-04-13T12:00:00Z".to_string(),
    };

    auth.set_agent_identity(record.clone())
        .expect("set agent identity");

    assert_eq!(auth.get_agent_identity("account-123"), Some(record.clone()));
    assert_eq!(auth.get_agent_identity("other-account"), None);
    let storage = FileAuthStorage::new(codex_home.path().to_path_buf());
    let persisted = storage
        .load()
        .expect("load auth")
        .expect("auth should exist");
    assert_eq!(persisted.agent_identity, Some(record));

    assert!(auth.remove_agent_identity().expect("remove agent identity"));
    assert_eq!(auth.get_agent_identity("account-123"), None);
}

#[tokio::test]
async fn chatgpt_authorization_header_helpers_return_bearer_for_chatgpt_auth() {
    let auth = CodexAuth::create_dummy_chatgpt_auth_for_testing();
    let manager = AuthManager::from_auth_for_testing(auth.clone());

    assert_eq!(
        manager.chatgpt_authorization_header().await.as_deref(),
        Some("Bearer Access Token")
    );
    assert_eq!(
        manager
            .chatgpt_authorization_header_for_auth(&auth)
            .await
            .as_deref(),
        Some("Bearer Access Token")
    );
    assert_eq!(
        AuthManager::chatgpt_bearer_token_for_auth(&auth).as_deref(),
        Some("Access Token")
    );
    assert_eq!(
        AuthManager::chatgpt_bearer_authorization_header_for_auth(&auth).as_deref(),
        Some("Bearer Access Token")
    );
}

#[tokio::test]
async fn chatgpt_authorization_header_helpers_ignore_api_key_auth() {
    let auth = CodexAuth::from_api_key("sk-test");
    let manager = AuthManager::from_auth_for_testing(auth.clone());

    assert_eq!(manager.chatgpt_authorization_header().await, None);
    assert_eq!(
        manager.chatgpt_authorization_header_for_auth(&auth).await,
        None
    );
    assert_eq!(AuthManager::chatgpt_bearer_token_for_auth(&auth), None);
    assert_eq!(
        AuthManager::chatgpt_bearer_authorization_header_for_auth(&auth),
        None
    );
}

#[test]
fn external_chatgpt_token_auth_loads_from_ephemeral_store() {
    let codex_home = tempdir().unwrap();
    save_auth(
        codex_home.path(),
        &external_chatgpt_auth_store("store-account-1", "account-123"),
        AuthCredentialsStoreMode::Ephemeral,
    )
    .expect("save external ChatGPT token auth store");

    let auth = CodexAuth::from_auth_storage(codex_home.path(), AuthCredentialsStoreMode::File)
        .expect("load auth")
        .expect("external auth should be available");

    assert_eq!(auth.auth_mode(), AuthMode::ChatgptAuthTokens);
    assert_eq!(auth.api_auth_mode(), ApiAuthMode::ChatgptAuthTokens);
    assert_eq!(
        auth.active_chatgpt_account_summary()
            .map(|summary| summary.store_account_id),
        Some("store-account-1".to_string())
    );
}

#[tokio::test]
async fn external_chatgpt_token_auth_manager_reload_and_resolution_stay_constructible() {
    let codex_home = tempdir().unwrap();
    save_auth(
        codex_home.path(),
        &external_chatgpt_auth_store("store-account-1", "account-123"),
        AuthCredentialsStoreMode::Ephemeral,
    )
    .expect("save external ChatGPT token auth store");

    let manager = AuthManager::new(
        codex_home.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
    );
    let cached_auth = manager
        .auth_cached()
        .expect("cached auth should be available");
    assert_eq!(cached_auth.api_auth_mode(), ApiAuthMode::ChatgptAuthTokens);

    manager.reload();
    let resolution = manager
        .resolve_chatgpt_auth_for_store_account_id(
            "store-account-1",
            ChatgptAccountRefreshMode::Never,
        )
        .await
        .expect("external auth account should resolve");

    let ChatgptAccountAuthResolution::Auth(auth) = resolution else {
        panic!("external auth account should resolve without removal");
    };
    assert_eq!(auth.api_auth_mode(), ApiAuthMode::ChatgptAuthTokens);
}

#[test]
fn auth_manager_prefers_external_chatgpt_tokens_over_persisted_auth() {
    let codex_home = tempdir().unwrap();
    save_auth(
        codex_home.path(),
        &chatgpt_auth_store_for_manager_logout(
            "persistent-store-account",
            "persistent-workspace",
            "persistent-access-token",
            "persistent-refresh-token",
        ),
        AuthCredentialsStoreMode::File,
    )
    .expect("save persistent auth store");
    save_auth(
        codex_home.path(),
        &external_chatgpt_auth_store("external-store-account", "external-workspace"),
        AuthCredentialsStoreMode::Ephemeral,
    )
    .expect("save external auth store");

    let manager = AuthManager::shared(
        codex_home.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
    );
    let auth = manager
        .auth_cached()
        .expect("external auth should be cached");

    assert_eq!(auth.api_auth_mode(), ApiAuthMode::ChatgptAuthTokens);
    assert_eq!(
        auth.active_chatgpt_account_summary()
            .map(|summary| summary.store_account_id),
        Some("external-store-account".to_string())
    );

    manager.reload();
    let reloaded_auth = manager
        .auth_cached()
        .expect("external auth should still be cached after reload");
    assert_eq!(
        reloaded_auth.api_auth_mode(),
        ApiAuthMode::ChatgptAuthTokens
    );
    assert_eq!(
        reloaded_auth
            .active_chatgpt_account_summary()
            .map(|summary| summary.store_account_id),
        Some("external-store-account".to_string())
    );
}

#[test]
fn auth_manager_falls_back_to_persisted_auth_when_external_store_is_not_admitted() {
    let codex_home = tempdir().unwrap();
    write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("pro".to_string()),
            chatgpt_account_id: Some("persistent-workspace".to_string()),
        },
        codex_home.path(),
    )
    .expect("write persistent auth file");
    save_auth(
        codex_home.path(),
        &external_chatgpt_auth_store_with_plan(
            "external-store-account",
            "external-workspace",
            Some(InternalPlanType::Unknown("mystery-tier".to_string())),
        ),
        AuthCredentialsStoreMode::Ephemeral,
    )
    .expect("save external auth store");
    let persistent_storage = create_auth_storage(
        codex_home.path().to_path_buf(),
        AuthCredentialsStoreMode::File,
    );
    let persistent_store = persistent_storage
        .load()
        .expect("load persistent auth store")
        .expect("persistent auth store should exist");
    let persistent_store_account_id = persistent_store
        .active_account()
        .expect("persistent auth store should keep an active account")
        .id
        .clone();

    let manager = AuthManager::new_with_sqlite_home(
        codex_home.path().to_path_buf(),
        codex_home.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
    );
    assert!(manager.account_manager.account_state_store.is_some());
    let cached_store = manager
        .inner
        .read()
        .expect("cached auth lock should be readable")
        .store
        .clone();
    let cached_auth = manager.auth_cached();
    assert!(
        cached_auth.is_some(),
        "persistent auth should remain cached; cached_store={cached_store:?}"
    );
    let auth = cached_auth.expect("persistent auth should remain cached");
    assert_eq!(auth.api_auth_mode(), ApiAuthMode::Chatgpt);
    assert_eq!(
        auth.active_chatgpt_account_summary()
            .map(|summary| summary.store_account_id),
        Some(persistent_store_account_id.clone())
    );

    manager.reload();
    let reloaded_auth = manager
        .auth_cached()
        .expect("persistent auth should still be cached after reload");
    assert_eq!(reloaded_auth.api_auth_mode(), ApiAuthMode::Chatgpt);
    assert_eq!(
        reloaded_auth
            .active_chatgpt_account_summary()
            .map(|summary| summary.store_account_id),
        Some(persistent_store_account_id)
    );

    let external_storage = create_auth_storage(
        codex_home.path().to_path_buf(),
        AuthCredentialsStoreMode::Ephemeral,
    );
    let external_store = external_storage
        .load()
        .expect("load external auth store")
        .unwrap_or_default();
    assert!(external_store.accounts.is_empty());
    assert_eq!(external_store.active_account_id, None);
}

#[test]
fn external_chatgpt_token_auth_persists_agent_identity_for_workspace() {
    let codex_home = tempdir().unwrap();
    save_auth(
        codex_home.path(),
        &external_chatgpt_auth_store("store-account-1", "account-123"),
        AuthCredentialsStoreMode::Ephemeral,
    )
    .expect("save external ChatGPT token auth store");
    let auth = super::load_auth(
        codex_home.path(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
    )
    .expect("load auth")
    .expect("auth available");
    let record = AgentIdentityAuthRecord {
        workspace_id: "account-123".to_string(),
        chatgpt_user_id: Some("user-123".to_string()),
        agent_runtime_id: "agent_123".to_string(),
        agent_private_key: "pkcs8-base64".to_string(),
        registered_at: "2026-04-13T12:00:00Z".to_string(),
    };

    auth.set_agent_identity(record.clone())
        .expect("set agent identity");

    assert_eq!(auth.get_agent_identity("account-123"), Some(record.clone()));
    let storage = create_auth_storage(
        codex_home.path().to_path_buf(),
        AuthCredentialsStoreMode::Ephemeral,
    );
    let persisted = storage
        .load()
        .expect("load auth")
        .expect("auth should exist");
    assert_eq!(persisted.agent_identity, Some(record));

    assert!(auth.remove_agent_identity().expect("remove agent identity"));
    assert_eq!(auth.get_agent_identity("account-123"), None);
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
    let mut updated_auth_dot_json = auth
        .get_current_auth_json()
        .expect("AuthDotJson should exist");
    let updated_tokens = updated_auth_dot_json
        .tokens
        .as_mut()
        .expect("tokens should exist");
    updated_tokens.access_token = "new-access-token".to_string();
    updated_tokens.refresh_token = "new-refresh-token".to_string();
    let updated_auth_home = tempdir().unwrap();
    let updated_auth_store = AuthStore::from_legacy(updated_auth_dot_json);
    save_auth(
        updated_auth_home.path(),
        &updated_auth_store,
        AuthCredentialsStoreMode::File,
    )
    .expect("updated auth should save");
    let updated_auth =
        CodexAuth::from_auth_storage(updated_auth_home.path(), AuthCredentialsStoreMode::File)
            .expect("updated auth should load")
            .expect("updated auth should exist");

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

#[tokio::test]
async fn auth_manager_notifies_when_auth_state_changes() {
    let dir = tempdir().unwrap();
    let manager = AuthManager::shared(
        dir.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
    );
    let mut auth_state_rx = manager.subscribe_auth_state();

    save_auth(
        dir.path(),
        &AuthStore::from_legacy(AuthDotJson {
            auth_mode: Some(ApiAuthMode::ApiKey),
            openai_api_key: Some("sk-test-key".to_string()),
            tokens: None,
            last_refresh: None,
            agent_identity: None,
        }),
        AuthCredentialsStoreMode::File,
    )
    .expect("save auth");

    assert!(
        manager.reload(),
        "reload should report a changed auth state"
    );
    timeout(Duration::from_secs(1), auth_state_rx.changed())
        .await
        .expect("auth change notification should arrive")
        .expect("auth state watch should remain open");

    save_auth(
        dir.path(),
        &AuthStore::from_legacy(AuthDotJson {
            auth_mode: Some(ApiAuthMode::ApiKey),
            openai_api_key: Some("sk-updated-key".to_string()),
            tokens: None,
            last_refresh: None,
            agent_identity: None,
        }),
        AuthCredentialsStoreMode::File,
    )
    .expect("save updated auth");

    assert!(
        manager.reload(),
        "reload should report changed auth when the underlying credentials change"
    );
    timeout(Duration::from_secs(1), auth_state_rx.changed())
        .await
        .expect("auth reload notification should still arrive")
        .expect("auth state watch should remain open");

    manager.set_forced_chatgpt_workspace_id(Some("workspace-123".to_string()));
    timeout(Duration::from_secs(1), auth_state_rx.changed())
        .await
        .expect("workspace change notification should arrive")
        .expect("auth state watch should remain open");
}

struct AuthFileParams {
    openai_api_key: Option<String>,
    chatgpt_plan_type: Option<String>,
    chatgpt_account_id: Option<String>,
}

fn write_auth_file(params: AuthFileParams, codex_home: &Path) -> std::io::Result<String> {
    let fake_jwt = fake_jwt_for_auth_file_params(&params)?;
    let auth_file = get_auth_file(codex_home);
    let auth_json_data = json!({
        "OPENAI_API_KEY": params.openai_api_key,
        "tokens": {
            "id_token": fake_jwt,
            "access_token": "test-access-token",
            "refresh_token": "test-refresh-token"
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
