use super::*;
use crate::auth::PreflightAuthState;
use crate::auth::load_auth_preflight_state;
use crate::token_data::IdTokenInfo;
use anyhow::Context;
use base64::Engine;
use codex_protocol::protocol::RateLimitSnapshot;
use codex_protocol::protocol::RateLimitWindow;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::tempdir;

use codex_keyring_store::tests::MockKeyringStore;
use keyring::Error as KeyringError;

// Merge-safety anchor: auth storage tests must exercise the AuthStore-backed
// persistence contract so file/keyring followers stay aligned with current
// auth-manager loading behavior.

fn auth_store_from_legacy(auth_dot_json: AuthDotJson) -> AuthStore {
    AuthStore::from_legacy(auth_dot_json)
}

#[tokio::test]
async fn file_storage_load_returns_auth_dot_json() -> anyhow::Result<()> {
    let codex_home = tempdir()?;
    let storage = FileAuthStorage::new(codex_home.path().to_path_buf());
    let auth_dot_json = AuthDotJson {
        auth_mode: Some(AuthMode::ApiKey),
        openai_api_key: Some("test-key".to_string()),
        tokens: None,
        last_refresh: Some(Utc::now()),
        agent_identity: None,
    };

    storage
        .save(&auth_store_from_legacy(auth_dot_json.clone()))
        .context("failed to save auth file")?;

    let loaded = storage.load().context("failed to load auth file")?;
    assert_eq!(Some(auth_store_from_legacy(auth_dot_json)), loaded);
    Ok(())
}

#[test]
fn auth_store_loads_old_usage_cache_without_losing_accounts() -> anyhow::Result<()> {
    let codex_home = tempdir()?;
    let storage = FileAuthStorage::new(codex_home.path().to_path_buf());
    let (expected, value) = auth_store_with_old_usage_cache()?;
    std::fs::write(
        get_auth_file(codex_home.path()),
        serde_json::to_string(&value)?,
    )?;

    let loaded = storage.load()?.context("auth store should load")?;

    assert_eq!(loaded, expected);
    Ok(())
}

#[test]
fn preflight_accepts_old_usage_cache_without_relogin() -> anyhow::Result<()> {
    let codex_home = tempdir()?;
    let (_, value) = auth_store_with_old_usage_cache()?;
    std::fs::write(
        get_auth_file(codex_home.path()),
        serde_json::to_string(&value)?,
    )?;

    let preflight = load_auth_preflight_state(
        codex_home.path(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
        Some("workspace-legacy-cache"),
    )?;

    assert_eq!(
        preflight,
        PreflightAuthState::Chatgpt {
            has_matching_workspace: true
        }
    );
    Ok(())
}

#[test]
fn versioned_auth_store_parse_errors_do_not_fallback_to_legacy() -> anyhow::Result<()> {
    let codex_home = tempdir()?;
    let storage = FileAuthStorage::new(codex_home.path().to_path_buf());
    std::fs::write(
        get_auth_file(codex_home.path()),
        r#"{"version":1,"active_account_id":"missing","accounts":[]}"#,
    )?;

    let err = storage
        .load()
        .expect_err("invalid versioned store should fail");

    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(
        err.to_string()
            .contains("failed to parse versioned auth.json")
    );
    assert!(
        err.to_string()
            .contains("active_account_id 'missing' does not exist")
    );
    Ok(())
}

#[tokio::test]
async fn file_storage_save_persists_auth_dot_json() -> anyhow::Result<()> {
    let codex_home = tempdir()?;
    let storage = FileAuthStorage::new(codex_home.path().to_path_buf());
    let auth_dot_json = AuthDotJson {
        auth_mode: Some(AuthMode::ApiKey),
        openai_api_key: Some("test-key".to_string()),
        tokens: None,
        last_refresh: Some(Utc::now()),
        agent_identity: None,
    };

    let file = get_auth_file(codex_home.path());
    storage
        .save(&auth_store_from_legacy(auth_dot_json.clone()))
        .context("failed to save auth file")?;

    let same_auth_store = storage
        .try_read_auth_store(&file)
        .context("failed to read auth file after save")?;
    assert_eq!(auth_store_from_legacy(auth_dot_json), same_auth_store);
    Ok(())
}

#[tokio::test]
async fn file_storage_persists_agent_identity() -> anyhow::Result<()> {
    let codex_home = tempdir()?;
    let storage = FileAuthStorage::new(codex_home.path().to_path_buf());
    let auth_dot_json = AuthDotJson {
        auth_mode: Some(AuthMode::Chatgpt),
        openai_api_key: None,
        tokens: None,
        last_refresh: Some(Utc::now()),
        agent_identity: Some(AgentIdentityAuthRecord {
            workspace_id: "account-123".to_string(),
            chatgpt_user_id: Some("user-123".to_string()),
            agent_runtime_id: "agent_123".to_string(),
            agent_private_key: "pkcs8-base64".to_string(),
            registered_at: "2026-04-13T12:00:00Z".to_string(),
        }),
    };

    storage.save(&auth_store_from_legacy(auth_dot_json.clone()))?;

    assert_eq!(storage.load()?, Some(auth_store_from_legacy(auth_dot_json)));
    Ok(())
}

#[test]
fn file_storage_delete_removes_auth_file() -> anyhow::Result<()> {
    let dir = tempdir()?;
    let auth_dot_json = AuthDotJson {
        auth_mode: Some(AuthMode::ApiKey),
        openai_api_key: Some("sk-test-key".to_string()),
        tokens: None,
        last_refresh: None,
        agent_identity: None,
    };
    let storage = create_auth_storage(dir.path().to_path_buf(), AuthCredentialsStoreMode::File);
    storage.save(&auth_store_from_legacy(auth_dot_json))?;
    assert!(dir.path().join("auth.json").exists());
    let storage = FileAuthStorage::new(dir.path().to_path_buf());
    let removed = storage.delete()?;
    assert!(removed);
    assert!(!dir.path().join("auth.json").exists());
    Ok(())
}

#[test]
fn ephemeral_storage_save_load_delete_is_in_memory_only() -> anyhow::Result<()> {
    let dir = tempdir()?;
    let storage = create_auth_storage(
        dir.path().to_path_buf(),
        AuthCredentialsStoreMode::Ephemeral,
    );
    let auth_dot_json = AuthDotJson {
        auth_mode: Some(AuthMode::ApiKey),
        openai_api_key: Some("sk-ephemeral".to_string()),
        tokens: None,
        last_refresh: Some(Utc::now()),
        agent_identity: None,
    };

    storage.save(&auth_store_from_legacy(auth_dot_json.clone()))?;
    let loaded = storage.load()?;
    assert_eq!(Some(auth_store_from_legacy(auth_dot_json)), loaded);

    let removed = storage.delete()?;
    assert!(removed);
    let loaded = storage.load()?;
    assert_eq!(None, loaded);
    assert!(!get_auth_file(dir.path()).exists());
    Ok(())
}

fn seed_keyring_and_fallback_auth_file_for_delete<F>(
    mock_keyring: &MockKeyringStore,
    codex_home: &Path,
    compute_key: F,
) -> anyhow::Result<(String, PathBuf)>
where
    F: FnOnce() -> std::io::Result<String>,
{
    let key = compute_key()?;
    mock_keyring.save(KEYRING_SERVICE, &key, "{}")?;
    let auth_file = get_auth_file(codex_home);
    std::fs::write(&auth_file, "stale")?;
    Ok((key, auth_file))
}

fn seed_keyring_with_auth<F>(
    mock_keyring: &MockKeyringStore,
    compute_key: F,
    auth: &AuthStore,
) -> anyhow::Result<()>
where
    F: FnOnce() -> std::io::Result<String>,
{
    let key = compute_key()?;
    let serialized = serde_json::to_string(auth)?;
    mock_keyring.save(KEYRING_SERVICE, &key, &serialized)?;
    Ok(())
}

fn assert_keyring_saved_auth_and_removed_fallback(
    mock_keyring: &MockKeyringStore,
    key: &str,
    codex_home: &Path,
    expected: &AuthStore,
) {
    let saved_value = mock_keyring
        .saved_value(key)
        .expect("keyring entry should exist");
    let expected_serialized = serde_json::to_string(expected).expect("serialize expected auth");
    assert_eq!(saved_value, expected_serialized);
    let auth_file = get_auth_file(codex_home);
    assert!(
        !auth_file.exists(),
        "fallback auth.json should be removed after keyring save"
    );
}

fn id_token_with_prefix(prefix: &str) -> IdTokenInfo {
    #[derive(Serialize)]
    struct Header {
        alg: &'static str,
        typ: &'static str,
    }

    let header = Header {
        alg: "none",
        typ: "JWT",
    };
    let payload = json!({
        "email": format!("{prefix}@example.com"),
        "https://api.openai.com/auth": {
            "chatgpt_account_id": format!("{prefix}-account"),
        },
    });
    let encode = |bytes: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let header_b64 = encode(&serde_json::to_vec(&header).expect("serialize header"));
    let payload_b64 = encode(&serde_json::to_vec(&payload).expect("serialize payload"));
    let signature_b64 = encode(b"sig");
    let fake_jwt = format!("{header_b64}.{payload_b64}.{signature_b64}");

    crate::token_data::parse_chatgpt_jwt_claims(&fake_jwt).expect("fake JWT should parse")
}

fn auth_with_prefix(prefix: &str) -> AuthStore {
    auth_store_from_legacy(AuthDotJson {
        auth_mode: Some(AuthMode::ApiKey),
        openai_api_key: Some(format!("{prefix}-api-key")),
        tokens: Some(TokenData {
            id_token: id_token_with_prefix(prefix),
            access_token: format!("{prefix}-access"),
            refresh_token: format!("{prefix}-refresh"),
            account_id: Some(format!("{prefix}-account-id")),
        }),
        last_refresh: None,
        agent_identity: None,
    })
}

fn supported_chatgpt_id_token_with_prefix(prefix: &str, workspace_id: &str) -> IdTokenInfo {
    #[derive(Serialize)]
    struct Header {
        alg: &'static str,
        typ: &'static str,
    }

    let header = Header {
        alg: "none",
        typ: "JWT",
    };
    let payload = json!({
        "email": format!("{prefix}@example.com"),
        "https://api.openai.com/auth": {
            "chatgpt_plan_type": "team",
            "chatgpt_user_id": format!("user-{prefix}"),
            "chatgpt_account_id": workspace_id,
        },
    });
    let encode = |bytes: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let header_b64 = encode(&serde_json::to_vec(&header).expect("serialize header"));
    let payload_b64 = encode(&serde_json::to_vec(&payload).expect("serialize payload"));
    let signature_b64 = encode(b"sig");
    let fake_jwt = format!("{header_b64}.{payload_b64}.{signature_b64}");

    crate::token_data::parse_chatgpt_jwt_claims(&fake_jwt).expect("fake JWT should parse")
}

fn fixed_usage_time() -> chrono::DateTime<Utc> {
    chrono::DateTime::parse_from_rfc3339("2026-04-27T04:51:00Z")
        .expect("fixed time should parse")
        .with_timezone(&Utc)
}

fn auth_store_with_old_usage_cache() -> anyhow::Result<(AuthStore, serde_json::Value)> {
    let workspace_id = "workspace-legacy-cache";
    let store_account_id = format!("chatgpt-user:user-legacy-cache:workspace:{workspace_id}");
    let expected = AuthStore {
        active_account_id: Some(store_account_id.clone()),
        accounts: vec![StoredAccount {
            id: store_account_id,
            label: Some("Legacy usage cache".to_string()),
            tokens: TokenData {
                id_token: supported_chatgpt_id_token_with_prefix("legacy-cache", workspace_id),
                access_token: "legacy-cache-access".to_string(),
                refresh_token: "legacy-cache-refresh".to_string(),
                account_id: Some(workspace_id.to_string()),
            },
            last_refresh: Some(fixed_usage_time()),
            usage: Some(AccountUsageCache {
                last_rate_limits: Some(RateLimitSnapshot {
                    limit_id: Some("codex".to_string()),
                    limit_name: None,
                    primary: Some(RateLimitWindow {
                        remaining_percent: 53.0,
                        window_minutes: Some(300),
                        resets_at: Some(1_777_283_454),
                    }),
                    secondary: Some(RateLimitWindow {
                        remaining_percent: 20.0,
                        window_minutes: Some(10_080),
                        resets_at: Some(1_777_596_056),
                    }),
                    credits: None,
                    plan_type: None,
                    rate_limit_reached_type: None,
                }),
                exhausted_until: None,
                last_seen_at: Some(fixed_usage_time()),
            }),
        }],
        ..AuthStore::default()
    };
    let mut value = serde_json::to_value(&expected)?;
    replace_remaining_percent_with_used_percent(
        &mut value,
        "/accounts/0/usage/last_rate_limits/primary",
        47.0,
    )?;
    replace_remaining_percent_with_used_percent(
        &mut value,
        "/accounts/0/usage/last_rate_limits/secondary",
        80.0,
    )?;
    Ok((expected, value))
}

fn replace_remaining_percent_with_used_percent(
    value: &mut serde_json::Value,
    pointer: &str,
    used_percent: f64,
) -> anyhow::Result<()> {
    let window = value
        .pointer_mut(pointer)
        .and_then(serde_json::Value::as_object_mut)
        .context("rate-limit window should exist")?;
    window
        .remove("remaining_percent")
        .context("remaining_percent should exist before legacy rewrite")?;
    window.insert("used_percent".to_string(), json!(used_percent));
    Ok(())
}

#[test]
fn keyring_auth_storage_load_returns_deserialized_auth() -> anyhow::Result<()> {
    let codex_home = tempdir()?;
    let mock_keyring = MockKeyringStore::default();
    let storage = KeyringAuthStorage::new(
        codex_home.path().to_path_buf(),
        Arc::new(mock_keyring.clone()),
    );
    let expected = auth_store_from_legacy(AuthDotJson {
        auth_mode: Some(AuthMode::ApiKey),
        openai_api_key: Some("sk-test".to_string()),
        tokens: None,
        last_refresh: None,
        agent_identity: None,
    });
    seed_keyring_with_auth(
        &mock_keyring,
        || compute_store_key(codex_home.path()),
        &expected,
    )?;

    let loaded = storage.load()?;
    assert_eq!(Some(expected), loaded);
    Ok(())
}

#[test]
fn keyring_auth_storage_compute_store_key_for_home_directory() -> anyhow::Result<()> {
    let codex_home = PathBuf::from("~/.codex");

    let key = compute_store_key(codex_home.as_path())?;

    assert_eq!(key, "cli|940db7b1d0e4eb40");
    Ok(())
}

#[test]
fn keyring_auth_storage_save_persists_and_removes_fallback_file() -> anyhow::Result<()> {
    let codex_home = tempdir()?;
    let mock_keyring = MockKeyringStore::default();
    let storage = KeyringAuthStorage::new(
        codex_home.path().to_path_buf(),
        Arc::new(mock_keyring.clone()),
    );
    let auth_file = get_auth_file(codex_home.path());
    std::fs::write(&auth_file, "stale")?;
    let auth = auth_store_from_legacy(AuthDotJson {
        auth_mode: Some(AuthMode::Chatgpt),
        openai_api_key: None,
        tokens: Some(TokenData {
            id_token: Default::default(),
            access_token: "access".to_string(),
            refresh_token: "refresh".to_string(),
            account_id: Some("account".to_string()),
        }),
        last_refresh: Some(Utc::now()),
        agent_identity: None,
    });

    storage.save(&auth)?;

    let key = compute_store_key(codex_home.path())?;
    assert_keyring_saved_auth_and_removed_fallback(&mock_keyring, &key, codex_home.path(), &auth);
    Ok(())
}

#[test]
fn keyring_auth_storage_delete_removes_keyring_and_file() -> anyhow::Result<()> {
    let codex_home = tempdir()?;
    let mock_keyring = MockKeyringStore::default();
    let storage = KeyringAuthStorage::new(
        codex_home.path().to_path_buf(),
        Arc::new(mock_keyring.clone()),
    );
    let (key, auth_file) =
        seed_keyring_and_fallback_auth_file_for_delete(&mock_keyring, codex_home.path(), || {
            compute_store_key(codex_home.path())
        })?;

    let removed = storage.delete()?;

    assert!(removed, "delete should report removal");
    assert!(
        !mock_keyring.contains(&key),
        "keyring entry should be removed"
    );
    assert!(
        !auth_file.exists(),
        "fallback auth.json should be removed after keyring delete"
    );
    Ok(())
}

#[test]
fn auto_auth_storage_load_prefers_keyring_value() -> anyhow::Result<()> {
    let codex_home = tempdir()?;
    let mock_keyring = MockKeyringStore::default();
    let storage = AutoAuthStorage::new(
        codex_home.path().to_path_buf(),
        Arc::new(mock_keyring.clone()),
    );
    let keyring_auth = auth_with_prefix("keyring");
    seed_keyring_with_auth(
        &mock_keyring,
        || compute_store_key(codex_home.path()),
        &keyring_auth,
    )?;

    let file_auth = auth_with_prefix("file");
    storage.file_storage.save(&file_auth)?;

    let loaded = storage.load()?;
    assert_eq!(loaded, Some(keyring_auth));
    Ok(())
}

#[test]
fn auto_auth_storage_load_uses_file_when_keyring_empty() -> anyhow::Result<()> {
    let codex_home = tempdir()?;
    let mock_keyring = MockKeyringStore::default();
    let storage = AutoAuthStorage::new(codex_home.path().to_path_buf(), Arc::new(mock_keyring));

    let expected = auth_with_prefix("file-only");
    storage.file_storage.save(&expected)?;

    let loaded = storage.load()?;
    assert_eq!(loaded, Some(expected));
    Ok(())
}

#[test]
fn auto_auth_storage_load_falls_back_when_keyring_errors() -> anyhow::Result<()> {
    let codex_home = tempdir()?;
    let mock_keyring = MockKeyringStore::default();
    let storage = AutoAuthStorage::new(
        codex_home.path().to_path_buf(),
        Arc::new(mock_keyring.clone()),
    );
    let key = compute_store_key(codex_home.path())?;
    mock_keyring.set_error(&key, KeyringError::Invalid("error".into(), "load".into()));

    let expected = auth_with_prefix("fallback");
    storage.file_storage.save(&expected)?;

    let loaded = storage.load()?;
    assert_eq!(loaded, Some(expected));
    Ok(())
}

#[test]
fn auto_auth_storage_save_prefers_keyring() -> anyhow::Result<()> {
    let codex_home = tempdir()?;
    let mock_keyring = MockKeyringStore::default();
    let storage = AutoAuthStorage::new(
        codex_home.path().to_path_buf(),
        Arc::new(mock_keyring.clone()),
    );
    let key = compute_store_key(codex_home.path())?;

    let stale = auth_with_prefix("stale");
    storage.file_storage.save(&stale)?;

    let expected = auth_with_prefix("to-save");
    storage.save(&expected)?;

    assert_keyring_saved_auth_and_removed_fallback(
        &mock_keyring,
        &key,
        codex_home.path(),
        &expected,
    );
    Ok(())
}

#[test]
fn auto_auth_storage_save_falls_back_when_keyring_errors() -> anyhow::Result<()> {
    let codex_home = tempdir()?;
    let mock_keyring = MockKeyringStore::default();
    let storage = AutoAuthStorage::new(
        codex_home.path().to_path_buf(),
        Arc::new(mock_keyring.clone()),
    );
    let key = compute_store_key(codex_home.path())?;
    mock_keyring.set_error(&key, KeyringError::Invalid("error".into(), "save".into()));

    let auth = auth_with_prefix("fallback");
    storage.save(&auth)?;

    let auth_file = get_auth_file(codex_home.path());
    assert!(
        auth_file.exists(),
        "fallback auth.json should be created when keyring save fails"
    );
    let saved = storage
        .file_storage
        .load()?
        .context("fallback auth should exist")?;
    assert_eq!(saved, auth);
    assert!(
        mock_keyring.saved_value(&key).is_none(),
        "keyring should not contain value when save fails"
    );
    Ok(())
}

#[test]
fn auto_auth_storage_delete_removes_keyring_and_file() -> anyhow::Result<()> {
    let codex_home = tempdir()?;
    let mock_keyring = MockKeyringStore::default();
    let storage = AutoAuthStorage::new(
        codex_home.path().to_path_buf(),
        Arc::new(mock_keyring.clone()),
    );
    let (key, auth_file) =
        seed_keyring_and_fallback_auth_file_for_delete(&mock_keyring, codex_home.path(), || {
            compute_store_key(codex_home.path())
        })?;

    let removed = storage.delete()?;

    assert!(removed, "delete should report removal");
    assert!(
        !mock_keyring.contains(&key),
        "keyring entry should be removed"
    );
    assert!(
        !auth_file.exists(),
        "fallback auth.json should be removed after delete"
    );
    Ok(())
}
