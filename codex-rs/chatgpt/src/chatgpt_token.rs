#[cfg(test)]
use codex_login::AuthCredentialsStoreMode;
use codex_login::AuthManager;
#[cfg(test)]
use codex_login::AuthManagerConfig;
use codex_login::ChatGptRequestAuth;
#[cfg(test)]
use std::path::PathBuf;

/// Load a ChatGPT request-auth snapshot from the caller's runtime owner.
pub async fn load_chatgpt_request_auth(
    auth_manager: &AuthManager,
) -> std::io::Result<Option<ChatGptRequestAuth>> {
    // Merge-safety anchor: ChatGPT request snapshots must be derived from the
    // caller's lease-bearing AuthManager; never construct a hidden AccountManager
    // or reintroduce a process-global token owner here.
    Ok(auth_manager.chatgpt_request_auth().await)
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

        let auth_manager =
            AuthManager::shared_from_config(&config, /*enable_codex_api_key_env*/ false);
        let request_auth = load_chatgpt_request_auth(auth_manager.as_ref()).await?;

        assert_eq!(request_auth, None);
        Ok(())
    }
}
