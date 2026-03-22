use super::AuthRequestTelemetryContext;
use super::ModelClient;
use super::PendingUnauthorizedRetry;
use super::UnauthorizedRecoveryExecution;
use super::X_CODEX_TURN_STATE_HEADER;
use super::build_responses_headers;
use crate::ResponseEvent;
use crate::auth::AuthCredentialsStoreMode;
use crate::auth::AuthStore;
use crate::auth::StoredAccount;
use crate::auth::save_auth;
use crate::client_common::Prompt;
use base64::Engine;
use chrono::Utc;
use codex_otel::SessionTelemetry;
use codex_protocol::ThreadId;
use codex_protocol::config_types::ReasoningSummary as ReasoningSummaryConfig;
use codex_protocol::models::BaseInstructions;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::start_websocket_server;
use futures::StreamExt;
use pretty_assertions::assert_eq;
use serde::Serialize;
use serde_json::json;
use std::collections::HashSet;
use tempfile::tempdir;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path_regex;

// Merge-safety anchor: client tests guard the customized session/provider wiring used by local
// auth, retry, and hidden-runtime behavior.

fn test_model_client(session_source: SessionSource) -> ModelClient {
    let provider = test_provider("https://example.com/v1");
    ModelClient::new(
        None,
        ThreadId::new(),
        provider,
        session_source,
        None,
        false,
        false,
        None,
    )
}

fn test_provider(base_url: &str) -> crate::ModelProviderInfo {
    let mut provider = crate::model_provider_info::create_oss_provider_with_base_url(
        base_url,
        crate::model_provider_info::WireApi::Responses,
    );
    provider.supports_websockets = true;
    provider
}

fn test_model_info() -> ModelInfo {
    serde_json::from_value(json!({
        "slug": "gpt-test",
        "display_name": "gpt-test",
        "description": "desc",
        "default_reasoning_level": "medium",
        "supported_reasoning_levels": [
            {"effort": "medium", "description": "medium"}
        ],
        "shell_type": "shell_command",
        "visibility": "list",
        "supported_in_api": true,
        "priority": 1,
        "upgrade": null,
        "base_instructions": "base instructions",
        "model_messages": null,
        "supports_reasoning_summaries": false,
        "support_verbosity": false,
        "default_verbosity": null,
        "apply_patch_tool_type": null,
        "truncation_policy": {"mode": "bytes", "limit": 10000},
        "supports_parallel_tool_calls": false,
        "supports_image_detail_original": false,
        "context_window": 272000,
        "auto_compact_token_limit": null,
        "experimental_supported_tools": []
    }))
    .expect("deserialize test model info")
}

fn test_session_telemetry() -> SessionTelemetry {
    SessionTelemetry::new(
        ThreadId::new(),
        "gpt-test",
        "gpt-test",
        None,
        None,
        None,
        "test-originator".to_string(),
        false,
        "test-terminal".to_string(),
        SessionSource::Cli,
    )
}

fn test_chatgpt_token_data(chatgpt_account_id: &str) -> crate::token_data::TokenData {
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
        "email": "user@example.com",
        "email_verified": true,
        "https://api.openai.com/auth": {
            "chatgpt_plan_type": "pro",
            "chatgpt_user_id": "user-12345",
            "user_id": "user-12345",
            "chatgpt_account_id": chatgpt_account_id,
        }
    });
    let b64 = |bytes: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let header_b64 = b64(&serde_json::to_vec(&header).expect("serialize header"));
    let payload_b64 = b64(&serde_json::to_vec(&payload).expect("serialize payload"));
    let signature_b64 = b64(b"sig");
    let fake_jwt = format!("{header_b64}.{payload_b64}.{signature_b64}");

    crate::token_data::TokenData {
        id_token: crate::token_data::IdTokenInfo {
            raw_jwt: fake_jwt,
            ..crate::token_data::IdTokenInfo::default()
        },
        access_token: format!("access-{chatgpt_account_id}"),
        refresh_token: format!("refresh-{chatgpt_account_id}"),
        account_id: Some(chatgpt_account_id.to_string()),
    }
}

#[test]
fn build_subagent_headers_sets_other_subagent_label() {
    let client = test_model_client(SessionSource::SubAgent(SubAgentSource::Other(
        "memory_consolidation".to_string(),
    )));
    let headers = client.build_subagent_headers();
    let value = headers
        .get("x-openai-subagent")
        .and_then(|value| value.to_str().ok());
    assert_eq!(value, Some("memory_consolidation"));
}

#[tokio::test]
async fn summarize_memories_returns_empty_for_empty_input() {
    let client = test_model_client(SessionSource::Cli);
    let model_info = test_model_info();
    let session_telemetry = test_session_telemetry();

    let output = client
        .summarize_memories(Vec::new(), &model_info, None, &session_telemetry)
        .await
        .expect("empty summarize request should succeed");
    assert_eq!(output.len(), 0);
}

#[test]
fn prompt_gc_hidden_child_session_reuses_turn_state_without_request_cache() {
    let client = test_model_client(SessionSource::Cli);
    let parent = client.new_session();
    parent
        .turn_state
        .set("sticky-turn-state".to_string())
        .expect("set turn state");

    let child = parent.new_hidden_child_session();
    let headers = build_responses_headers(None, Some(&child.turn_state), None);

    assert_eq!(
        child.turn_state.get().map(String::as_str),
        Some("sticky-turn-state")
    );
    assert_eq!(
        headers
            .get(X_CODEX_TURN_STATE_HEADER)
            .and_then(|value| value.to_str().ok()),
        Some("sticky-turn-state")
    );
    assert!(child.websocket_session.connection.is_none());
    assert!(child.websocket_session.last_request.is_none());
    assert!(child.websocket_session.last_response_rx.is_none());
    assert!(!child.cache_websocket_session_on_drop);
    assert!(!child.allow_session_transport_fallback_mutation);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prompt_gc_hidden_child_session_stream_fallback_keeps_visible_websockets_enabled() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(".*/responses$"))
        .respond_with(ResponseTemplate::new(426))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path_regex(".*/responses$"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(concat!(
                    "event: response.created\n",
                    "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp-1\"}}\n\n",
                    "event: response.completed\n",
                    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp-1\"}}\n\n",
                )),
        )
        .mount(&server)
        .await;

    let client = ModelClient::new(
        None,
        ThreadId::new(),
        test_provider(&format!("{}/v1", server.uri())),
        SessionSource::Cli,
        None,
        true,
        false,
        None,
    );
    let parent = client.new_session();
    let mut child = parent.new_hidden_child_session();
    let model_info = test_model_info();
    let session_telemetry = test_session_telemetry();
    let prompt = Prompt {
        input: Vec::new(),
        tools: Vec::new(),
        parallel_tool_calls: false,
        base_instructions: BaseInstructions {
            text: "hidden prompt gc".to_string(),
        },
        personality: None,
        output_schema: None,
    };

    let _stream = child
        .stream(
            &prompt,
            &model_info,
            &session_telemetry,
            None,
            ReasoningSummaryConfig::None,
            None,
            None,
        )
        .await
        .expect("hidden fallback should continue over HTTP");

    assert!(
        client.responses_websocket_enabled(),
        "hidden prompt_gc fallback must not disable visible websocket transport"
    );

    let requests = server.received_requests().await.unwrap_or_default();
    let websocket_attempts = requests
        .iter()
        .filter(|request| {
            request.method.as_str() == "GET" && request.url.path().ends_with("/responses")
        })
        .count();
    let http_attempts = requests
        .iter()
        .filter(|request| {
            request.method.as_str() == "POST" && request.url.path().ends_with("/responses")
        })
        .count();
    assert_eq!(websocket_attempts, 1);
    assert_eq!(http_attempts, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subagent_websocket_reconnects_when_auth_account_changes_mid_session() {
    let server = start_websocket_server(vec![
        vec![
            vec![ev_response_created("resp-1"), ev_completed("resp-1")],
            vec![
                ev_response_created("resp-unused-if-connection-is-reused"),
                ev_completed("resp-unused-if-connection-is-reused"),
            ],
        ],
        vec![vec![ev_response_created("resp-2"), ev_completed("resp-2")]],
    ])
    .await;

    let auth_home = tempdir().expect("create auth tempdir");
    let auth_store = AuthStore {
        active_account_id: Some("acc-0".to_string()),
        accounts: vec![
            StoredAccount {
                id: "acc-0".to_string(),
                label: None,
                tokens: test_chatgpt_token_data("acc-0"),
                last_refresh: Some(Utc::now()),
                usage: None,
            },
            StoredAccount {
                id: "acc-1".to_string(),
                label: None,
                tokens: test_chatgpt_token_data("acc-1"),
                last_refresh: Some(Utc::now()),
                usage: None,
            },
        ],
        ..AuthStore::default()
    };
    save_auth(
        auth_home.path(),
        &auth_store,
        AuthCredentialsStoreMode::File,
    )
    .expect("persist auth store");
    let auth_manager = crate::AuthManager::shared(
        auth_home.path().to_path_buf(),
        false,
        AuthCredentialsStoreMode::File,
    );

    let client = ModelClient::new(
        Some(auth_manager.clone()),
        ThreadId::new(),
        test_provider(&format!("{}/v1", server.uri())),
        SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id: ThreadId::new(),
            depth: 1,
            agent_path: None,
            agent_nickname: None,
            agent_role: None,
        }),
        None,
        false,
        false,
        None,
    );
    let model_info = test_model_info();
    let session_telemetry = test_session_telemetry();
    let prompt = Prompt {
        input: Vec::new(),
        tools: Vec::new(),
        parallel_tool_calls: false,
        base_instructions: BaseInstructions {
            text: "auth-sensitive websocket reuse".to_string(),
        },
        personality: None,
        output_schema: None,
    };
    let mut session = client.new_session();

    let mut first_stream = session
        .stream(
            &prompt,
            &model_info,
            &session_telemetry,
            None,
            ReasoningSummaryConfig::None,
            None,
            None,
        )
        .await
        .expect("first websocket request should succeed");
    while let Some(event) = first_stream.next().await {
        if matches!(
            event.expect("first stream event"),
            ResponseEvent::Completed { .. }
        ) {
            break;
        }
    }

    let accounts_before = auth_manager.list_accounts();
    let failing_store_account_id = accounts_before
        .iter()
        .find(|account| account.is_active)
        .map(|account| account.id.clone())
        .expect("active account should be present");
    let freshly_unsupported_store_account_ids = HashSet::new();
    let switched_to = auth_manager
        .switch_account_on_usage_limit(
            None,
            Some(failing_store_account_id.as_str()),
            None,
            None,
            &freshly_unsupported_store_account_ids,
            None,
        )
        .expect("account switch should succeed");
    assert!(
        switched_to.is_some(),
        "account switch should select a fallback account"
    );

    let mut second_stream = session
        .stream(
            &prompt,
            &model_info,
            &session_telemetry,
            None,
            ReasoningSummaryConfig::None,
            None,
            None,
        )
        .await
        .expect("second websocket request should reconnect with the new account");
    while let Some(event) = second_stream.next().await {
        if matches!(
            event.expect("second stream event"),
            ResponseEvent::Completed { .. }
        ) {
            break;
        }
    }

    assert!(
        server
            .wait_for_handshakes(2, std::time::Duration::from_secs(2))
            .await,
        "expected a second websocket handshake after the account changed"
    );
    let handshakes = server.handshakes();
    assert_eq!(handshakes.len(), 2);
    assert_eq!(
        handshakes[0].header("chatgpt-account-id").as_deref(),
        Some("acc-0")
    );
    assert_eq!(
        handshakes[1].header("chatgpt-account-id").as_deref(),
        Some("acc-1")
    );
    assert_eq!(
        handshakes[0].header("authorization").as_deref(),
        Some("Bearer access-acc-0")
    );
    assert_eq!(
        handshakes[1].header("authorization").as_deref(),
        Some("Bearer access-acc-1")
    );

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subagent_preconnect_reconnects_when_auth_account_changes_mid_session() {
    let server = start_websocket_server(vec![Vec::new(), Vec::new()]).await;

    let auth_home = tempdir().expect("create auth tempdir");
    let auth_store = AuthStore {
        active_account_id: Some("acc-0".to_string()),
        accounts: vec![
            StoredAccount {
                id: "acc-0".to_string(),
                label: None,
                tokens: test_chatgpt_token_data("acc-0"),
                last_refresh: Some(Utc::now()),
                usage: None,
            },
            StoredAccount {
                id: "acc-1".to_string(),
                label: None,
                tokens: test_chatgpt_token_data("acc-1"),
                last_refresh: Some(Utc::now()),
                usage: None,
            },
        ],
        ..AuthStore::default()
    };
    save_auth(
        auth_home.path(),
        &auth_store,
        AuthCredentialsStoreMode::File,
    )
    .expect("persist auth store");
    let auth_manager = crate::AuthManager::shared(
        auth_home.path().to_path_buf(),
        false,
        AuthCredentialsStoreMode::File,
    );

    let client = ModelClient::new(
        Some(auth_manager.clone()),
        ThreadId::new(),
        test_provider(&format!("{}/v1", server.uri())),
        SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id: ThreadId::new(),
            depth: 1,
            agent_path: None,
            agent_nickname: None,
            agent_role: None,
        }),
        None,
        false,
        false,
        None,
    );
    let model_info = test_model_info();
    let session_telemetry = test_session_telemetry();
    let mut session = client.new_session();

    session
        .preconnect_websocket(&session_telemetry, &model_info)
        .await
        .expect("first websocket preconnect should succeed");

    let accounts_before = auth_manager.list_accounts();
    let failing_store_account_id = accounts_before
        .iter()
        .find(|account| account.is_active)
        .map(|account| account.id.clone())
        .expect("active account should be present");
    let freshly_unsupported_store_account_ids = HashSet::new();
    let switched_to = auth_manager
        .switch_account_on_usage_limit(
            None,
            Some(failing_store_account_id.as_str()),
            None,
            None,
            &freshly_unsupported_store_account_ids,
            None,
        )
        .expect("account switch should succeed");
    assert!(
        switched_to.is_some(),
        "account switch should select a fallback account"
    );

    session
        .preconnect_websocket(&session_telemetry, &model_info)
        .await
        .expect("second websocket preconnect should reconnect with the new account");

    assert!(
        server
            .wait_for_handshakes(2, std::time::Duration::from_secs(2))
            .await,
        "expected a second websocket handshake after the account changed"
    );
    let handshakes = server.handshakes();
    assert_eq!(handshakes.len(), 2);
    assert_eq!(
        handshakes[0].header("chatgpt-account-id").as_deref(),
        Some("acc-0")
    );
    assert_eq!(
        handshakes[1].header("chatgpt-account-id").as_deref(),
        Some("acc-1")
    );
    assert_eq!(
        handshakes[0].header("authorization").as_deref(),
        Some("Bearer access-acc-0")
    );
    assert_eq!(
        handshakes[1].header("authorization").as_deref(),
        Some("Bearer access-acc-1")
    );

    server.shutdown().await;
}

#[test]
fn auth_request_telemetry_context_tracks_attached_auth_and_retry_phase() {
    let auth_context = AuthRequestTelemetryContext::new(
        Some(crate::auth::AuthMode::Chatgpt),
        &crate::api_bridge::CoreAuthProvider::for_test(Some("access-token"), Some("workspace-123")),
        PendingUnauthorizedRetry::from_recovery(UnauthorizedRecoveryExecution {
            mode: "managed",
            phase: "refresh_token",
        }),
    );

    assert_eq!(auth_context.auth_mode, Some("Chatgpt"));
    assert!(auth_context.auth_header_attached);
    assert_eq!(auth_context.auth_header_name, Some("authorization"));
    assert!(auth_context.retry_after_unauthorized);
    assert_eq!(auth_context.recovery_mode, Some("managed"));
    assert_eq!(auth_context.recovery_phase, Some("refresh_token"));
}
