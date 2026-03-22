#![allow(clippy::unwrap_used)]

use anyhow::Context;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use codex_login::ServerOptions;
use codex_login::auth::AuthCredentialsStoreMode;
use codex_login::auth::UNSUPPORTED_CHATGPT_PLAN_REMOVED_MESSAGE;
use codex_login::auth::load_auth_store;
use codex_login::request_device_code;
use codex_login::run_device_code_login;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use tempfile::tempdir;
use tokio::time::Instant;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::Request;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

use core_test_support::skip_if_no_network;

// Merge-safety anchor: device-code auth-admission tests must stay aligned with
// the supported ChatGPT plan policy and persistence behavior.

// ---------- Small helpers  ----------

fn make_jwt(payload: serde_json::Value) -> String {
    let header = json!({ "alg": "none", "typ": "JWT" });
    let header_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap());
    let payload_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap());
    let signature_b64 = URL_SAFE_NO_PAD.encode(b"sig");
    format!("{header_b64}.{payload_b64}.{signature_b64}")
}

async fn mock_usercode_success(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/usercode"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_auth_id": "device-auth-123",
            "user_code": "CODE-12345",
            // NOTE: Interval is kept 0 in order to avoid waiting for the interval to pass
            "interval": "0"
        })))
        .mount(server)
        .await;
}

async fn mock_usercode_failure(server: &MockServer, status: u16) {
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/usercode"))
        .respond_with(ResponseTemplate::new(status))
        .mount(server)
        .await;
}

async fn mock_poll_token_two_step(
    server: &MockServer,
    counter: Arc<AtomicUsize>,
    first_response_status: u16,
) {
    let c = counter.clone();
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/token"))
        .respond_with(move |_: &Request| {
            let attempt = c.fetch_add(1, Ordering::SeqCst);
            if attempt == 0 {
                ResponseTemplate::new(first_response_status)
            } else {
                ResponseTemplate::new(200).set_body_json(json!({
                    "authorization_code": "poll-code-321",
                    "code_challenge": "code-challenge-321",
                    "code_verifier": "code-verifier-321"
                }))
            }
        })
        .expect(2)
        .mount(server)
        .await;
}

async fn mock_poll_token_single(server: &MockServer, endpoint: &str, response: ResponseTemplate) {
    Mock::given(method("POST"))
        .and(path(endpoint))
        .respond_with(response)
        .mount(server)
        .await;
}

async fn mock_oauth_token_single(server: &MockServer, jwt: String) {
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id_token": jwt.clone(),
            "access_token": "access-token-123",
            "refresh_token": "refresh-token-123"
        })))
        .mount(server)
        .await;
}

fn server_opts(
    codex_home: &tempfile::TempDir,
    issuer: String,
    cli_auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> ServerOptions {
    let mut opts = ServerOptions::new(
        codex_home.path().to_path_buf(),
        "client-id".to_string(),
        None,
        cli_auth_credentials_store_mode,
    );
    opts.issuer = issuer;
    opts.open_browser = false;
    opts
}

#[tokio::test]
async fn device_code_login_integration_succeeds() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let codex_home = tempdir().unwrap();
    let mock_server = MockServer::start().await;

    mock_usercode_success(&mock_server).await;

    mock_poll_token_two_step(&mock_server, Arc::new(AtomicUsize::new(0)), 404).await;

    let jwt = make_jwt(json!({
        "https://api.openai.com/auth": {
            "chatgpt_account_id": "acct_321",
            "chatgpt_plan_type": "business"
        }
    }));

    mock_oauth_token_single(&mock_server, jwt.clone()).await;

    let issuer = mock_server.uri();
    let opts = server_opts(&codex_home, issuer, AuthCredentialsStoreMode::File);

    run_device_code_login(opts)
        .await
        .expect("device code login integration should succeed");

    let auth = load_auth_store(codex_home.path(), AuthCredentialsStoreMode::File)
        .context("auth store should load after login succeeds")?
        .context("auth store written")?;
    let tokens = &auth.accounts.first().expect("account persisted").tokens;
    assert_eq!(tokens.access_token, "access-token-123");
    assert_eq!(tokens.refresh_token, "refresh-token-123");
    assert_eq!(tokens.id_token.raw_jwt, jwt);
    assert_eq!(tokens.account_id.as_deref(), Some("acct_321"));
    Ok(())
}

#[tokio::test]
async fn request_device_code_uses_server_verification_uri() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let codex_home = tempdir().unwrap();
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/usercode"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_auth_id": "device-auth-123",
            "user_code": "CODE-12345",
            "verification_uri": "https://example.com/custom-device",
            "interval": 7
        })))
        .mount(&mock_server)
        .await;

    let opts = server_opts(
        &codex_home,
        mock_server.uri(),
        AuthCredentialsStoreMode::File,
    );
    let device_code = request_device_code(&opts).await?;

    assert_eq!(
        device_code.verification_url,
        "https://example.com/custom-device"
    );
    assert_eq!(device_code.user_code, "CODE-12345");
    Ok(())
}

#[tokio::test]
async fn device_code_login_waits_one_interval_before_first_poll() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let codex_home = tempdir().unwrap();
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/usercode"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_auth_id": "device-auth-123",
            "user_code": "CODE-12345",
            "interval": 1,
            "expires_in": 10
        })))
        .mount(&mock_server)
        .await;

    mock_poll_token_single(
        &mock_server,
        "/api/accounts/deviceauth/token",
        ResponseTemplate::new(200).set_body_json(json!({
            "authorization_code": "poll-code-321",
            "code_challenge": "code-challenge-321",
            "code_verifier": "code-verifier-321"
        })),
    )
    .await;

    let jwt = make_jwt(json!({
        "https://api.openai.com/auth": {
            "chatgpt_account_id": "acct_321",
            "chatgpt_plan_type": "business"
        }
    }));
    mock_oauth_token_single(&mock_server, jwt).await;

    let opts = server_opts(
        &codex_home,
        mock_server.uri(),
        AuthCredentialsStoreMode::File,
    );
    let start = Instant::now();
    run_device_code_login(opts).await?;

    assert!(
        start.elapsed() >= std::time::Duration::from_millis(900),
        "first poll should wait one interval before hitting the token endpoint"
    );
    Ok(())
}

#[tokio::test]
async fn device_code_login_rejects_workspace_mismatch() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let codex_home = tempdir().unwrap();
    let mock_server = MockServer::start().await;

    mock_usercode_success(&mock_server).await;

    mock_poll_token_two_step(&mock_server, Arc::new(AtomicUsize::new(0)), 404).await;

    let jwt = make_jwt(json!({
        "https://api.openai.com/auth": {
            "chatgpt_account_id": "acct_321",
            "organization_id": "org-actual"
        }
    }));

    mock_oauth_token_single(&mock_server, jwt).await;

    let issuer = mock_server.uri();
    let mut opts = server_opts(&codex_home, issuer, AuthCredentialsStoreMode::File);
    opts.forced_chatgpt_workspace_id = Some("org-required".to_string());

    let err = run_device_code_login(opts)
        .await
        .expect_err("device code login should fail when workspace mismatches");
    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);

    let auth = load_auth_store(codex_home.path(), AuthCredentialsStoreMode::File)
        .context("auth store should load after login fails")?;
    assert!(
        auth.is_none(),
        "auth store should not be created when workspace validation fails"
    );
    Ok(())
}

#[tokio::test]
async fn device_code_login_rejects_unsupported_account() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let codex_home = tempdir().unwrap();
    let mock_server = MockServer::start().await;

    mock_usercode_success(&mock_server).await;
    mock_poll_token_two_step(&mock_server, Arc::new(AtomicUsize::new(0)), 404).await;

    let jwt = make_jwt(json!({
        "https://api.openai.com/auth": {
            "chatgpt_account_id": "acct_321",
            "chatgpt_plan_type": "free"
        }
    }));

    mock_oauth_token_single(&mock_server, jwt).await;

    let issuer = mock_server.uri();
    let opts = server_opts(&codex_home, issuer, AuthCredentialsStoreMode::File);

    let err = run_device_code_login(opts)
        .await
        .expect_err("device code login should fail for unsupported plan");
    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    assert_eq!(err.to_string(), UNSUPPORTED_CHATGPT_PLAN_REMOVED_MESSAGE);

    let auth = load_auth_store(codex_home.path(), AuthCredentialsStoreMode::File)
        .context("auth store should load after login fails")?;
    assert!(
        auth.is_none(),
        "auth store should not be created when plan validation fails"
    );
    Ok(())
}

#[tokio::test]
async fn device_code_login_retries_authorization_pending_payload() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let codex_home = tempdir().unwrap();
    let mock_server = MockServer::start().await;

    mock_usercode_success(&mock_server).await;

    let attempts = Arc::new(AtomicUsize::new(0));
    let counter = attempts.clone();
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/token"))
        .respond_with(move |_: &Request| {
            let attempt = counter.fetch_add(1, Ordering::SeqCst);
            if attempt == 0 {
                ResponseTemplate::new(400).set_body_json(json!({
                    "error": "authorization_pending",
                    "error_description": "still waiting"
                }))
            } else {
                ResponseTemplate::new(200).set_body_json(json!({
                    "authorization_code": "poll-code-321",
                    "code_challenge": "code-challenge-321",
                    "code_verifier": "code-verifier-321"
                }))
            }
        })
        .expect(2)
        .mount(&mock_server)
        .await;

    let jwt = make_jwt(json!({
        "https://api.openai.com/auth": {
            "chatgpt_account_id": "acct_321",
            "chatgpt_plan_type": "business"
        }
    }));

    mock_oauth_token_single(&mock_server, jwt).await;

    let opts = server_opts(
        &codex_home,
        mock_server.uri(),
        AuthCredentialsStoreMode::File,
    );
    run_device_code_login(opts)
        .await
        .expect("authorization_pending should retry until success");

    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    Ok(())
}

#[tokio::test]
async fn device_code_login_integration_handles_usercode_http_failure() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let codex_home = tempdir().unwrap();
    let mock_server = MockServer::start().await;

    mock_usercode_failure(&mock_server, 503).await;

    let issuer = mock_server.uri();

    let opts = server_opts(&codex_home, issuer, AuthCredentialsStoreMode::File);

    let err = run_device_code_login(opts)
        .await
        .expect_err("usercode HTTP failure should bubble up");
    assert!(
        err.to_string()
            .contains("device code request failed with status"),
        "unexpected error: {err:?}"
    );

    let auth = load_auth_store(codex_home.path(), AuthCredentialsStoreMode::File)
        .context("auth store should load after login fails")?;
    assert!(
        auth.is_none(),
        "auth store should not be created when login fails"
    );
    Ok(())
}

#[tokio::test]
async fn device_code_login_integration_persists_without_api_key_on_exchange_failure()
-> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let codex_home = tempdir().unwrap();

    let mock_server = MockServer::start().await;

    mock_usercode_success(&mock_server).await;

    mock_poll_token_two_step(&mock_server, Arc::new(AtomicUsize::new(0)), 404).await;

    let jwt = make_jwt(json!({
        "https://api.openai.com/auth": {
            "chatgpt_account_id": "acct_321",
            "chatgpt_plan_type": "pro"
        }
    }));

    mock_oauth_token_single(&mock_server, jwt.clone()).await;

    let issuer = mock_server.uri();

    let mut opts = ServerOptions::new(
        codex_home.path().to_path_buf(),
        "client-id".to_string(),
        None,
        AuthCredentialsStoreMode::File,
    );
    opts.issuer = issuer;
    opts.open_browser = false;

    run_device_code_login(opts)
        .await
        .expect("device login should succeed without API key exchange");

    let auth = load_auth_store(codex_home.path(), AuthCredentialsStoreMode::File)
        .context("auth store should load after login succeeds")?
        .context("auth store written")?;
    assert!(auth.openai_api_key.is_none());
    let tokens = &auth.accounts.first().expect("account persisted").tokens;
    assert_eq!(tokens.access_token, "access-token-123");
    assert_eq!(tokens.refresh_token, "refresh-token-123");
    assert_eq!(tokens.id_token.raw_jwt, jwt);
    Ok(())
}

#[tokio::test]
async fn device_code_login_integration_handles_error_payload() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let codex_home = tempdir().unwrap();

    // Start WireMock
    let mock_server = MockServer::start().await;

    mock_usercode_success(&mock_server).await;

    // // /deviceauth/token → returns error payload with status 401
    mock_poll_token_single(
        &mock_server,
        "/api/accounts/deviceauth/token",
        ResponseTemplate::new(401).set_body_json(json!({
            "error": "authorization_declined",
            "error_description": "Denied"
        })),
    )
    .await;

    // (WireMock will automatically 404 for other paths)

    let issuer = mock_server.uri();

    let mut opts = ServerOptions::new(
        codex_home.path().to_path_buf(),
        "client-id".to_string(),
        None,
        AuthCredentialsStoreMode::File,
    );
    opts.issuer = issuer;
    opts.open_browser = false;

    let err = run_device_code_login(opts)
        .await
        .expect_err("integration failure path should return error");

    // Accept either the specific error payload, a 400, or a 404 (since the client may return 404 if the flow is incomplete)
    assert!(
        err.to_string().contains("authorization_declined") || err.to_string().contains("401"),
        "Expected an authorization_declined / 400 / 404 error, got {err:?}"
    );

    let auth = load_auth_store(codex_home.path(), AuthCredentialsStoreMode::File)
        .context("auth store should load after login fails")?;
    assert!(
        auth.is_none(),
        "auth store should not be created when device auth fails"
    );
    Ok(())
}

#[tokio::test]
async fn device_code_login_treats_404_terminal_error_payload_as_terminal() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let codex_home = tempdir().unwrap();
    let mock_server = MockServer::start().await;

    mock_usercode_success(&mock_server).await;
    mock_poll_token_single(
        &mock_server,
        "/api/accounts/deviceauth/token",
        ResponseTemplate::new(404).set_body_json(json!({
            "error": "access_denied",
            "error_description": "Denied"
        })),
    )
    .await;

    let opts = server_opts(
        &codex_home,
        mock_server.uri(),
        AuthCredentialsStoreMode::File,
    );
    let err = run_device_code_login(opts)
        .await
        .expect_err("404 terminal payload should fail without retry loop");
    assert!(
        err.to_string().contains("access_denied"),
        "expected parsed terminal OAuth error, got {err:?}"
    );
    Ok(())
}

#[tokio::test]
async fn device_code_login_treats_403_terminal_error_payload_as_terminal() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let codex_home = tempdir().unwrap();
    let mock_server = MockServer::start().await;

    mock_usercode_success(&mock_server).await;
    mock_poll_token_single(
        &mock_server,
        "/api/accounts/deviceauth/token",
        ResponseTemplate::new(403).set_body_json(json!({
            "error": "expired_token",
            "error_description": "Expired"
        })),
    )
    .await;

    let opts = server_opts(
        &codex_home,
        mock_server.uri(),
        AuthCredentialsStoreMode::File,
    );
    let err = run_device_code_login(opts)
        .await
        .expect_err("403 terminal payload should fail without retry loop");
    assert!(
        err.to_string().contains("expired_token"),
        "expected parsed terminal OAuth error, got {err:?}"
    );
    Ok(())
}

#[tokio::test]
async fn device_code_login_respects_expires_in_before_first_poll() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let codex_home = tempdir().unwrap();
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/usercode"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_auth_id": "device-auth-123",
            "user_code": "CODE-12345",
            "interval": 1,
            "expires_in": 0
        })))
        .mount(&mock_server)
        .await;

    let opts = server_opts(
        &codex_home,
        mock_server.uri(),
        AuthCredentialsStoreMode::File,
    );
    let err = run_device_code_login(opts)
        .await
        .expect_err("expired device code should fail before polling");
    assert!(
        err.to_string().contains("timed out after 0 seconds"),
        "expected timeout derived from expires_in, got {err:?}"
    );
    Ok(())
}

#[tokio::test]
async fn device_code_login_does_not_poll_after_waiting_past_expiry() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let codex_home = tempdir().unwrap();
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/usercode"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_auth_id": "device-auth-123",
            "user_code": "CODE-12345",
            "interval": 2,
            "expires_in": 1
        })))
        .mount(&mock_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "authorization_code": "should-not-happen",
            "code_challenge": "should-not-happen",
            "code_verifier": "should-not-happen"
        })))
        .expect(0)
        .mount(&mock_server)
        .await;

    let opts = server_opts(
        &codex_home,
        mock_server.uri(),
        AuthCredentialsStoreMode::File,
    );
    let err = run_device_code_login(opts)
        .await
        .expect_err("device code should expire before the first poll is sent");
    assert!(
        err.to_string().contains("timed out after 1 seconds"),
        "expected timeout derived from expires_in before first poll, got {err:?}"
    );
    Ok(())
}

#[tokio::test]
async fn device_code_login_does_not_poll_after_slow_down_exhausts_expiry() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let codex_home = tempdir().unwrap();
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/usercode"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_auth_id": "device-auth-123",
            "user_code": "CODE-12345",
            "interval": 0,
            "expires_in": 1
        })))
        .mount(&mock_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": "slow_down",
            "error_description": "Wait longer"
        })))
        .expect(1)
        .mount(&mock_server)
        .await;

    let opts = server_opts(
        &codex_home,
        mock_server.uri(),
        AuthCredentialsStoreMode::File,
    );
    let err = run_device_code_login(opts)
        .await
        .expect_err("device code should stop once slow_down exhausts the expiry budget");
    assert!(
        err.to_string().contains("timed out after 1 seconds"),
        "expected timeout before a second poll after slow_down, got {err:?}"
    );
    Ok(())
}
