use chrono::DateTime;
use chrono::Local;
use chrono::Utc;
use reqwest::header::HeaderMap;
use std::sync::Arc;

use codex_core::config::Config;
use codex_login::AuthManager;
use codex_login::ChatGptRequestAuth;

pub fn set_user_agent_suffix(suffix: &str) {
    if let Ok(mut guard) = codex_login::default_client::USER_AGENT_SUFFIX.lock() {
        guard.replace(suffix.to_string());
    }
}

pub fn append_error_log(message: impl AsRef<str>) {
    let ts = Utc::now().to_rfc3339();
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("error.log")
    {
        use std::io::Write as _;
        let _ = writeln!(f, "[{ts}] {}", message.as_ref());
    }
}

/// Normalize the configured base URL to a canonical form used by the backend client.
/// - trims trailing '/'
/// - appends '/backend-api' for ChatGPT hosts when missing
pub fn normalize_base_url(input: &str) -> String {
    let mut base_url = input.to_string();
    while base_url.ends_with('/') {
        base_url.pop();
    }
    if (base_url.starts_with("https://chatgpt.com")
        || base_url.starts_with("https://chat.openai.com"))
        && !base_url.contains("/backend-api")
    {
        base_url = format!("{base_url}/backend-api");
    }
    base_url
}

pub fn auth_manager_from_config(config: &Config) -> Arc<AuthManager> {
    // Merge-safety anchor: cloud-task ChatGPT headers are a config-aware
    // production auth path, so AuthManager must receive sqlite_home and forced
    // workspace together before cached auth or account-state leases hydrate.
    AuthManager::shared_from_config(config, /*enable_codex_api_key_env*/ false)
}

/// Build headers for ChatGPT-backed requests from the command's request-auth snapshot.
pub fn build_chatgpt_headers(auth: &ChatGptRequestAuth) -> HeaderMap {
    use reqwest::header::AUTHORIZATION;
    use reqwest::header::HeaderName;
    use reqwest::header::HeaderValue;
    use reqwest::header::USER_AGENT;

    let ua = codex_login::default_client::get_codex_user_agent();
    let mut headers = HeaderMap::new();
    headers.insert(
        USER_AGENT,
        HeaderValue::from_str(&ua).unwrap_or(HeaderValue::from_static("codex-cli")),
    );
    // Merge-safety anchor: cloud-task environment probes share the command
    // AuthManager request-auth snapshot; never recover account id from JWTs
    // or mint a fresh AuthManager per probe.
    if let Ok(header_value) = HeaderValue::from_str(auth.authorization()) {
        headers.insert(AUTHORIZATION, header_value);
    }
    if let Ok(name) = HeaderName::from_bytes(b"ChatGPT-Account-Id")
        && let Ok(header_value) = HeaderValue::from_str(auth.account_id())
    {
        headers.insert(name, header_value);
    }
    if auth.is_fedramp_account()
        && let Ok(name) = HeaderName::from_bytes(b"X-OpenAI-Fedramp")
    {
        headers.insert(name, HeaderValue::from_static("true"));
    }
    headers
}

/// Construct a browser-friendly task URL for the given backend base URL.
pub fn task_url(base_url: &str, task_id: &str) -> String {
    let normalized = normalize_base_url(base_url);
    if let Some(root) = normalized.strip_suffix("/backend-api") {
        return format!("{root}/codex/tasks/{task_id}");
    }
    if let Some(root) = normalized.strip_suffix("/api/codex") {
        return format!("{root}/codex/tasks/{task_id}");
    }
    if normalized.ends_with("/codex") {
        return format!("{normalized}/tasks/{task_id}");
    }
    format!("{normalized}/codex/tasks/{task_id}")
}

pub fn format_relative_time(reference: DateTime<Utc>, ts: DateTime<Utc>) -> String {
    let mut secs = (reference - ts).num_seconds();
    if secs < 0 {
        secs = 0;
    }
    if secs < 60 {
        return format!("{secs}s ago");
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m ago");
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    let local = ts.with_timezone(&Local);
    local.format("%b %e %H:%M").to_string()
}

pub fn format_relative_time_now(ts: DateTime<Utc>) -> String {
    format_relative_time(Utc::now(), ts)
}
