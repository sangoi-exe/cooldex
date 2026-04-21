use super::*;
use crate::auth::storage::FileAuthStorage;
use crate::auth::storage::get_auth_file;
use crate::token_data::IdTokenInfo;
use chrono::Utc;
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
            .account_state_store
            .as_ref()
            .expect("account-state store should open")
            .account_is_leased_by_other("other-session", None, store_account_id, Utc::now())
            .expect("lease lookup should succeed")
    );

    assert!(manager.logout().expect("logout should succeed"));

    assert!(
        !manager
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
            .account_state_store
            .as_ref()
            .expect("account-state store should remain open")
            .account_is_leased_by_other("other-session", None, store_account_id, Utc::now())
            .expect("lease lookup should succeed")
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
    assert!(manager.account_state_store.is_some());
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
