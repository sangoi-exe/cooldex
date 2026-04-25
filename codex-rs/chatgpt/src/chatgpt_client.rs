use codex_core::config::Config;
use codex_login::default_client::create_client;
use codex_login::token_data::TokenData;

use crate::chatgpt_token::load_chatgpt_token_data_from_auth;

use anyhow::Context;
use serde::de::DeserializeOwned;
use std::time::Duration;

/// Make a GET request to the ChatGPT backend API.
pub(crate) async fn chatgpt_get_request<T: DeserializeOwned>(
    config: &Config,
    path: String,
) -> anyhow::Result<T> {
    chatgpt_get_request_with_timeout(config, path, /*timeout*/ None).await
}

pub(crate) async fn chatgpt_get_request_with_timeout<T: DeserializeOwned>(
    config: &Config,
    path: String,
    timeout: Option<Duration>,
) -> anyhow::Result<T> {
    // Merge-safety anchor: direct ChatGPT backend calls must use the
    // config-aware token snapshot so forced workspace and WS12 sqlite_home
    // selection cannot drift from the request account or stale global state.
    let token = load_chatgpt_token_data_from_auth(config)
        .await?
        .ok_or_else(|| anyhow::anyhow!("ChatGPT token not available"))?;
    chatgpt_get_request_with_token(config, path, timeout, token).await
}

pub(crate) async fn chatgpt_get_request_with_token<T: DeserializeOwned>(
    config: &Config,
    path: String,
    timeout: Option<Duration>,
    token: TokenData,
) -> anyhow::Result<T> {
    let chatgpt_base_url = &config.chatgpt_base_url;
    // Make direct HTTP request to ChatGPT backend API with the token
    let client = create_client();
    let url = format!("{chatgpt_base_url}{path}");

    let account_id = token.account_id.ok_or_else(|| {
        anyhow::anyhow!("ChatGPT account ID not available, please re-run `codex login`")
    });

    let mut request = client
        .get(&url)
        .bearer_auth(&token.access_token)
        .header("chatgpt-account-id", account_id?)
        .header("Content-Type", "application/json");

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
