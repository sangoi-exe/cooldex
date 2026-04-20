use std::path::Path;

use codex_config::types::AuthCredentialsStoreMode;
use codex_login::StoredAccount;
use codex_login::load_auth_store;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalChatgptAuth {
    pub(crate) store_account_id: String,
    pub(crate) access_token: String,
    pub(crate) chatgpt_account_id: String,
    pub(crate) chatgpt_plan_type: Option<String>,
}

fn load_local_chatgpt_auth_store(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    // Merge-safety anchor: local embedded ChatGPT auth refresh must read only the explicitly
    // selected persistent store so managed workspace auth keeps winning over ephemeral external
    // tokens.
) -> Result<codex_login::AuthStore, String> {
    // Merge-safety anchor: local embedded ChatGPT auth refresh must read only the explicitly
    // selected persistent store so managed workspace auth keeps winning over ephemeral external
    // tokens.
    let auth_store = load_auth_store(codex_home, auth_credentials_store_mode)
        .map_err(|err| format!("failed to load local auth: {err}"))?
        .ok_or_else(|| "no local auth available".to_string())?;
    if auth_store.openai_api_key.is_some() {
        return Err("local auth is not a ChatGPT login".to_string());
    }

    Ok(auth_store)
}

fn stored_account_chatgpt_account_id(account: &StoredAccount) -> Option<&str> {
    account
        .tokens
        .account_id
        .as_deref()
        .or(account.tokens.id_token.chatgpt_account_id.as_deref())
}

fn local_chatgpt_auth_from_store_account(
    account: &StoredAccount,
    forced_chatgpt_workspace_id: Option<&str>,
) -> Result<LocalChatgptAuth, String> {
    let tokens = &account.tokens;
    let access_token = tokens.access_token.clone();
    let chatgpt_account_id = stored_account_chatgpt_account_id(account)
        .map(str::to_string)
        .ok_or_else(|| "local ChatGPT auth is missing chatgpt account id".to_string())?;
    if let Some(expected_workspace) = forced_chatgpt_workspace_id
        && chatgpt_account_id != expected_workspace
    {
        return Err(format!(
            "local ChatGPT auth must use workspace {expected_workspace}, but found {chatgpt_account_id:?}"
        ));
    }

    let chatgpt_plan_type = tokens
        .id_token
        .get_chatgpt_plan_type_raw()
        .map(|plan_type| plan_type.to_ascii_lowercase());

    Ok(LocalChatgptAuth {
        store_account_id: account.id.clone(),
        access_token,
        chatgpt_account_id,
        chatgpt_plan_type,
    })
}

#[cfg(test)]
pub(crate) fn load_local_chatgpt_auth(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    forced_chatgpt_workspace_id: Option<&str>,
) -> Result<LocalChatgptAuth, String> {
    let auth_store = load_local_chatgpt_auth_store(codex_home, auth_credentials_store_mode)?;
    let account = auth_store
        .active_account_id
        .as_deref()
        .and_then(|account_id| {
            auth_store
                .accounts
                .iter()
                .find(|account| account.id == account_id)
        })
        .or_else(|| auth_store.accounts.first())
        .ok_or_else(|| "no local auth available".to_string())?;

    local_chatgpt_auth_from_store_account(account, forced_chatgpt_workspace_id)
}

pub(crate) fn load_local_chatgpt_auth_for_chatgpt_account_id(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    requested_chatgpt_account_id: &str,
    forced_chatgpt_workspace_id: Option<&str>,
) -> Result<LocalChatgptAuth, String> {
    let auth_store = load_local_chatgpt_auth_store(codex_home, auth_credentials_store_mode)?;
    let account = auth_store
        .accounts
        .iter()
        .find(|account| {
            stored_account_chatgpt_account_id(account) == Some(requested_chatgpt_account_id)
        })
        .ok_or_else(|| {
            format!("no saved ChatGPT account matches workspace {requested_chatgpt_account_id:?}")
        })?;

    local_chatgpt_auth_from_store_account(account, forced_chatgpt_workspace_id)
}

pub(crate) fn load_local_chatgpt_auth_for_store_account_id(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    requested_store_account_id: &str,
    forced_chatgpt_workspace_id: Option<&str>,
) -> Result<LocalChatgptAuth, String> {
    let auth_store = load_local_chatgpt_auth_store(codex_home, auth_credentials_store_mode)?;
    let account = auth_store
        .accounts
        .iter()
        .find(|account| account.id == requested_store_account_id)
        .ok_or_else(|| {
            format!(
                "no saved ChatGPT account matches store account id {requested_store_account_id:?}"
            )
        })?;

    local_chatgpt_auth_from_store_account(account, forced_chatgpt_workspace_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    use base64::Engine;
    use chrono::Utc;
    use codex_app_server_protocol::AuthMode;
    use codex_login::AuthDotJson;
    use codex_login::AuthStore;
    use codex_login::auth::login_with_chatgpt_auth_tokens;
    use codex_login::save_auth;
    use codex_login::token_data::TokenData;
    use pretty_assertions::assert_eq;
    use serde::Serialize;
    use serde_json::json;
    use tempfile::TempDir;

    fn fake_jwt(email: &str, account_id: &str, plan_type: &str) -> String {
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
            "email": email,
            "https://api.openai.com/auth": {
                "chatgpt_account_id": account_id,
                "chatgpt_plan_type": plan_type,
            },
        });
        let encode = |bytes: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        let header_b64 = encode(&serde_json::to_vec(&header).expect("serialize header"));
        let payload_b64 = encode(&serde_json::to_vec(&payload).expect("serialize payload"));
        let signature_b64 = encode(b"sig");
        format!("{header_b64}.{payload_b64}.{signature_b64}")
    }

    fn write_chatgpt_auth(codex_home: &Path, plan_type: &str) {
        let id_token = fake_jwt("user@example.com", "workspace-1", plan_type);
        let access_token = fake_jwt("user@example.com", "workspace-1", plan_type);
        let auth = AuthStore::from_legacy(AuthDotJson {
            auth_mode: Some(AuthMode::Chatgpt),
            openai_api_key: None,
            tokens: Some(TokenData {
                id_token: codex_login::token_data::parse_chatgpt_jwt_claims(&id_token)
                    .expect("id token should parse"),
                access_token,
                refresh_token: "refresh-token".to_string(),
                account_id: Some("workspace-1".to_string()),
            }),
            last_refresh: Some(Utc::now()),
            agent_identity: None,
        });
        save_auth(codex_home, &auth, AuthCredentialsStoreMode::File)
            .expect("chatgpt auth should save");
    }

    fn canonical_store_account_id(workspace_id: &str) -> String {
        format!("chatgpt-user:user-{workspace_id}:workspace:{workspace_id}")
    }

    fn stored_chatgpt_account(
        store_account_id: &str,
        workspace_id: &str,
        email: &str,
        plan_type: &str,
    ) -> StoredAccount {
        StoredAccount {
            id: store_account_id.to_string(),
            label: None,
            tokens: TokenData {
                id_token: codex_login::token_data::parse_chatgpt_jwt_claims(&fake_jwt(
                    email,
                    workspace_id,
                    plan_type,
                ))
                .expect("id token should parse"),
                access_token: fake_jwt(email, workspace_id, plan_type),
                refresh_token: format!("refresh-{workspace_id}"),
                account_id: Some(workspace_id.to_string()),
            },
            last_refresh: Some(Utc::now()),
            usage: None,
        }
    }

    #[test]
    fn loads_local_chatgpt_auth_from_managed_auth() {
        let codex_home = TempDir::new().expect("tempdir");
        write_chatgpt_auth(codex_home.path(), "business");

        let auth = load_local_chatgpt_auth(
            codex_home.path(),
            AuthCredentialsStoreMode::File,
            Some("workspace-1"),
        )
        .expect("chatgpt auth should load");

        assert_eq!(auth.chatgpt_account_id, "workspace-1");
        assert_eq!(auth.chatgpt_plan_type.as_deref(), Some("business"));
        assert!(!auth.access_token.is_empty());
    }

    #[test]
    fn loads_requested_saved_account_by_workspace_id() {
        let codex_home = TempDir::new().expect("tempdir");
        let requested_store_account_id = canonical_store_account_id("workspace-2");
        let auth = AuthStore {
            active_account_id: Some(canonical_store_account_id("workspace-1")),
            accounts: vec![
                stored_chatgpt_account(
                    &canonical_store_account_id("workspace-1"),
                    "workspace-1",
                    "first@example.com",
                    "business",
                ),
                stored_chatgpt_account(
                    &requested_store_account_id,
                    "workspace-2",
                    "second@example.com",
                    "enterprise",
                ),
            ],
            ..AuthStore::default()
        };
        save_auth(codex_home.path(), &auth, AuthCredentialsStoreMode::File)
            .expect("chatgpt auth should save");

        let auth = load_local_chatgpt_auth_for_chatgpt_account_id(
            codex_home.path(),
            AuthCredentialsStoreMode::File,
            "workspace-2",
            Some("workspace-2"),
        )
        .expect("requested workspace should load");

        assert_eq!(auth.store_account_id, requested_store_account_id);
        assert_eq!(auth.chatgpt_account_id, "workspace-2");
        assert_eq!(auth.chatgpt_plan_type.as_deref(), Some("enterprise"));
    }

    #[test]
    fn rejects_missing_local_auth() {
        let codex_home = TempDir::new().expect("tempdir");

        let err = load_local_chatgpt_auth(
            codex_home.path(),
            AuthCredentialsStoreMode::File,
            /*forced_chatgpt_workspace_id*/ None,
        )
        .expect_err("missing auth should fail");

        assert_eq!(err, "no local auth available");
    }

    #[test]
    fn rejects_api_key_auth() {
        let codex_home = TempDir::new().expect("tempdir");
        save_auth(
            codex_home.path(),
            &AuthStore {
                openai_api_key: Some("sk-test".to_string()),
                ..AuthStore::default()
            },
            AuthCredentialsStoreMode::File,
        )
        .expect("api key auth should save");

        let err = load_local_chatgpt_auth(
            codex_home.path(),
            AuthCredentialsStoreMode::File,
            /*forced_chatgpt_workspace_id*/ None,
        )
        .expect_err("api key auth should fail");

        assert_eq!(err, "local auth is not a ChatGPT login");
    }

    #[test]
    fn prefers_managed_auth_over_external_ephemeral_tokens() {
        let codex_home = TempDir::new().expect("tempdir");
        write_chatgpt_auth(codex_home.path(), "business");
        login_with_chatgpt_auth_tokens(
            codex_home.path(),
            &fake_jwt("user@example.com", "workspace-2", "enterprise"),
            "workspace-2",
            Some("enterprise"),
            None,
        )
        .expect("external auth should save");

        let auth = load_local_chatgpt_auth(
            codex_home.path(),
            AuthCredentialsStoreMode::File,
            Some("workspace-1"),
        )
        .expect("managed auth should win");

        assert_eq!(auth.chatgpt_account_id, "workspace-1");
        assert_eq!(auth.chatgpt_plan_type.as_deref(), Some("business"));
    }

    #[test]
    fn preserves_usage_based_plan_type_wire_name() {
        let codex_home = TempDir::new().expect("tempdir");
        write_chatgpt_auth(codex_home.path(), "self_serve_business_usage_based");

        let auth = load_local_chatgpt_auth(
            codex_home.path(),
            AuthCredentialsStoreMode::File,
            Some("workspace-1"),
        )
        .expect("chatgpt auth should load");

        assert_eq!(
            auth.chatgpt_plan_type.as_deref(),
            Some("self_serve_business_usage_based")
        );
    }
}
