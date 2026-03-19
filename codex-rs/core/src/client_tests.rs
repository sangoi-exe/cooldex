use super::AuthRequestTelemetryContext;
use super::ModelClient;
use super::PendingUnauthorizedRetry;
use super::UnauthorizedRecoveryExecution;
use super::X_CODEX_TURN_STATE_HEADER;
use super::build_responses_headers;
use crate::client_common::Prompt;
use codex_otel::SessionTelemetry;
use codex_protocol::ThreadId;
use codex_protocol::config_types::ReasoningSummary as ReasoningSummaryConfig;
use codex_protocol::models::BaseInstructions;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use pretty_assertions::assert_eq;
use serde_json::json;
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
