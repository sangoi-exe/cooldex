use codex_core::config::Config;
use codex_login::AuthManager;
use codex_login::ChatGptRequestAuth;
use codex_login::default_client::create_client;

use crate::chatgpt_token::load_chatgpt_request_auth;
use anyhow::Context;
use serde::de::DeserializeOwned;
use std::time::Duration;

/// Make a GET request to the ChatGPT backend API.
pub(crate) async fn chatgpt_get_request<T: DeserializeOwned>(
    config: &Config,
    auth_manager: &AuthManager,
    path: String,
) -> anyhow::Result<T> {
    chatgpt_get_request_with_timeout(config, auth_manager, path, /*timeout*/ None).await
}

pub(crate) async fn chatgpt_get_request_with_timeout<T: DeserializeOwned>(
    config: &Config,
    auth_manager: &AuthManager,
    path: String,
    timeout: Option<Duration>,
) -> anyhow::Result<T> {
    // Merge-safety anchor: direct ChatGPT backend calls must use the
    // caller-owned request-auth snapshot so WS12 leases, forced workspace,
    // FedRAMP routing, and request account cannot drift through a hidden owner.
    let auth = load_chatgpt_request_auth(auth_manager)
        .await?
        .ok_or_else(|| anyhow::anyhow!("ChatGPT token not available"))?;
    chatgpt_get_request_with_auth(config, path, timeout, auth).await
}

pub(crate) async fn chatgpt_get_request_with_auth<T: DeserializeOwned>(
    config: &Config,
    path: String,
    timeout: Option<Duration>,
    auth: ChatGptRequestAuth,
) -> anyhow::Result<T> {
    let chatgpt_base_url = &config.chatgpt_base_url;
    let client = create_client();
    let url = format!("{chatgpt_base_url}{path}");

    let mut request = client
        .get(&url)
        .header("Authorization", auth.authorization())
        .header("chatgpt-account-id", auth.account_id())
        .header("Content-Type", "application/json");
    if auth.is_fedramp_account() {
        request = request.header("X-OpenAI-Fedramp", "true");
    }

    if let Some(timeout) = timeout {
        request = request.timeout(timeout);
    }

    let response = request.send().await.context("Failed to send request")?;

    if response.status().is_success() {
        let result: T = response
            .json()
            .await
            .context("Failed to parse JSON response")?;
        Ok(result)
    } else {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("Request failed with status {status}: {body}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_core::config::ConfigBuilder;
    use codex_login::AuthCredentialsStoreMode;
    use codex_login::AuthManager;
    use serde::Deserialize;
    use serde_json::json;
    use std::path::Path;
    use tempfile::TempDir;
    use tokio::io::AsyncReadExt;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    const FEDRAMP_BUSINESS_ID_TOKEN: &str = "eyJhbGciOiJub25lIn0.eyJlbWFpbCI6ImZlZEBleGFtcGxlLmNvbSIsImh0dHBzOi8vYXBpLm9wZW5haS5jb20vYXV0aCI6eyJjaGF0Z3B0X3BsYW5fdHlwZSI6ImJ1c2luZXNzIiwiY2hhdGdwdF91c2VyX2lkIjoidXNlci1mZWQiLCJjaGF0Z3B0X2FjY291bnRfaWQiOiJhY2NvdW50LWZlZCIsImNoYXRncHRfYWNjb3VudF9pc19mZWRyYW1wIjp0cnVlfX0.sig";

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    struct TestResponse {
        ok: bool,
    }

    async fn write_fedramp_chatgpt_auth(codex_home: &Path) -> anyhow::Result<ChatGptRequestAuth> {
        let store_account_id = "chatgpt-user:user-fed:workspace:account-fed";
        let auth_json = json!({
            "version": 1,
            "active_account_id": store_account_id,
            "accounts": [{
                "id": store_account_id,
                "tokens": {
                    "id_token": FEDRAMP_BUSINESS_ID_TOKEN,
                    "access_token": "access-token-fed",
                    "refresh_token": "refresh-token-fed",
                    "account_id": "account-fed"
                },
                "last_refresh": "3025-01-01T00:00:00Z"
            }]
        });
        std::fs::write(
            codex_home.join("auth.json"),
            serde_json::to_vec(&auth_json)?,
        )?;

        AuthManager::new(
            codex_home.to_path_buf(),
            /*enable_codex_api_key_env*/ false,
            AuthCredentialsStoreMode::File,
        )
        .chatgpt_request_auth()
        .await?
        .ok_or_else(|| anyhow::anyhow!("saved ChatGPT auth should produce request auth"))
    }

    async fn capture_single_get_request(
        listener: TcpListener,
        response_body: &'static str,
    ) -> anyhow::Result<String> {
        let (mut socket, _) = listener.accept().await?;
        let mut buffer = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            let bytes_read = socket.read(&mut chunk).await?;
            if bytes_read == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..bytes_read]);
            if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }

        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            response_body.len(),
            response_body
        );
        socket.write_all(response.as_bytes()).await?;
        Ok(String::from_utf8(buffer)?)
    }

    fn request_header(request: &str, header_name: &str) -> Option<String> {
        request.lines().skip(1).find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case(header_name)
                .then(|| value.trim().to_string())
        })
    }

    #[tokio::test]
    async fn chatgpt_get_request_with_auth_uses_request_auth_headers() -> anyhow::Result<()> {
        let temp_dir = TempDir::new()?;
        let codex_home = temp_dir.path().join("codex-home");
        let cwd = temp_dir.path().join("cwd");
        std::fs::create_dir_all(&codex_home)?;
        std::fs::create_dir_all(&cwd)?;

        let request_auth = write_fedramp_chatgpt_auth(&codex_home).await?;
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let base_url = format!("http://{}", listener.local_addr()?);
        let request_capture = tokio::spawn(capture_single_get_request(listener, r#"{"ok":true}"#));

        let mut config = ConfigBuilder::default()
            .codex_home(codex_home)
            .fallback_cwd(Some(cwd))
            .build()
            .await?;
        config.chatgpt_base_url = base_url;

        // Merge-safety anchor: direct ChatGPT backend requests must consume the
        // caller-owned request-auth snapshot, including FedRAMP routing metadata.
        let response: TestResponse = chatgpt_get_request_with_auth(
            &config,
            "/wham/header-check".to_string(),
            /*timeout*/ None,
            request_auth,
        )
        .await?;
        let request = request_capture.await??;

        assert_eq!(response, TestResponse { ok: true });
        assert!(
            request.starts_with("GET /wham/header-check HTTP/1.1"),
            "unexpected request line: {request:?}"
        );
        assert_eq!(
            request_header(&request, "authorization").as_deref(),
            Some("Bearer access-token-fed")
        );
        assert_eq!(
            request_header(&request, "chatgpt-account-id").as_deref(),
            Some("account-fed")
        );
        assert_eq!(
            request_header(&request, "x-openai-fedramp").as_deref(),
            Some("true")
        );
        Ok(())
    }
}
