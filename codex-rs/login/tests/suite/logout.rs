use anyhow::Context;
use anyhow::Result;
use base64::Engine;
use chrono::Utc;
use codex_account_state::accounts_db_path;
use codex_app_server_protocol::AuthMode;
use codex_config::types::AuthCredentialsStoreMode;
use codex_login::AuthDotJson;
use codex_login::AuthManager;
use codex_login::AuthManagerConfig;
use codex_login::AuthStore;
use codex_login::CLIENT_ID;
use codex_login::REVOKE_TOKEN_URL_OVERRIDE_ENV_VAR;
use codex_login::StoredAccount;
use codex_login::login_with_api_key;
use codex_login::logout_with_revoke;
use codex_login::save_auth;
use codex_login::token_data::IdTokenInfo;
use codex_login::token_data::TokenData;
use codex_protocol::auth::KnownPlan;
use codex_protocol::auth::PlanType;
use core_test_support::skip_if_no_network;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::ffi::OsString;
use std::path::Path;
use std::path::PathBuf;
use tempfile::TempDir;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

const ACCESS_TOKEN: &str = "access-token";
const REFRESH_TOKEN: &str = "refresh-token";

// Merge-safety anchor: logout integration tests must persist legacy fixtures through AuthStore
// so they keep exercising the live auth-store contract.

#[serial_test::serial(logout_revoke)]
#[tokio::test]
async fn logout_with_revoke_revokes_refresh_token_then_removes_auth() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/revoke"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "message": "success"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let _env_guard = EnvGuard::set(
        REVOKE_TOKEN_URL_OVERRIDE_ENV_VAR,
        format!("{}/oauth/revoke", server.uri()),
    );

    let codex_home = TempDir::new()?;
    save_legacy_auth(codex_home.path(), &chatgpt_auth())?;

    let config = TestAuthManagerConfig::new(codex_home.path(), codex_home.path());

    let removed = logout_with_revoke(&config).await?;

    assert!(removed);
    assert!(!codex_home.path().join("auth.json").exists());

    let requests = server
        .received_requests()
        .await
        .context("failed to fetch revoke requests")?;
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0]
            .body_json::<Value>()
            .context("revoke request should be JSON")?,
        json!({
            "token": REFRESH_TOKEN,
            "token_type_hint": "refresh_token",
            "client_id": CLIENT_ID,
        })
    );
    server.verify().await;
    Ok(())
}

#[serial_test::serial(logout_revoke)]
#[tokio::test]
async fn logout_with_revoke_removes_auth_when_revoke_fails() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/revoke"))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({
            "error": {
                "message": "revoke failed"
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    let _env_guard = EnvGuard::set(
        REVOKE_TOKEN_URL_OVERRIDE_ENV_VAR,
        format!("{}/oauth/revoke", server.uri()),
    );

    let codex_home = TempDir::new()?;
    save_legacy_auth(codex_home.path(), &chatgpt_auth())?;

    let config = TestAuthManagerConfig::new(codex_home.path(), codex_home.path());

    let removed = logout_with_revoke(&config).await?;

    assert!(removed);
    assert!(!codex_home.path().join("auth.json").exists());

    server.verify().await;
    Ok(())
}

#[tokio::test]
async fn logout_with_revoke_uses_configured_sqlite_home() -> Result<()> {
    let codex_home = TempDir::new()?;
    let sqlite_home = TempDir::new()?;
    login_with_api_key(
        codex_home.path(),
        "sk-test-key",
        AuthCredentialsStoreMode::File,
    )?;
    let config = TestAuthManagerConfig::new(codex_home.path(), sqlite_home.path());

    let removed = logout_with_revoke(&config).await?;

    assert!(removed);
    assert!(!codex_home.path().join("auth.json").exists());
    assert!(
        accounts_db_path(sqlite_home.path()).exists(),
        "configured sqlite_home should hold the account-state DB"
    );
    assert!(
        !accounts_db_path(codex_home.path()).exists(),
        "logout_with_revoke must not fall back to codex_home for account-state DB"
    );
    Ok(())
}

#[tokio::test]
async fn shared_from_config_applies_forced_workspace_before_cached_auth() -> Result<()> {
    let codex_home = TempDir::new()?;
    let sqlite_home = TempDir::new()?;
    let workspace_a = "workspace-a";
    let workspace_b = "workspace-b";
    save_auth(
        codex_home.path(),
        &AuthStore {
            active_account_id: Some("store-account-a".to_string()),
            accounts: vec![
                saved_chatgpt_account("store-account-a", workspace_a)?,
                saved_chatgpt_account("store-account-b", workspace_b)?,
            ],
            ..AuthStore::default()
        },
        AuthCredentialsStoreMode::File,
    )?;
    let config = TestAuthManagerConfig::new(codex_home.path(), sqlite_home.path())
        .with_forced_chatgpt_workspace_id(workspace_b);

    let manager =
        AuthManager::shared_from_config(&config, /*enable_codex_api_key_env*/ false)?;
    let auth = manager
        .auth()
        .await
        .expect("load auth")
        .expect("forced workspace should select matching saved account");

    assert_eq!(auth.get_account_id().as_deref(), Some(workspace_b));
    assert_eq!(
        auth.active_chatgpt_account_summary()
            .map(|summary| summary.store_account_id),
        Some("store-account-b".to_string())
    );
    Ok(())
}

#[serial_test::serial(logout_revoke)]
#[tokio::test]
async fn auth_manager_logout_with_revoke_uses_cached_auth() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/revoke"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "message": "success"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let _env_guard = EnvGuard::set(
        REVOKE_TOKEN_URL_OVERRIDE_ENV_VAR,
        format!("{}/oauth/revoke", server.uri()),
    );

    let codex_home = TempDir::new()?;
    save_legacy_auth(
        codex_home.path(),
        &chatgpt_auth_with_refresh_token(REFRESH_TOKEN),
    )?;
    let manager = AuthManager::new(
        codex_home.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
    )
    .expect("create auth manager");
    save_legacy_auth(
        codex_home.path(),
        &chatgpt_auth_with_refresh_token("newer-disk-refresh-token"),
    )?;

    let removed = manager.logout_with_revoke().await?;

    assert!(removed);
    assert!(manager.auth_cached()?.is_none());
    assert!(!codex_home.path().join("auth.json").exists());

    let requests = server
        .received_requests()
        .await
        .context("failed to fetch revoke requests")?;
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0]
            .body_json::<Value>()
            .context("revoke request should be JSON")?,
        json!({
            "token": REFRESH_TOKEN,
            "token_type_hint": "refresh_token",
            "client_id": CLIENT_ID,
        })
    );
    server.verify().await;
    Ok(())
}

fn chatgpt_auth() -> AuthDotJson {
    chatgpt_auth_with_refresh_token(REFRESH_TOKEN)
}

fn chatgpt_auth_with_refresh_token(refresh_token: &str) -> AuthDotJson {
    AuthDotJson {
        auth_mode: Some(AuthMode::Chatgpt),
        openai_api_key: None,
        tokens: Some(TokenData {
            id_token: IdTokenInfo {
                raw_jwt: minimal_jwt(),
                ..Default::default()
            },
            access_token: ACCESS_TOKEN.to_string(),
            refresh_token: refresh_token.to_string(),
            account_id: Some("account-id".to_string()),
        }),
        last_refresh: None,
        agent_identity: None,
    }
}

fn minimal_jwt() -> String {
    let b64 = |b: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b);
    let header_b64 = b64(br#"{"alg":"none"}"#);
    let payload_b64 = b64(br#"{"sub":"user-123"}"#);
    let signature_b64 = b64(b"sig");
    format!("{header_b64}.{payload_b64}.{signature_b64}")
}

fn chatgpt_jwt(store_account_id: &str, workspace_id: &str) -> Result<String> {
    let b64 = |b: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b);
    let header_b64 = b64(br#"{"alg":"none","typ":"JWT"}"#);
    let payload = json!({
        "email": format!("{store_account_id}@example.com"),
        "https://api.openai.com/auth": {
            "chatgpt_plan_type": "plus",
            "chatgpt_user_id": format!("user-{store_account_id}"),
            "user_id": format!("user-{store_account_id}"),
            "chatgpt_account_id": workspace_id,
            "chatgpt_account_is_fedramp": false,
        },
    });
    let payload_b64 = b64(&serde_json::to_vec(&payload)?);
    let signature_b64 = b64(b"sig");
    Ok(format!("{header_b64}.{payload_b64}.{signature_b64}"))
}

fn saved_chatgpt_account(store_account_id: &str, workspace_id: &str) -> Result<StoredAccount> {
    let raw_jwt = chatgpt_jwt(store_account_id, workspace_id)?;
    Ok(StoredAccount {
        id: store_account_id.to_string(),
        label: Some(store_account_id.to_string()),
        tokens: TokenData {
            id_token: IdTokenInfo {
                email: Some(format!("{store_account_id}@example.com")),
                chatgpt_plan_type: Some(PlanType::Known(KnownPlan::Plus)),
                chatgpt_user_id: Some(format!("user-{store_account_id}")),
                chatgpt_account_id: Some(workspace_id.to_string()),
                chatgpt_account_is_fedramp: false,
                raw_jwt,
            },
            access_token: format!("{store_account_id}-access-token"),
            refresh_token: format!("{store_account_id}-refresh-token"),
            account_id: Some(workspace_id.to_string()),
        },
        last_refresh: Some(Utc::now()),
        usage: None,
    })
}

fn save_legacy_auth(codex_home: &Path, auth_dot_json: &AuthDotJson) -> Result<()> {
    save_auth(
        codex_home,
        &AuthStore::from_legacy(auth_dot_json.clone()),
        AuthCredentialsStoreMode::File,
    )?;
    Ok(())
}

struct EnvGuard {
    key: &'static str,
    original: Option<OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: String) -> Self {
        let original = std::env::var_os(key);
        // SAFETY: these tests execute serially, so updating the process environment is safe.
        unsafe {
            std::env::set_var(key, &value);
        }
        Self { key, original }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: the guard restores the original environment value before other tests run.
        unsafe {
            match &self.original {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

struct TestAuthManagerConfig {
    codex_home: PathBuf,
    sqlite_home: PathBuf,
    forced_chatgpt_workspace_id: Option<String>,
}

impl TestAuthManagerConfig {
    fn new(codex_home: &Path, sqlite_home: &Path) -> Self {
        Self {
            codex_home: codex_home.to_path_buf(),
            sqlite_home: sqlite_home.to_path_buf(),
            forced_chatgpt_workspace_id: None,
        }
    }

    fn with_forced_chatgpt_workspace_id(mut self, workspace_id: &str) -> Self {
        self.forced_chatgpt_workspace_id = Some(workspace_id.to_string());
        self
    }
}

impl AuthManagerConfig for TestAuthManagerConfig {
    fn codex_home(&self) -> PathBuf {
        self.codex_home.clone()
    }

    fn sqlite_home(&self) -> PathBuf {
        self.sqlite_home.clone()
    }

    fn cli_auth_credentials_store_mode(&self) -> AuthCredentialsStoreMode {
        AuthCredentialsStoreMode::File
    }

    fn forced_chatgpt_workspace_id(&self) -> Option<String> {
        self.forced_chatgpt_workspace_id.clone()
    }

    fn chatgpt_base_url(&self) -> String {
        "https://chatgpt.com/backend-api".to_string()
    }
}
