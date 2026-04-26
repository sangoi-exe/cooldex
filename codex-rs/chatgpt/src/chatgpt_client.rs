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
