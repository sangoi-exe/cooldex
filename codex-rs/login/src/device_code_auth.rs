use reqwest::StatusCode;
use serde::Deserialize;
use serde::Serialize;
use serde::de::Deserializer;
use serde::de::{self};
use std::time::Duration;
use std::time::Instant;

use crate::pkce::PkceCodes;
use crate::server::ServerOptions;
use codex_client::build_reqwest_client_with_custom_ca;
use std::io;

// Merge-safety anchor: device-code polling and verification URL handling are customized for the
// local auth/login contract and must stay reconciled during upstream auth changes.

const ANSI_BLUE: &str = "\x1b[94m";
const ANSI_GRAY: &str = "\x1b[90m";
const ANSI_RESET: &str = "\x1b[0m";
const DEFAULT_DEVICE_CODE_INTERVAL_SECS: u64 = 5;
const DEFAULT_DEVICE_CODE_EXPIRES_IN_SECS: u64 = 15 * 60;
const DEVICE_CODE_SLOW_DOWN_INCREMENT_SECS: u64 = 5;

#[derive(Debug, Clone)]
pub struct DeviceCode {
    pub verification_url: String,
    pub user_code: String,
    device_auth_id: String,
    interval: u64,
    expires_in_secs: u64,
}

#[derive(Deserialize)]
struct UserCodeResp {
    device_auth_id: String,
    #[serde(alias = "user_code", alias = "usercode")]
    user_code: String,
    #[serde(alias = "verification_url")]
    verification_uri: Option<String>,
    #[serde(
        default = "default_device_code_interval",
        deserialize_with = "deserialize_interval"
    )]
    interval: u64,
    #[serde(
        default = "default_device_code_expires_in",
        deserialize_with = "deserialize_interval"
    )]
    expires_in: u64,
}

#[derive(Serialize)]
struct UserCodeReq {
    client_id: String,
}

#[derive(Serialize)]
struct TokenPollReq {
    device_auth_id: String,
    user_code: String,
}

fn default_device_code_interval() -> u64 {
    DEFAULT_DEVICE_CODE_INTERVAL_SECS
}

fn default_device_code_expires_in() -> u64 {
    DEFAULT_DEVICE_CODE_EXPIRES_IN_SECS
}

fn deserialize_interval<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum IntervalValue {
        Integer(u64),
        String(String),
    }

    match IntervalValue::deserialize(deserializer)? {
        IntervalValue::Integer(value) => Ok(value),
        IntervalValue::String(value) => value
            .trim()
            .parse::<u64>()
            .map_err(|e| de::Error::custom(format!("invalid u64 string: {e}"))),
    }
}

#[derive(Deserialize)]
struct CodeSuccessResp {
    authorization_code: String,
    code_challenge: String,
    code_verifier: String,
}

#[derive(Deserialize)]
struct CodeErrorResp {
    error: String,
    error_description: Option<String>,
}

fn device_code_poll_error_message(error: &CodeErrorResp) -> String {
    match error.error_description.as_deref() {
        Some(description) if !description.is_empty() => {
            format!("device auth failed: {} ({description})", error.error)
        }
        _ => format!("device auth failed: {}", error.error),
    }
}

fn next_poll_interval_secs(current_interval: u64, error_code: &str) -> Option<u64> {
    match error_code {
        "authorization_pending" => Some(current_interval),
        "slow_down" => Some(current_interval.saturating_add(DEVICE_CODE_SLOW_DOWN_INCREMENT_SECS)),
        _ => None,
    }
}

fn next_device_code_sleep(
    start: Instant,
    max_wait: Duration,
    interval_secs: u64,
) -> std::io::Result<Duration> {
    ensure_device_code_not_expired(start, max_wait)?;
    Ok(Duration::from_secs(interval_secs).min(max_wait - start.elapsed()))
}

fn ensure_device_code_not_expired(start: Instant, max_wait: Duration) -> std::io::Result<()> {
    if start.elapsed() >= max_wait {
        return Err(std::io::Error::other(format!(
            "device auth timed out after {} seconds",
            max_wait.as_secs()
        )));
    }
    Ok(())
}

/// Request the user code and polling interval.
async fn request_user_code(
    client: &reqwest::Client,
    auth_base_url: &str,
    client_id: &str,
) -> std::io::Result<UserCodeResp> {
    let url = format!("{auth_base_url}/deviceauth/usercode");
    let body = serde_json::to_string(&UserCodeReq {
        client_id: client_id.to_string(),
    })
    .map_err(std::io::Error::other)?;
    let resp = client
        .post(url)
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .await
        .map_err(std::io::Error::other)?;

    if !resp.status().is_success() {
        let status = resp.status();
        if status == StatusCode::NOT_FOUND {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "device code login is not enabled for this Codex server. Use the browser login or verify the server URL.",
            ));
        }

        return Err(std::io::Error::other(format!(
            "device code request failed with status {status}"
        )));
    }

    let body = resp.text().await.map_err(std::io::Error::other)?;
    serde_json::from_str(&body).map_err(std::io::Error::other)
}

/// Poll token endpoint until a code is issued or timeout occurs.
async fn poll_for_token(
    client: &reqwest::Client,
    auth_base_url: &str,
    device_auth_id: &str,
    user_code: &str,
    interval: u64,
    expires_in_secs: u64,
) -> std::io::Result<CodeSuccessResp> {
    let url = format!("{auth_base_url}/deviceauth/token");
    let max_wait = Duration::from_secs(expires_in_secs);
    let start = Instant::now();
    let mut poll_interval_secs = interval;
    let initial_sleep = next_device_code_sleep(start, max_wait, poll_interval_secs)?;
    tokio::time::sleep(initial_sleep).await;

    loop {
        ensure_device_code_not_expired(start, max_wait)?;
        let body = serde_json::to_string(&TokenPollReq {
            device_auth_id: device_auth_id.to_string(),
            user_code: user_code.to_string(),
        })
        .map_err(std::io::Error::other)?;
        let resp = client
            .post(&url)
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await
            .map_err(std::io::Error::other)?;

        let status = resp.status();
        let body = resp.text().await.map_err(std::io::Error::other)?;

        if status.is_success() {
            return serde_json::from_str(&body).map_err(std::io::Error::other);
        }

        if let Ok(error) = serde_json::from_str::<CodeErrorResp>(&body) {
            if let Some(next_interval_secs) =
                next_poll_interval_secs(poll_interval_secs, error.error.as_str())
            {
                poll_interval_secs = next_interval_secs;
                let sleep_for = next_device_code_sleep(start, max_wait, poll_interval_secs)?;
                tokio::time::sleep(sleep_for).await;
                continue;
            }

            return Err(std::io::Error::other(device_code_poll_error_message(
                &error,
            )));
        }

        if status == StatusCode::FORBIDDEN || status == StatusCode::NOT_FOUND {
            let sleep_for = next_device_code_sleep(start, max_wait, poll_interval_secs)?;
            tokio::time::sleep(sleep_for).await;
            continue;
        }

        if body.trim().is_empty() {
            return Err(std::io::Error::other(format!(
                "device auth failed with status {status}"
            )));
        }

        return Err(std::io::Error::other(format!(
            "device auth failed with status {status}: {body}"
        )));
    }
}

fn expires_in_message(expires_in_secs: u64) -> String {
    if expires_in_secs.is_multiple_of(60) {
        return format!("expires in {} minutes", expires_in_secs / 60);
    }
    format!("expires in {expires_in_secs} seconds")
}

fn print_device_code_prompt(verification_url: &str, code: &str, expires_in_secs: u64) {
    let version = env!("CARGO_PKG_VERSION");
    let expiry_hint = expires_in_message(expires_in_secs);
    println!(
        "\nWelcome to Codex [v{ANSI_GRAY}{version}{ANSI_RESET}]\n{ANSI_GRAY}OpenAI's command-line coding agent{ANSI_RESET}\n\
\nFollow these steps to sign in with ChatGPT using device code authorization:\n\
\n1. Open this link in your browser and sign in to your account\n   {ANSI_BLUE}{verification_url}{ANSI_RESET}\n\
\n2. Enter this one-time code {ANSI_GRAY}({expiry_hint}){ANSI_RESET}\n   {ANSI_BLUE}{code}{ANSI_RESET}\n\
\n{ANSI_GRAY}Device codes are a common phishing target. Never share this code.{ANSI_RESET}\n",
    );
}

pub async fn request_device_code(opts: &ServerOptions) -> std::io::Result<DeviceCode> {
    let client = build_reqwest_client_with_custom_ca(reqwest::Client::builder())?;
    let base_url = opts.issuer.trim_end_matches('/');
    let api_base_url = format!("{base_url}/api/accounts");
    let uc = request_user_code(&client, &api_base_url, &opts.client_id).await?;

    Ok(DeviceCode {
        verification_url: uc
            .verification_uri
            .unwrap_or_else(|| format!("{base_url}/codex/device")),
        user_code: uc.user_code,
        device_auth_id: uc.device_auth_id,
        interval: uc.interval,
        expires_in_secs: uc.expires_in,
    })
}

pub async fn complete_device_code_login(
    opts: ServerOptions,
    device_code: DeviceCode,
) -> std::io::Result<()> {
    let client = build_reqwest_client_with_custom_ca(reqwest::Client::builder())?;
    let base_url = opts.issuer.trim_end_matches('/');
    let api_base_url = format!("{base_url}/api/accounts");

    let code_resp = poll_for_token(
        &client,
        &api_base_url,
        &device_code.device_auth_id,
        &device_code.user_code,
        device_code.interval,
        device_code.expires_in_secs,
    )
    .await?;

    let pkce = PkceCodes {
        code_verifier: code_resp.code_verifier,
        code_challenge: code_resp.code_challenge,
    };
    let redirect_uri = format!("{base_url}/deviceauth/callback");

    let tokens = crate::server::exchange_code_for_tokens(
        base_url,
        &opts.client_id,
        &redirect_uri,
        &pkce,
        &code_resp.authorization_code,
    )
    .await
    .map_err(|err| std::io::Error::other(format!("device code exchange failed: {err}")))?;

    if let Err(message) = crate::server::ensure_workspace_allowed(
        opts.forced_chatgpt_workspace_id.as_deref(),
        &tokens.id_token,
    ) {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, message));
    }

    crate::server::persist_tokens_async(
        &opts.codex_home,
        /*api_key*/ None,
        tokens.id_token,
        tokens.access_token,
        tokens.refresh_token,
        opts.cli_auth_credentials_store_mode,
    )
    .await
}

pub async fn run_device_code_login(opts: ServerOptions) -> std::io::Result<()> {
    let device_code = request_device_code(&opts).await?;
    print_device_code_prompt(
        &device_code.verification_url,
        &device_code.user_code,
        device_code.expires_in_secs,
    );
    complete_device_code_login(opts, device_code).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_poll_interval_secs_handles_rfc_retry_errors() {
        assert_eq!(next_poll_interval_secs(3, "authorization_pending"), Some(3));
        assert_eq!(next_poll_interval_secs(3, "slow_down"), Some(8));
        assert_eq!(next_poll_interval_secs(3, "access_denied"), None);
    }

    #[test]
    fn deserialize_interval_accepts_integer_string_and_default() {
        #[derive(Deserialize)]
        struct Wrapper {
            #[serde(
                default = "default_device_code_interval",
                deserialize_with = "deserialize_interval"
            )]
            interval: u64,
        }

        let numeric: Wrapper = serde_json::from_str("{\"interval\":7}").expect("numeric interval");
        let stringy: Wrapper =
            serde_json::from_str("{\"interval\":\"9\"}").expect("string interval");
        let defaulted: Wrapper = serde_json::from_str("{}").expect("default interval");

        assert_eq!(numeric.interval, 7);
        assert_eq!(stringy.interval, 9);
        assert_eq!(defaulted.interval, DEFAULT_DEVICE_CODE_INTERVAL_SECS);
    }

    #[test]
    fn expires_in_message_formats_minutes_and_seconds() {
        assert_eq!(expires_in_message(900), "expires in 15 minutes");
        assert_eq!(expires_in_message(75), "expires in 75 seconds");
    }
}
