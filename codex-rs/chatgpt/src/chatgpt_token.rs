#[cfg(test)]
use codex_login::AuthCredentialsStoreMode;
use codex_login::AuthManager;
use codex_login::AuthManagerConfig;
use codex_login::token_data::TokenData;
#[cfg(test)]
use std::path::PathBuf;

/// Load a ChatGPT request token snapshot from auth storage.
pub async fn load_chatgpt_token_data_from_auth(
    config: &impl AuthManagerConfig,
) -> std::io::Result<Option<TokenData>> {
    // Merge-safety anchor: ChatGPT token snapshots receive resolved config and
    // must preserve sqlite_home plus forced workspace before account-runtime
    // state hydrates; never reintroduce a process-global token owner here.
    let auth_manager =
        AuthManager::shared_from_config(config, /*enable_codex_api_key_env*/ false);
    auth_manager
        .auth()
        .await
        .map(|auth| auth.get_token_data())
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    struct TestAuthConfig {
        codex_home: PathBuf,
        sqlite_home: PathBuf,
    }

    impl AuthManagerConfig for TestAuthConfig {
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
            Some("missing-workspace".to_string())
        }
    }

    #[tokio::test]
    async fn token_snapshot_returns_none_without_matching_auth() -> std::io::Result<()> {
        let temp_dir = TempDir::new()?;
        let config = TestAuthConfig {
            codex_home: temp_dir.path().join("codex-home"),
            sqlite_home: temp_dir.path().join("sqlite-home"),
        };
        std::fs::create_dir_all(&config.codex_home)?;
        std::fs::create_dir_all(&config.sqlite_home)?;

        let token_data = load_chatgpt_token_data_from_auth(&config).await?;

        assert_eq!(token_data, None);
        Ok(())
    }
}
