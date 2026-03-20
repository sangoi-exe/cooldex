use super::*;
use crate::CodexAuth;
use crate::config::CONFIG_TOML_FILE;
use crate::config::ConfigBuilder;
use crate::config::test_config;
use crate::config_loader::ConfigLayerStack;
use crate::config_loader::ConfigLayerStackOrdering;
use crate::config_loader::NetworkConstraints;
use crate::config_loader::RequirementSource;
use crate::config_loader::Sourced;
use crate::exec::ExecToolCallOutput;
use crate::function_tool::FunctionCallError;
use crate::mcp_connection_manager::ToolInfo;
use crate::models_manager::model_info;
use crate::shell::default_user_shell;
use crate::tools::format_exec_output_str;

use codex_protocol::ThreadId;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_protocol::permissions::FileSystemSpecialPath;
use codex_protocol::protocol::ReadOnlyAccess;
use codex_protocol::protocol::SandboxPolicy;
use codex_protocol::request_permissions::PermissionGrantScope;
use codex_protocol::request_permissions::RequestPermissionProfile;
use tracing::Span;

use crate::protocol::CompactedItem;
use crate::protocol::CreditsSnapshot;
use crate::protocol::InitialHistory;
use crate::protocol::NetworkApprovalProtocol;
use crate::protocol::PromptGcCompactionMetadata;
use crate::protocol::PromptGcExecutionPhase;
use crate::protocol::PromptGcOutcomeKind;
use crate::protocol::RateLimitSnapshot;
use crate::protocol::RateLimitWindow;
use crate::protocol::ResumedHistory;
use crate::protocol::TokenCountEvent;
use crate::protocol::TokenUsage;
use crate::protocol::TokenUsageInfo;
use crate::protocol::TurnCompleteEvent;
use crate::protocol::TurnStartedEvent;
use crate::protocol::UserMessageEvent;
use crate::rollout::policy::EventPersistenceMode;
use crate::rollout::recorder::RolloutRecorder;
use crate::rollout::recorder::RolloutRecorderParams;
use crate::state::TaskKind;
use crate::tasks::RegularTask;
use crate::tasks::SessionTask;
use crate::tasks::SessionTaskContext;
use crate::tools::ToolRouter;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::handlers::ShellHandler;
use crate::tools::handlers::UnifiedExecHandler;
use crate::tools::registry::ToolHandler;
use crate::tools::router::ToolCallSource;
use crate::turn_diff_tracker::TurnDiffTracker;
use base64::Engine;
use chrono::TimeZone;
use chrono::Utc;
use codex_app_server_protocol::AppInfo;
use codex_execpolicy::Decision;
use codex_execpolicy::NetworkRuleProtocol;
use codex_execpolicy::Policy;
use codex_hooks::Hook;
use codex_hooks::HookResult;
use codex_hooks::Hooks;
use codex_network_proxy::NetworkProxyConfig;
use codex_otel::TelemetryAuthMode;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::ContentItem;
use codex_protocol::models::DeveloperInstructions;
use codex_protocol::models::LocalShellAction;
use codex_protocol::models::LocalShellExecAction;
use codex_protocol::models::LocalShellStatus;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ModelsResponse;
use codex_protocol::protocol::ConversationAudioParams;
use codex_protocol::protocol::RealtimeAudioFrame;
use codex_protocol::protocol::Submission;
use codex_protocol::protocol::W3cTraceContext;
use opentelemetry::trace::TraceContextExt;
use opentelemetry::trace::TraceId;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::trace::SdkTracerProvider;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::time::sleep;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::prelude::*;

use codex_protocol::mcp::CallToolResult as McpCallToolResult;
use pretty_assertions::assert_eq;
use rmcp::model::JsonObject;
use rmcp::model::Tool;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Once;
use std::time::Duration as StdDuration;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::Respond;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

#[path = "codex_tests_guardian.rs"]
mod guardian_tests;

use codex_protocol::models::function_call_output_content_items_to_text;

fn expect_text_tool_output(output: &FunctionToolOutput) -> String {
    function_call_output_content_items_to_text(&output.body).unwrap_or_default()
}

struct InstructionsTestCase {
    slug: &'static str,
    expects_apply_patch_instructions: bool,
}

fn user_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        end_turn: None,
        phase: None,
    }
}

fn assistant_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: text.to_string(),
        }],
        end_turn: None,
        phase: None,
    }
}

fn skill_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        end_turn: None,
        phase: None,
    }
}

#[tokio::test]
async fn regular_turn_emits_turn_started_without_waiting_for_startup_prewarm() {
    let (sess, tc, rx) = make_session_and_context_with_rx().await;
    let (_tx, startup_prewarm_rx) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        let _ = startup_prewarm_rx.await;
        Ok(test_model_client_session())
    });

    sess.set_session_startup_prewarm(
        crate::session_startup_prewarm::SessionStartupPrewarmHandle::new(
            handle,
            std::time::Instant::now(),
            crate::client::WEBSOCKET_CONNECT_TIMEOUT,
        ),
    )
    .await;
    sess.spawn_task(
        Arc::clone(&tc),
        Vec::new(),
        crate::tasks::RegularTask::new(),
    )
    .await;

    let first = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv())
        .await
        .expect("expected turn started event without waiting for startup prewarm")
        .expect("channel open");
    assert!(matches!(
        first.msg,
        EventMsg::TurnStarted(TurnStartedEvent { turn_id, .. }) if turn_id == tc.sub_id
    ));

    sess.abort_all_tasks(TurnAbortReason::Interrupted).await;
}

#[tokio::test]
async fn interrupting_regular_turn_waiting_on_startup_prewarm_emits_turn_aborted() {
    let (sess, tc, rx) = make_session_and_context_with_rx().await;
    let (_tx, startup_prewarm_rx) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        let _ = startup_prewarm_rx.await;
        Ok(test_model_client_session())
    });

    sess.set_session_startup_prewarm(
        crate::session_startup_prewarm::SessionStartupPrewarmHandle::new(
            handle,
            std::time::Instant::now(),
            crate::client::WEBSOCKET_CONNECT_TIMEOUT,
        ),
    )
    .await;
    sess.spawn_task(
        Arc::clone(&tc),
        Vec::new(),
        crate::tasks::RegularTask::new(),
    )
    .await;

    let first = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv())
        .await
        .expect("expected turn started event without waiting for startup prewarm")
        .expect("channel open");
    assert!(matches!(
        first.msg,
        EventMsg::TurnStarted(TurnStartedEvent { turn_id, .. }) if turn_id == tc.sub_id
    ));

    sess.abort_all_tasks(TurnAbortReason::Interrupted).await;

    let second = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("expected turn aborted event")
        .expect("channel open");
    assert!(matches!(
        second.msg,
        EventMsg::TurnAborted(crate::protocol::TurnAbortedEvent {
            turn_id: Some(turn_id),
            reason: TurnAbortReason::Interrupted,
        }) if turn_id == tc.sub_id
    ));
}

fn test_model_client_session() -> crate::client::ModelClientSession {
    crate::client::ModelClient::new(
        None,
        ThreadId::try_from("00000000-0000-4000-8000-000000000001")
            .expect("test thread id should be valid"),
        crate::model_provider_info::ModelProviderInfo::create_openai_provider(
            /* base_url */ None,
        ),
        codex_protocol::protocol::SessionSource::Exec,
        None,
        false,
        false,
        None,
    )
    .new_session()
}

fn developer_input_texts(items: &[ResponseItem]) -> Vec<&str> {
    items
        .iter()
        .filter_map(|item| match item {
            ResponseItem::Message { role, content, .. } if role == "developer" => {
                Some(content.as_slice())
            }
            _ => None,
        })
        .flat_map(|content| content.iter())
        .filter_map(|item| match item {
            ContentItem::InputText { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn test_tool_runtime(session: Arc<Session>, turn_context: Arc<TurnContext>) -> ToolCallRuntime {
    let router = Arc::new(ToolRouter::from_config(
        &turn_context.tools_config,
        crate::tools::router::ToolRouterParams {
            mcp_tools: None,
            app_tools: None,
            discoverable_tools: None,
            dynamic_tools: turn_context.dynamic_tools.as_slice(),
        },
    ));
    let tracker = Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new()));
    ToolCallRuntime::new(
        router,
        session,
        turn_context,
        tracker,
        None,
        ToolCallSource::Direct,
    )
}

fn make_connector(id: &str, name: &str) -> AppInfo {
    AppInfo {
        id: id.to_string(),
        name: name.to_string(),
        description: None,
        logo_url: None,
        logo_url_dark: None,
        distribution_channel: None,
        branding: None,
        app_metadata: None,
        labels: None,
        install_url: None,
        is_accessible: true,
        is_enabled: true,
        plugin_display_names: Vec::new(),
    }
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
fn assistant_message_stream_parsers_can_be_seeded_from_output_item_added_text() {
    let mut parsers = AssistantMessageStreamParsers::new(false);
    let item_id = "msg-1";

    let seeded = parsers.seed_item_text(item_id, "hello <oai-mem-citation>doc");
    let parsed = parsers.parse_delta(item_id, "1</oai-mem-citation> world");
    let tail = parsers.finish_item(item_id);

    assert_eq!(seeded.visible_text, "hello ");
    assert_eq!(seeded.citations, Vec::<String>::new());
    assert_eq!(parsed.visible_text, " world");
    assert_eq!(parsed.citations, vec!["doc1".to_string()]);
    assert_eq!(tail.visible_text, "");
    assert_eq!(tail.citations, Vec::<String>::new());
}

#[test]
fn assistant_message_stream_parsers_seed_buffered_prefix_stays_out_of_finish_tail() {
    let mut parsers = AssistantMessageStreamParsers::new(false);
    let item_id = "msg-1";

    let seeded = parsers.seed_item_text(item_id, "hello <oai-mem-");
    let parsed = parsers.parse_delta(item_id, "citation>doc</oai-mem-citation> world");
    let tail = parsers.finish_item(item_id);

    assert_eq!(seeded.visible_text, "hello ");
    assert_eq!(seeded.citations, Vec::<String>::new());
    assert_eq!(parsed.visible_text, " world");
    assert_eq!(parsed.citations, vec!["doc".to_string()]);
    assert_eq!(tail.visible_text, "");
    assert_eq!(tail.citations, Vec::<String>::new());
}

#[test]
fn assistant_message_stream_parsers_seed_plan_parser_across_added_and_delta_boundaries() {
    let mut parsers = AssistantMessageStreamParsers::new(true);
    let item_id = "msg-1";

    let seeded = parsers.seed_item_text(item_id, "Intro\n<proposed");
    let parsed = parsers.parse_delta(item_id, "_plan>\n- step\n</proposed_plan>\nOutro");
    let tail = parsers.finish_item(item_id);

    assert_eq!(seeded.visible_text, "Intro\n");
    assert_eq!(
        seeded.plan_segments,
        vec![ProposedPlanSegment::Normal("Intro\n".to_string())]
    );
    assert_eq!(parsed.visible_text, "Outro");
    assert_eq!(
        parsed.plan_segments,
        vec![
            ProposedPlanSegment::ProposedPlanStart,
            ProposedPlanSegment::ProposedPlanDelta("- step\n".to_string()),
            ProposedPlanSegment::ProposedPlanEnd,
            ProposedPlanSegment::Normal("Outro".to_string()),
        ]
    );
    assert_eq!(tail.visible_text, "");
    assert!(tail.plan_segments.is_empty());
}

#[test]
fn prompt_gc_capability_is_explicitly_opt_in() {
    assert!(RegularTask.supports_prompt_gc());
    assert!(!crate::tasks::UserShellCommandTask::new("echo hi".to_string()).supports_prompt_gc());
    assert!(!crate::tasks::UndoTask::new().supports_prompt_gc());
    assert!(
        !NeverEndingTask {
            kind: TaskKind::Regular,
            listen_to_cancellation_token: false,
        }
        .supports_prompt_gc()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prompt_gc_sidecar_no_eligible_chunks_complete_without_visible_accounting() {
    let server = MockServer::start().await;
    let (mut session, turn_context, rx) = make_session_and_context_with_rx().await;
    let rollout_path = attach_rollout_recorder(&session).await;
    let checkpoint_id = format!("{}:prompt_gc:0", turn_context.sub_id);

    configure_session_model_client_for_server(&mut session, &turn_context, &server);

    let sidecar = install_prompt_gc_active_turn(&session, &turn_context).await;
    let phase_message = ResponseItem::Message {
        id: Some("phase-1".to_string()),
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: "checkpoint".to_string(),
        }],
        end_turn: None,
        phase: Some(codex_protocol::models::MessagePhase::Commentary),
    };
    let observed_items = session
        .record_into_history(std::slice::from_ref(&phase_message), &turn_context)
        .await;
    sidecar.lock().await.observe_recorded_batch(&observed_items);
    let parent_client_session = session.services.model_client.new_session();

    run_prompt_gc_sidecar_if_needed(
        &session,
        &turn_context,
        Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new())),
        &parent_client_session,
        CancellationToken::new(),
    )
    .await;

    let status = sidecar.lock().await.status.clone();
    assert_eq!(status.last_applied_checkpoint_seq, Some(0));
    assert_eq!(status.last_error, None);
    session.flush_rollout().await;
    assert_eq!(
        prompt_gc_rollout_markers(&rollout_path).await,
        vec![
            PromptGcCompactionMetadata {
                checkpoint_id: checkpoint_id.clone(),
                checkpoint_seq: 0,
                kind: PromptGcOutcomeKind::Started,
                phase: Some(PromptGcExecutionPhase::Prepare),
                stop_reason: None,
                error_message: None,
                applied_unit_count: None,
            },
            PromptGcCompactionMetadata {
                checkpoint_id,
                checkpoint_seq: 0,
                kind: PromptGcOutcomeKind::NoEligibleChunks,
                phase: Some(PromptGcExecutionPhase::Prepare),
                stop_reason: Some("no_eligible_chunks".to_string()),
                error_message: None,
                applied_unit_count: Some(0),
            },
        ]
    );

    let tool_calls = {
        let active = session.active_turn.lock().await;
        let active_turn = active.as_ref().expect("active turn");
        active_turn.turn_state.lock().await.tool_calls
    };
    assert_eq!(
        tool_calls, 0,
        "hidden prompt_gc should not increment visible tool accounting"
    );
    assert!(rx.try_recv().is_err());
    let latest_rate_limits = session.state.lock().await.latest_rate_limits.clone();
    assert_eq!(latest_rate_limits, None);
}

fn prompt_gc_unified_exec_output(token_qty: usize, body: &str) -> String {
    format!("Wall time: 0.0000 seconds\nToken qty: {token_qty}\nOutput:\n{body}")
}

#[test]
fn prompt_gc_plan_build_failure_details_falls_back_to_plan_build_failed_for_unstructured_errors() {
    let details = prompt_gc_plan_build_failure_details(&FunctionCallError::Fatal(
        "plain failure".to_string(),
    ));

    assert_eq!(details.marker_stop_reason, "plan_build_failed");
    assert_eq!(details.error_message, "Fatal error: plain failure");
    assert_eq!(details.status_error, "Fatal error: plain failure");
    assert!(!details.blocks_remaining_turn);
}

#[test]
fn prompt_gc_plan_build_failure_details_preserves_structured_state_hash_mismatch() {
    let error = FunctionCallError::RespondToModel(
        json!({
            "mode": "error",
            "stop_reason": "state_hash_mismatch",
            "message": "tool call drifted before apply",
        })
        .to_string(),
    );

    let details = prompt_gc_plan_build_failure_details(&error);

    assert_eq!(details.marker_stop_reason, "state_hash_mismatch");
    assert_eq!(details.error_message, error.to_string());
    assert_eq!(details.status_error, "tool call drifted before apply");
    assert!(details.blocks_remaining_turn);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prompt_gc_sidecar_skips_when_function_call_output_lacks_token_qty_without_rollout_markers()
{
    let (session, mut turn_context, rx) = make_session_and_context_with_rx().await;
    let rollout_path = attach_rollout_recorder(&session).await;
    let turn_context_mut =
        Arc::get_mut(&mut turn_context).expect("turn_context arc should be unique");
    turn_context_mut.model_info.context_window = Some(100_000);
    turn_context_mut.model_info.effective_context_window_percent = 100;
    {
        let mut state = session.state.lock().await;
        state.set_token_info(Some(TokenUsageInfo {
            total_token_usage: TokenUsage {
                total_tokens: 90_000,
                ..TokenUsage::default()
            },
            last_token_usage: TokenUsage {
                total_tokens: 90_000,
                ..TokenUsage::default()
            },
            model_context_window: Some(100_000),
        }));
    }

    let sidecar = install_prompt_gc_active_turn(&session, &turn_context).await;
    let tool_call = ResponseItem::FunctionCall {
        id: None,
        call_id: "call-1".to_string(),
        name: "exec_command".to_string(),
        namespace: None,
        arguments: "{\"cmd\":\"pwd\"}".to_string(),
    };
    let tool_output = ResponseItem::FunctionCallOutput {
        call_id: "call-1".to_string(),
        output: FunctionCallOutputPayload::from_text("x".repeat(900)),
    };
    let phase_message = prompt_gc_phase_message("phase-1");
    let observed_items = session
        .record_into_history(&[tool_call, tool_output, phase_message], &turn_context)
        .await;
    sidecar.lock().await.observe_recorded_batch(&observed_items);
    let parent_client_session = session.services.model_client.new_session();

    run_prompt_gc_sidecar_if_needed(
        &session,
        &turn_context,
        Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new())),
        &parent_client_session,
        CancellationToken::new(),
    )
    .await;

    let checkpoint_id = format!("{}:prompt_gc:0", turn_context.sub_id);
    let status = sidecar.lock().await.status.clone();
    assert_eq!(status.last_applied_checkpoint_seq, None);
    assert_eq!(status.last_error, None);
    assert_eq!(status.blocked_reason, None);
    assert!(sidecar.lock().await.checkpoint(&checkpoint_id).is_none());
    session.flush_rollout().await;
    assert_eq!(prompt_gc_rollout_markers(&rollout_path).await, Vec::new());

    let tool_calls = {
        let active = session.active_turn.lock().await;
        let active_turn = active.as_ref().expect("active turn");
        active_turn.turn_state.lock().await.tool_calls
    };
    assert_eq!(
        tool_calls, 0,
        "skipped prompt_gc should not increment visible tool accounting"
    );
    assert!(rx.try_recv().is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prompt_gc_sidecar_function_call_output_token_qty_over_200_triggers_below_global_context_pressure()
 {
    let server = MockServer::start().await;
    let (mut session, mut turn_context, rx) = make_session_and_context_with_rx().await;
    let rollout_path = attach_rollout_recorder(&session).await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(StaticSseResponder {
            calls: AtomicUsize::new(0),
            response_body: sse(&[
                response_created("resp-1"),
                assistant_message_event(
                    "msg-1",
                    "{\"summaries\":[{\"chunk_id\":\"prompt_gc_chunk_0\",\"tool_context\":\"tool\",\"reasoning_context\":\"reasoning\"}]}",
                ),
                response_completed_with_usage("resp-1", 17),
            ]),
            headers: Vec::new(),
        })
        .expect(1)
        .mount(&server)
        .await;
    let turn_context_mut =
        Arc::get_mut(&mut turn_context).expect("turn_context arc should be unique");
    turn_context_mut.model_info.context_window = Some(100_000);
    turn_context_mut.model_info.effective_context_window_percent = 100;
    {
        let mut state = session.state.lock().await;
        state.set_token_info(Some(TokenUsageInfo {
            total_token_usage: TokenUsage {
                total_tokens: 12_000,
                ..TokenUsage::default()
            },
            last_token_usage: TokenUsage {
                total_tokens: 12_000,
                ..TokenUsage::default()
            },
            model_context_window: Some(100_000),
        }));
    }

    configure_session_model_client_for_server(&mut session, &turn_context, &server);

    let sidecar = install_prompt_gc_active_turn(&session, &turn_context).await;
    let items = vec![
        ResponseItem::FunctionCall {
            id: None,
            call_id: "call-1".to_string(),
            name: "exec_command".to_string(),
            namespace: None,
            arguments: "{\"cmd\":\"pwd\"}".to_string(),
        },
        ResponseItem::FunctionCallOutput {
            call_id: "call-1".to_string(),
            output: FunctionCallOutputPayload::from_text(prompt_gc_unified_exec_output(2798, "ok")),
        },
        ResponseItem::Message {
            id: Some("phase-1".to_string()),
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: "checkpoint".to_string(),
            }],
            end_turn: None,
            phase: Some(codex_protocol::models::MessagePhase::Commentary),
        },
    ];
    let observed_items = session.record_into_history(&items, &turn_context).await;
    sidecar.lock().await.observe_recorded_batch(&observed_items);
    let parent_client_session = session.services.model_client.new_session();

    run_prompt_gc_sidecar_if_needed(
        &session,
        &turn_context,
        Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new())),
        &parent_client_session,
        CancellationToken::new(),
    )
    .await;

    let status = sidecar.lock().await.status.clone();
    assert_eq!(status.last_applied_checkpoint_seq, Some(0));
    assert_eq!(status.last_error, None);
    assert_eq!(status.blocked_reason, None);
    session.flush_rollout().await;
    let markers = prompt_gc_rollout_markers(&rollout_path).await;
    assert!(!markers.is_empty());
    assert_eq!(markers[0].kind, PromptGcOutcomeKind::Started);
    assert!(rx.try_recv().is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prompt_gc_sidecar_function_call_output_token_qty_over_200_triggers_for_non_exec_function_call()
 {
    let server = MockServer::start().await;
    let (mut session, mut turn_context, rx) = make_session_and_context_with_rx().await;
    let rollout_path = attach_rollout_recorder(&session).await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(StaticSseResponder {
            calls: AtomicUsize::new(0),
            response_body: sse(&[
                response_created("resp-1"),
                assistant_message_event(
                    "msg-1",
                    "{\"summaries\":[{\"chunk_id\":\"prompt_gc_chunk_0\",\"tool_context\":\"tool\",\"reasoning_context\":\"reasoning\"}]}",
                ),
                response_completed_with_usage("resp-1", 17),
            ]),
            headers: Vec::new(),
        })
        .expect(1)
        .mount(&server)
        .await;
    let turn_context_mut =
        Arc::get_mut(&mut turn_context).expect("turn_context arc should be unique");
    turn_context_mut.model_info.context_window = Some(100_000);
    turn_context_mut.model_info.effective_context_window_percent = 100;
    {
        let mut state = session.state.lock().await;
        state.set_token_info(Some(TokenUsageInfo {
            total_token_usage: TokenUsage {
                total_tokens: 12_000,
                ..TokenUsage::default()
            },
            last_token_usage: TokenUsage {
                total_tokens: 12_000,
                ..TokenUsage::default()
            },
            model_context_window: Some(100_000),
        }));
    }

    configure_session_model_client_for_server(&mut session, &turn_context, &server);

    let sidecar = install_prompt_gc_active_turn(&session, &turn_context).await;
    let items = vec![
        ResponseItem::FunctionCall {
            id: None,
            call_id: "call-1".to_string(),
            name: "other_tool".to_string(),
            namespace: None,
            arguments: "{\"cmd\":\"pwd\"}".to_string(),
        },
        ResponseItem::FunctionCallOutput {
            call_id: "call-1".to_string(),
            output: FunctionCallOutputPayload::from_text(prompt_gc_unified_exec_output(2798, "ok")),
        },
        ResponseItem::Message {
            id: Some("phase-1".to_string()),
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: "checkpoint".to_string(),
            }],
            end_turn: None,
            phase: Some(codex_protocol::models::MessagePhase::Commentary),
        },
    ];
    let observed_items = session.record_into_history(&items, &turn_context).await;
    sidecar.lock().await.observe_recorded_batch(&observed_items);
    let parent_client_session = session.services.model_client.new_session();

    run_prompt_gc_sidecar_if_needed(
        &session,
        &turn_context,
        Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new())),
        &parent_client_session,
        CancellationToken::new(),
    )
    .await;

    let status = sidecar.lock().await.status.clone();
    assert_eq!(status.last_applied_checkpoint_seq, Some(0));
    assert_eq!(status.last_error, None);
    assert_eq!(status.blocked_reason, None);
    session.flush_rollout().await;
    let markers = prompt_gc_rollout_markers(&rollout_path).await;
    assert!(!markers.is_empty());
    assert_eq!(markers[0].kind, PromptGcOutcomeKind::Started);

    let tool_calls = {
        let active = session.active_turn.lock().await;
        let active_turn = active.as_ref().expect("active turn");
        active_turn.turn_state.lock().await.tool_calls
    };
    assert_eq!(tool_calls, 0);
    assert!(rx.try_recv().is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prompt_gc_sidecar_ignores_ambiguous_exec_command_local_shell_collision_without_rollout_markers()
 {
    let (session, mut turn_context, rx) = make_session_and_context_with_rx().await;
    let rollout_path = attach_rollout_recorder(&session).await;
    let turn_context_mut =
        Arc::get_mut(&mut turn_context).expect("turn_context arc should be unique");
    turn_context_mut.model_info.context_window = Some(100_000);
    turn_context_mut.model_info.effective_context_window_percent = 100;
    {
        let mut state = session.state.lock().await;
        state.set_token_info(Some(TokenUsageInfo {
            total_token_usage: TokenUsage {
                total_tokens: 12_000,
                ..TokenUsage::default()
            },
            last_token_usage: TokenUsage {
                total_tokens: 12_000,
                ..TokenUsage::default()
            },
            model_context_window: Some(100_000),
        }));
    }

    let sidecar = install_prompt_gc_active_turn(&session, &turn_context).await;
    let items = vec![
        ResponseItem::FunctionCall {
            id: None,
            call_id: "shared".to_string(),
            name: "exec_command".to_string(),
            namespace: None,
            arguments: "{\"cmd\":\"pwd\"}".to_string(),
        },
        ResponseItem::LocalShellCall {
            id: None,
            call_id: Some("shared".to_string()),
            status: LocalShellStatus::Completed,
            action: LocalShellAction::Exec(LocalShellExecAction {
                command: vec!["pwd".to_string()],
                working_directory: None,
                timeout_ms: None,
                env: None,
                user: None,
            }),
        },
        ResponseItem::FunctionCallOutput {
            call_id: "shared".to_string(),
            output: FunctionCallOutputPayload::from_text(prompt_gc_unified_exec_output(2798, "ok")),
        },
        prompt_gc_phase_message("phase-1"),
    ];
    let observed_items = session.record_into_history(&items, &turn_context).await;
    sidecar.lock().await.observe_recorded_batch(&observed_items);
    let parent_client_session = session.services.model_client.new_session();

    run_prompt_gc_sidecar_if_needed(
        &session,
        &turn_context,
        Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new())),
        &parent_client_session,
        CancellationToken::new(),
    )
    .await;

    let checkpoint_id = format!("{}:prompt_gc:0", turn_context.sub_id);
    let status = sidecar.lock().await.status.clone();
    assert_eq!(status.last_applied_checkpoint_seq, Some(0));
    assert_eq!(status.last_error, None);
    assert_eq!(status.blocked_reason, None);
    assert!(sidecar.lock().await.checkpoint(&checkpoint_id).is_none());
    session.flush_rollout().await;
    assert_eq!(
        prompt_gc_rollout_markers(&rollout_path).await,
        vec![
            PromptGcCompactionMetadata {
                checkpoint_id: checkpoint_id.clone(),
                checkpoint_seq: 0,
                kind: PromptGcOutcomeKind::Started,
                phase: Some(PromptGcExecutionPhase::Prepare),
                stop_reason: None,
                error_message: None,
                applied_unit_count: None,
            },
            PromptGcCompactionMetadata {
                checkpoint_id,
                checkpoint_seq: 0,
                kind: PromptGcOutcomeKind::NoEligibleChunks,
                phase: Some(PromptGcExecutionPhase::Prepare),
                stop_reason: Some("no_eligible_chunks".to_string()),
                error_message: None,
                applied_unit_count: Some(0),
            },
        ]
    );

    let tool_calls = {
        let active = session.active_turn.lock().await;
        let active_turn = active.as_ref().expect("active turn");
        active_turn.turn_state.lock().await.tool_calls
    };
    assert_eq!(tool_calls, 0);
    assert!(rx.try_recv().is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prompt_gc_state_hash_mismatch_blocks_remaining_turn() {
    let (session, mut turn_context, rx) = make_session_and_context_with_rx().await;
    let rollout_path = attach_rollout_recorder(&session).await;
    let turn_context_mut =
        Arc::get_mut(&mut turn_context).expect("turn_context arc should be unique");
    turn_context_mut.model_info.context_window = Some(100_000);
    turn_context_mut.model_info.effective_context_window_percent = 100;
    {
        let mut state = session.state.lock().await;
        state.set_token_info(Some(TokenUsageInfo {
            total_token_usage: TokenUsage {
                total_tokens: 90_000,
                ..TokenUsage::default()
            },
            last_token_usage: TokenUsage {
                total_tokens: 90_000,
                ..TokenUsage::default()
            },
            model_context_window: Some(100_000),
        }));
    }

    let sidecar = install_prompt_gc_active_turn(&session, &turn_context).await;
    let tool_call = ResponseItem::FunctionCall {
        id: None,
        call_id: "call-1".to_string(),
        name: "exec_command".to_string(),
        namespace: None,
        arguments: "{\"cmd\":\"pwd\"}".to_string(),
    };
    let tool_output = ResponseItem::FunctionCallOutput {
        call_id: "call-1".to_string(),
        output: FunctionCallOutputPayload::from_text(prompt_gc_unified_exec_output(2798, "ok")),
    };
    let phase_one = ResponseItem::Message {
        id: Some("phase-1".to_string()),
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: "checkpoint".to_string(),
        }],
        end_turn: None,
        phase: Some(codex_protocol::models::MessagePhase::Commentary),
    };
    let observed_items = session
        .record_into_history(&[tool_call, tool_output, phase_one.clone()], &turn_context)
        .await;
    sidecar.lock().await.observe_recorded_batch(&observed_items);
    session
        .replace_history(
            vec![phase_one.clone()],
            Some(turn_context.to_turn_context_item()),
        )
        .await;
    let parent_client_session = session.services.model_client.new_session();

    run_prompt_gc_sidecar_if_needed(
        &session,
        &turn_context,
        Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new())),
        &parent_client_session,
        CancellationToken::new(),
    )
    .await;

    let status = sidecar.lock().await.status.clone();
    assert_eq!(status.last_applied_checkpoint_seq, None);
    assert!(status.blocked_reason.is_some(), "{status:?}");
    session.flush_rollout().await;
    let first_markers = prompt_gc_rollout_markers(&rollout_path).await;
    assert_eq!(first_markers.len(), 2);
    assert_eq!(first_markers[0].kind, PromptGcOutcomeKind::Started);
    assert_eq!(first_markers[1].kind, PromptGcOutcomeKind::Failed);
    assert_eq!(
        first_markers[1].phase,
        Some(PromptGcExecutionPhase::Prepare)
    );
    assert_eq!(
        first_markers[1].stop_reason.as_deref(),
        Some("state_hash_mismatch")
    );
    assert!(
        first_markers[1]
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("state_hash_mismatch"))
    );

    let phase_two = ResponseItem::Message {
        id: Some("phase-2".to_string()),
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: "later checkpoint".to_string(),
        }],
        end_turn: None,
        phase: Some(codex_protocol::models::MessagePhase::Commentary),
    };
    let later_items = session
        .record_into_history(std::slice::from_ref(&phase_two), &turn_context)
        .await;
    sidecar.lock().await.observe_recorded_batch(&later_items);

    run_prompt_gc_sidecar_if_needed(
        &session,
        &turn_context,
        Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new())),
        &parent_client_session,
        CancellationToken::new(),
    )
    .await;

    session.flush_rollout().await;
    assert_eq!(
        prompt_gc_rollout_markers(&rollout_path).await,
        first_markers
    );
    assert!(rx.try_recv().is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prompt_gc_sidecar_skips_after_tool_use_hooks() {
    let server = MockServer::start().await;
    let (mut session, turn_context, rx) = make_session_and_context_with_rx().await;
    let hook_calls = Arc::new(AtomicUsize::new(0));
    let hook_calls_for_hook = Arc::clone(&hook_calls);
    let session_mut = Arc::get_mut(&mut session).expect("session arc should be unique");
    session_mut.services.hooks = Hooks::from_hooks(
        Vec::new(),
        vec![Hook {
            name: "abort-sidecar".to_string(),
            func: Arc::new(move |_| {
                let hook_calls = Arc::clone(&hook_calls_for_hook);
                Box::pin(async move {
                    hook_calls.fetch_add(1, Ordering::SeqCst);
                    HookResult::FailedAbort(
                        std::io::Error::other("sidecar hook should not run").into(),
                    )
                })
            }),
        }],
    );

    configure_session_model_client_for_server(&mut session, &turn_context, &server);

    let sidecar = install_prompt_gc_active_turn(&session, &turn_context).await;
    let phase_message = ResponseItem::Message {
        id: Some("phase-1".to_string()),
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: "checkpoint".to_string(),
        }],
        end_turn: None,
        phase: Some(codex_protocol::models::MessagePhase::Commentary),
    };
    let observed_items = session
        .record_into_history(std::slice::from_ref(&phase_message), &turn_context)
        .await;
    sidecar.lock().await.observe_recorded_batch(&observed_items);
    let parent_client_session = session.services.model_client.new_session();

    run_prompt_gc_sidecar_if_needed(
        &session,
        &turn_context,
        Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new())),
        &parent_client_session,
        CancellationToken::new(),
    )
    .await;

    let status = sidecar.lock().await.status.clone();
    assert_eq!(status.last_applied_checkpoint_seq, Some(0));
    assert_eq!(status.last_error, None);
    assert_eq!(
        hook_calls.load(Ordering::SeqCst),
        0,
        "hidden prompt_gc must not dispatch after_tool_use hooks"
    );
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn prompt_gc_persist_replacement_history_flush_failure_keeps_live_history_unchanged() {
    let (session, turn_context) = make_session_and_context().await;
    let checkpoint = crate::prompt_gc_sidecar::PromptGcCheckpoint {
        checkpoint_id: format!("{}:prompt_gc:7", turn_context.sub_id),
        checkpoint_seq: 7,
        eligible_unit_count: 0,
        phase: codex_protocol::models::MessagePhase::Commentary,
        assistant_item_id: None,
    };
    let history_before = {
        let mut state = session.state.lock().await;
        state.record_items(
            [user_message("before"), assistant_message("still here")].iter(),
            turn_context.truncation_policy,
        );
        state.history_snapshot_lenient()
    };

    let error = session
        .persist_prompt_gc_replacement_history_with_sink(
            &turn_context,
            &checkpoint,
            1,
            vec![assistant_message("replacement history")],
            Some(&FlushFailingPromptGcRolloutSink),
        )
        .await
        .expect_err("flush failure should fail prompt_gc persistence");
    assert!(
        error.contains("failed to persist prompt_gc replacement_history atomically"),
        "unexpected error: {error}"
    );

    let history_after = {
        let state = session.state.lock().await;
        state.history_snapshot_lenient()
    };
    assert_eq!(history_after, history_before);
}

#[tokio::test]
async fn prompt_gc_persist_replacement_history_records_apply_succeeded_marker() {
    let (session, turn_context, _rx) = make_session_and_context_with_rx().await;
    let rollout_path = attach_rollout_recorder(&session).await;
    let checkpoint = crate::prompt_gc_sidecar::PromptGcCheckpoint {
        checkpoint_id: format!("{}:prompt_gc:3", turn_context.sub_id),
        checkpoint_seq: 3,
        eligible_unit_count: 0,
        phase: codex_protocol::models::MessagePhase::Commentary,
        assistant_item_id: None,
    };
    let replacement_history = vec![assistant_message("replacement history")];

    session
        .persist_prompt_gc_replacement_history(
            &turn_context,
            &checkpoint,
            2,
            replacement_history.clone(),
        )
        .await
        .expect("persist prompt_gc replacement history");
    session.flush_rollout().await;

    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let prompt_gc_items = resumed
        .history
        .into_iter()
        .filter_map(|item| match item {
            RolloutItem::Compacted(compacted)
                if compacted.prompt_gc.is_some()
                    || compacted.message
                        == crate::prompt_gc_sidecar::PROMPT_GC_COMPACTION_MARKER =>
            {
                Some(compacted)
            }
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(prompt_gc_items.len(), 1);
    assert_eq!(
        prompt_gc_items[0].replacement_history,
        Some(replacement_history)
    );
    assert_eq!(
        prompt_gc_items[0].prompt_gc,
        Some(PromptGcCompactionMetadata {
            checkpoint_id: checkpoint.checkpoint_id,
            checkpoint_seq: checkpoint.checkpoint_seq,
            kind: PromptGcOutcomeKind::ApplySucceeded,
            phase: Some(PromptGcExecutionPhase::Persist),
            stop_reason: Some("target_reached".to_string()),
            error_message: None,
            applied_unit_count: Some(2),
        })
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prompt_gc_hidden_usage_limit_updates_rate_limits_without_visible_events() {
    let (session, turn_context, rx) = make_session_and_context_with_rx().await;
    let rate_limits = RateLimitSnapshot {
        limit_id: Some("codex".to_string()),
        limit_name: None,
        primary: Some(RateLimitWindow {
            used_percent: 87.5,
            window_minutes: Some(15),
            resets_at: Some(1_700_000_123),
        }),
        secondary: None,
        credits: None,
        plan_type: None,
    };
    let usage_limit = crate::error::UsageLimitReachedError {
        plan_type: None,
        resets_at: Some(Utc.with_ymd_and_hms(2024, 1, 1, 0, 15, 0).unwrap()),
        rate_limits: Some(Box::new(rate_limits.clone())),
        promo_message: None,
    };

    let should_retry = handle_usage_limit_for_execution_mode(
        &session,
        &turn_context,
        &usage_limit,
        None,
        SamplingExecutionMode::Hidden,
        UsageLimitHandlingPolicy::HiddenSilentAutoSwitch,
    )
    .await
    .expect("hidden usage-limit handling should succeed");

    assert!(
        !should_retry,
        "hidden prompt_gc should not retry when there is no eligible fallback account"
    );
    assert!(
        rx.try_recv().is_err(),
        "hidden usage-limit refresh should not emit visible events"
    );
    let latest_rate_limits = session.state.lock().await.latest_rate_limits.clone();
    assert_eq!(latest_rate_limits, Some(rate_limits));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn visible_usage_limit_retry_preserves_changed_active_account_until_its_own_turn() {
    let (mut session, mut turn_context, rx) = make_session_and_context_with_rx().await;
    let auth_home = tempfile::tempdir().expect("create auth tempdir");
    let auth_store = crate::auth::AuthStore {
        active_account_id: Some("acc-2".to_string()),
        accounts: vec![
            crate::auth::StoredAccount {
                id: "acc-0".to_string(),
                label: None,
                tokens: test_chatgpt_token_data("acc-0"),
                last_refresh: Some(Utc::now()),
                usage: None,
            },
            crate::auth::StoredAccount {
                id: "acc-1".to_string(),
                label: None,
                tokens: test_chatgpt_token_data("acc-1"),
                last_refresh: Some(Utc::now()),
                usage: None,
            },
            crate::auth::StoredAccount {
                id: "acc-2".to_string(),
                label: None,
                tokens: test_chatgpt_token_data("acc-2"),
                last_refresh: Some(Utc::now()),
                usage: None,
            },
        ],
        ..crate::auth::AuthStore::default()
    };
    crate::auth::save_auth(
        auth_home.path(),
        &auth_store,
        crate::auth::AuthCredentialsStoreMode::File,
    )
    .expect("persist auth store");
    let auth_manager = crate::AuthManager::shared(
        auth_home.path().to_path_buf(),
        false,
        crate::auth::AuthCredentialsStoreMode::File,
    );
    let accounts = auth_manager.list_accounts();
    assert_eq!(
        accounts.len(),
        3,
        "the auth store must stay intact before stale-request retry handling runs"
    );
    assert_eq!(
        accounts
            .iter()
            .find(|account| account.is_active)
            .map(|account| account.id.as_str()),
        Some("acc-2")
    );
    assert!(
        accounts.iter().any(|account| account.id == "acc-1"),
        "the failing account must still exist before the stale-request retry call"
    );

    let session_mut = Arc::get_mut(&mut session).expect("session arc should be unique");
    session_mut.services.auth_manager = Arc::clone(&auth_manager);
    let turn_context_mut =
        Arc::get_mut(&mut turn_context).expect("turn_context arc should be unique");
    turn_context_mut.auth_manager = Some(Arc::clone(&auth_manager));

    let usage_limit = crate::error::UsageLimitReachedError {
        plan_type: None,
        resets_at: Some(Utc.with_ymd_and_hms(2024, 1, 1, 0, 15, 0).unwrap()),
        rate_limits: Some(Box::new(RateLimitSnapshot {
            limit_id: Some("codex".to_string()),
            limit_name: None,
            primary: Some(RateLimitWindow {
                used_percent: 100.0,
                window_minutes: Some(15),
                resets_at: Some(1_700_000_123),
            }),
            secondary: None,
            credits: None,
            plan_type: None,
        })),
        promo_message: None,
    };
    let freshly_unsupported_store_account_ids =
        std::collections::HashSet::from([String::from("acc-2")]);

    let should_retry = maybe_auto_switch_account_on_usage_limit_with_freshly_unsupported_ids(
        &session,
        &turn_context,
        &usage_limit,
        Some("acc-1"),
        &freshly_unsupported_store_account_ids,
        UsageLimitHandlingPolicy::VisibleWarnAndAutoSwitch,
    )
    .await
    .expect("stale request should still retry against the already-active account");

    assert!(
        should_retry,
        "changed active account should stay retryable until its own usage-limit path runs"
    );
    let accounts = auth_manager.list_accounts();
    assert_eq!(
        accounts
            .iter()
            .find(|account| account.is_active)
            .map(|account| account.id.as_str()),
        Some("acc-2")
    );
    assert!(
        accounts.iter().any(|account| account.id == "acc-2"),
        "the already-active account must not be pruned during stale-request recovery"
    );
    assert!(
        rx.try_recv().is_err(),
        "stale-request retry should not emit a duplicate warning event"
    );
}

#[test]
fn auto_switch_refresh_does_not_mark_missing_plan_as_unsupported() {
    let snapshot = RateLimitSnapshot {
        limit_id: Some("codex".to_string()),
        limit_name: None,
        primary: None,
        secondary: None,
        credits: None,
        plan_type: None,
    };

    assert!(
        !auto_switch_refresh_marks_account_unsupported(&snapshot),
        "missing plan data must not create a fresh unsupported-account eviction signal"
    );
}

// Merge-safety anchor: prompt_gc tests in this file must stay aligned with the
// summary-only hidden contract, override rejection, recovery path, and fail-loud
// schema enforcement.
#[tokio::test]
async fn prompt_gc_sidecar_invalid_summary_payload_is_terminal_after_request() {
    let (mut session, turn_context, rx) = make_session_and_context_with_rx().await;
    let server = MockServer::start().await;
    let rollout_path = attach_rollout_recorder(&session).await;
    let checkpoint_id = format!("{}:prompt_gc:0", turn_context.sub_id);
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(StaticSseResponder {
            calls: AtomicUsize::new(0),
            response_body: sse(&[
                response_created("resp-1"),
                assistant_message_event("msg-1", "{\"summaries\":[]}"),
                response_completed_with_usage("resp-1", 17),
            ]),
            headers: Vec::new(),
        })
        .expect(1)
        .mount(&server)
        .await;
    configure_session_model_client_for_server(&mut session, &turn_context, &server);

    let sidecar = install_prompt_gc_active_turn(&session, &turn_context).await;
    let mut items = Vec::from(prompt_gc_triggering_exec_command_items(
        "call-1", 2_798, "ok",
    ));
    items.push(prompt_gc_phase_message("phase-1"));
    let observed_items = session.record_into_history(&items, &turn_context).await;
    sidecar.lock().await.observe_recorded_batch(&observed_items);
    let parent_client_session = session.services.model_client.new_session();

    run_prompt_gc_sidecar_if_needed(
        &session,
        &turn_context,
        Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new())),
        &parent_client_session,
        CancellationToken::new(),
    )
    .await;

    session.flush_rollout().await;
    let status = sidecar.lock().await.status.clone();
    assert_eq!(status.last_applied_checkpoint_seq, None);
    assert_eq!(
        status.last_error.as_deref(),
        Some("prompt_gc summary response requires a non-empty summaries list")
    );
    session.flush_rollout().await;
    assert_eq!(
        prompt_gc_rollout_markers(&rollout_path).await,
        vec![
            PromptGcCompactionMetadata {
                checkpoint_id: checkpoint_id.clone(),
                checkpoint_seq: 0,
                kind: PromptGcOutcomeKind::Started,
                phase: Some(PromptGcExecutionPhase::Prepare),
                stop_reason: None,
                error_message: None,
                applied_unit_count: None,
            },
            PromptGcCompactionMetadata {
                checkpoint_id,
                checkpoint_seq: 0,
                kind: PromptGcOutcomeKind::Failed,
                phase: Some(PromptGcExecutionPhase::Summarize),
                stop_reason: Some("invalid_summary_payload".to_string()),
                error_message: Some(
                    "prompt_gc summary response requires a non-empty summaries list".to_string(),
                ),
                applied_unit_count: None,
            },
        ]
    );
    assert!(rx.try_recv().is_err());
}

#[test]
fn prompt_gc_summary_response_rejects_unknown_top_level_fields() {
    let error = parse_prompt_gc_summary_response_text(
        r#"{"summaries":[{"chunk_id":"chunk-1","tool_context":"tool","reasoning_context":"reasoning"}],"junk":1}"#,
    )
    .expect_err("unknown top-level fields must fail");
    assert!(error.contains("unknown field"));
}

#[test]
fn prompt_gc_summary_response_rejects_multiple_assistant_messages() {
    let error = parse_prompt_gc_summary_response(&[
        assistant_message("not json"),
        assistant_message(
            r#"{"summaries":[{"chunk_id":"chunk-1","tool_context":"tool","reasoning_context":"reasoning"}]}"#,
        ),
    ])
    .expect_err("multiple assistant messages must fail");
    assert!(error.contains("requires exactly one assistant summary payload"));
}

#[test]
fn prompt_gc_summary_response_rejects_single_empty_assistant_message() {
    let error = parse_prompt_gc_summary_response(&[ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: Vec::new(),
        end_turn: None,
        phase: None,
    }])
    .expect_err("empty assistant payload must fail");
    assert!(error.contains("returned no assistant summary payload"));
}

#[test]
fn prompt_gc_summary_response_rejects_multiple_assistant_messages_when_one_is_empty() {
    let error = parse_prompt_gc_summary_response(&[
        ResponseItem::Message {
            id: None,
            role: "assistant".to_string(),
            content: Vec::new(),
            end_turn: None,
            phase: None,
        },
        assistant_message(
            r#"{"summaries":[{"chunk_id":"chunk-1","tool_context":"tool","reasoning_context":"reasoning"}]}"#,
        ),
    ])
    .expect_err("multiple assistant messages must fail");
    assert!(error.contains("requires exactly one assistant summary payload, got 2"));
}

#[test]
fn prompt_gc_summary_response_rejects_multiple_assistant_messages_with_non_output_content() {
    let error = parse_prompt_gc_summary_response(&[
        ResponseItem::Message {
            id: None,
            role: "assistant".to_string(),
            content: vec![ContentItem::InputText {
                text: "non-output".to_string(),
            }],
            end_turn: None,
            phase: None,
        },
        assistant_message(
            r#"{"summaries":[{"chunk_id":"chunk-1","tool_context":"tool","reasoning_context":"reasoning"}]}"#,
        ),
    ])
    .expect_err("assistant message count must include non-output assistant messages");
    assert!(error.contains("requires exactly one assistant summary payload, got 2"));
}

#[tokio::test]
async fn prompt_gc_sidecar_incomplete_summary_payload_fails_apply_validation() {
    let (mut session, turn_context, rx) = make_session_and_context_with_rx().await;
    let server = MockServer::start().await;
    let rollout_path = attach_rollout_recorder(&session).await;
    let checkpoint_id = format!("{}:prompt_gc:0", turn_context.sub_id);
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(StaticSseResponder {
            calls: AtomicUsize::new(0),
            response_body: sse(&[
                response_created("resp-1"),
                assistant_message_event(
                    "msg-1",
                    "{\"summaries\":[{\"chunk_id\":\"prompt_gc_chunk_0\",\"tool_context\":\"tool\",\"reasoning_context\":\"reasoning\"}]}",
                ),
                response_completed_with_usage("resp-1", 17),
            ]),
            headers: Vec::new(),
        })
        .expect(1)
        .mount(&server)
        .await;
    configure_session_model_client_for_server(&mut session, &turn_context, &server);

    let sidecar = install_prompt_gc_active_turn(&session, &turn_context).await;
    let mut items = Vec::new();
    items.extend(prompt_gc_triggering_exec_command_items(
        "call-1", 2_798, "first",
    ));
    items.extend(prompt_gc_triggering_exec_command_items(
        "call-2", 2_799, "second",
    ));
    items.push(prompt_gc_phase_message("phase-1"));
    let observed_items = session.record_into_history(&items, &turn_context).await;
    sidecar.lock().await.observe_recorded_batch(&observed_items);
    let parent_client_session = session.services.model_client.new_session();

    run_prompt_gc_sidecar_if_needed(
        &session,
        &turn_context,
        Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new())),
        &parent_client_session,
        CancellationToken::new(),
    )
    .await;

    let status = sidecar.lock().await.status.clone();
    assert_eq!(status.last_applied_checkpoint_seq, None);
    let last_error = status.last_error.expect("apply validation error");
    assert!(last_error.contains("invalid_summary_schema"));
    assert!(last_error.contains("prompt_gc requires summaries for every chunk_manifest entry"));
    assert!(last_error.contains("prompt_gc_chunk_1"));
    session.flush_rollout().await;
    assert_eq!(
        prompt_gc_rollout_markers(&rollout_path).await,
        vec![
            PromptGcCompactionMetadata {
                checkpoint_id: checkpoint_id.clone(),
                checkpoint_seq: 0,
                kind: PromptGcOutcomeKind::Started,
                phase: Some(PromptGcExecutionPhase::Prepare),
                stop_reason: None,
                error_message: None,
                applied_unit_count: None,
            },
            PromptGcCompactionMetadata {
                checkpoint_id,
                checkpoint_seq: 0,
                kind: PromptGcOutcomeKind::Failed,
                phase: Some(PromptGcExecutionPhase::Apply),
                stop_reason: Some("apply_failed".to_string()),
                error_message: Some(last_error),
                applied_unit_count: None,
            },
        ]
    );
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn prompt_gc_hidden_usage_limit_auto_switches_and_retries() {
    let (mut session, mut turn_context, rx) = make_session_and_context_with_rx().await;
    let server = MockServer::start().await;
    let observed_headers = Arc::new(std::sync::Mutex::new(Vec::new()));
    let auth_home = tempfile::tempdir().expect("create auth tempdir");
    let auth_store = crate::auth::AuthStore {
        active_account_id: Some("acc-0".to_string()),
        accounts: vec![
            crate::auth::StoredAccount {
                id: "acc-0".to_string(),
                label: None,
                tokens: test_chatgpt_token_data("acc-0"),
                last_refresh: Some(Utc::now()),
                usage: None,
            },
            crate::auth::StoredAccount {
                id: "acc-1".to_string(),
                label: None,
                tokens: test_chatgpt_token_data("acc-1"),
                last_refresh: Some(Utc::now()),
                usage: None,
            },
        ],
        ..crate::auth::AuthStore::default()
    };
    crate::auth::save_auth(
        auth_home.path(),
        &auth_store,
        crate::auth::AuthCredentialsStoreMode::File,
    )
    .expect("persist auth store");
    let auth_manager = crate::AuthManager::shared(
        auth_home.path().to_path_buf(),
        false,
        crate::auth::AuthCredentialsStoreMode::File,
    );
    let session_mut = Arc::get_mut(&mut session).expect("session arc should be unique");
    session_mut.services.auth_manager = Arc::clone(&auth_manager);
    let turn_context_mut =
        Arc::get_mut(&mut turn_context).expect("turn_context arc should be unique");
    turn_context_mut.auth_manager = Some(Arc::clone(&auth_manager));

    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(PromptGcRetryResponder {
            calls: AtomicUsize::new(0),
            observed_headers: Arc::clone(&observed_headers),
            resets_at: Utc
                .with_ymd_and_hms(2024, 1, 1, 0, 15, 0)
                .unwrap()
                .timestamp(),
            response_body: sse(&[
                response_created("resp-1"),
                assistant_message_event(
                    "msg-1",
                    "{\"summaries\":[{\"chunk_id\":\"prompt_gc_chunk_0\",\"tool_context\":\"\",\"reasoning_context\":\"summary\"}]}",
                ),
                response_completed_with_usage("resp-1", 17),
            ]),
        })
        .expect(2)
        .mount(&server)
        .await;
    configure_session_model_client_for_server(&mut session, &turn_context, &server);

    let sidecar = install_prompt_gc_active_turn(&session, &turn_context).await;
    let mut items = Vec::from(prompt_gc_triggering_exec_command_items(
        "call-1", 2_798, "ok",
    ));
    items.push(prompt_gc_phase_message("phase-1"));
    let observed_items = session.record_into_history(&items, &turn_context).await;
    sidecar.lock().await.observe_recorded_batch(&observed_items);
    let parent_client_session = session.services.model_client.new_session();

    run_prompt_gc_sidecar_if_needed(
        &session,
        &turn_context,
        Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new())),
        &parent_client_session,
        CancellationToken::new(),
    )
    .await;

    let status = sidecar.lock().await.status.clone();
    assert_eq!(status.last_applied_checkpoint_seq, Some(0));
    assert_eq!(status.last_error, None);
    assert_eq!(status.blocked_reason, None);
    assert_eq!(
        auth_manager
            .list_accounts()
            .into_iter()
            .find(|account| account.is_active)
            .map(|account| account.id),
        Some("acc-1".to_string())
    );
    assert_eq!(
        *observed_headers
            .lock()
            .expect("prompt_gc retry headers should be recorded"),
        vec![
            ("acc-0".to_string(), "Bearer access-acc-0".to_string()),
            ("acc-1".to_string(), "Bearer access-acc-1".to_string()),
        ]
    );
    assert!(
        rx.try_recv().is_err(),
        "hidden prompt_gc autoswitch must stay silent in visible events"
    );
}

#[tokio::test]
async fn prompt_gc_hidden_usage_limit_blocks_remaining_turn_after_unrecoverable_failure() {
    let (mut session, turn_context, rx) = make_session_and_context_with_rx().await;
    let server = MockServer::start().await;
    let rollout_path = attach_rollout_recorder(&session).await;
    let checkpoint_id = format!("{}:prompt_gc:0", turn_context.sub_id);
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("content-type", "application/json")
                .insert_header("x-codex-primary-used-percent", "100.0")
                .insert_header("x-codex-primary-window-minutes", "15")
                .set_body_json(json!({
                    "error": {
                        "type": "usage_limit_reached",
                        "plan_type": "pro",
                        "resets_at": Utc
                            .with_ymd_and_hms(2024, 1, 1, 0, 15, 0)
                            .unwrap()
                            .timestamp(),
                    }
                })),
        )
        .expect(1)
        .mount(&server)
        .await;
    configure_session_model_client_for_server(&mut session, &turn_context, &server);

    let sidecar = install_prompt_gc_active_turn(&session, &turn_context).await;
    let mut items = Vec::from(prompt_gc_triggering_exec_command_items(
        "call-1", 2_798, "ok",
    ));
    items.push(prompt_gc_phase_message("phase-1"));
    let observed_items = session.record_into_history(&items, &turn_context).await;
    sidecar.lock().await.observe_recorded_batch(&observed_items);
    let parent_client_session = session.services.model_client.new_session();

    run_prompt_gc_sidecar_if_needed(
        &session,
        &turn_context,
        Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new())),
        &parent_client_session,
        CancellationToken::new(),
    )
    .await;

    let status = sidecar.lock().await.status.clone();
    assert_eq!(status.last_applied_checkpoint_seq, None);
    assert!(status.last_error.is_some());
    assert!(status.blocked_reason.is_some());
    assert!(
        status
            .blocked_reason
            .as_deref()
            .is_some_and(|error| error.contains("You've hit your usage limit"))
    );

    let second_phase_message = ResponseItem::Message {
        id: Some("phase-2".to_string()),
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: "later checkpoint".to_string(),
        }],
        end_turn: None,
        phase: Some(codex_protocol::models::MessagePhase::FinalAnswer),
    };
    let observed_items = session
        .record_into_history(std::slice::from_ref(&second_phase_message), &turn_context)
        .await;
    sidecar.lock().await.observe_recorded_batch(&observed_items);

    run_prompt_gc_sidecar_if_needed(
        &session,
        &turn_context,
        Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new())),
        &parent_client_session,
        CancellationToken::new(),
    )
    .await;

    session.flush_rollout().await;
    assert_eq!(
        prompt_gc_rollout_markers(&rollout_path).await,
        vec![
            PromptGcCompactionMetadata {
                checkpoint_id: checkpoint_id.clone(),
                checkpoint_seq: 0,
                kind: PromptGcOutcomeKind::Started,
                phase: Some(PromptGcExecutionPhase::Prepare),
                stop_reason: None,
                error_message: None,
                applied_unit_count: None,
            },
            PromptGcCompactionMetadata {
                checkpoint_id,
                checkpoint_seq: 0,
                kind: PromptGcOutcomeKind::Failed,
                phase: Some(PromptGcExecutionPhase::Request),
                stop_reason: Some("usage_limit_reached".to_string()),
                error_message: status.last_error,
                applied_unit_count: None,
            },
        ]
    );
    assert!(
        rx.try_recv().is_err(),
        "blocked hidden prompt_gc must not emit visible events"
    );
}

#[tokio::test]
async fn prompt_gc_hidden_request_error_does_not_apply() {
    let (mut session, turn_context, rx) = make_session_and_context_with_rx().await;
    let server = MockServer::start().await;
    let rollout_path = attach_rollout_recorder(&session).await;
    let checkpoint_id = format!("{}:prompt_gc:0", turn_context.sub_id);
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(500))
        .expect(1..)
        .mount(&server)
        .await;
    configure_session_model_client_for_server(&mut session, &turn_context, &server);

    let sidecar = install_prompt_gc_active_turn(&session, &turn_context).await;
    let mut items = Vec::from(prompt_gc_triggering_exec_command_items(
        "call-1", 2_798, "ok",
    ));
    items.push(prompt_gc_phase_message("phase-1"));
    let observed_items = session.record_into_history(&items, &turn_context).await;
    sidecar.lock().await.observe_recorded_batch(&observed_items);
    let parent_client_session = session.services.model_client.new_session();

    run_prompt_gc_sidecar_if_needed(
        &session,
        &turn_context,
        Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new())),
        &parent_client_session,
        CancellationToken::new(),
    )
    .await;

    let status = sidecar.lock().await.status.clone();
    assert_eq!(status.last_applied_checkpoint_seq, None);
    assert!(status.last_error.is_some());
    session.flush_rollout().await;
    assert_eq!(
        prompt_gc_rollout_markers(&rollout_path).await,
        vec![
            PromptGcCompactionMetadata {
                checkpoint_id: checkpoint_id.clone(),
                checkpoint_seq: 0,
                kind: PromptGcOutcomeKind::Started,
                phase: Some(PromptGcExecutionPhase::Prepare),
                stop_reason: None,
                error_message: None,
                applied_unit_count: None,
            },
            PromptGcCompactionMetadata {
                checkpoint_id,
                checkpoint_seq: 0,
                kind: PromptGcOutcomeKind::Failed,
                phase: Some(PromptGcExecutionPhase::Request),
                stop_reason: Some("request_failed".to_string()),
                error_message: status.last_error,
                applied_unit_count: None,
            },
        ]
    );
    assert!(rx.try_recv().is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prompt_gc_hidden_context_window_does_not_emit_token_count() {
    let (session, turn_context, rx) = make_session_and_context_with_rx().await;
    let token_info_before = {
        let state = session.state.lock().await;
        state.token_info()
    };

    maybe_set_total_tokens_full_for_execution_mode(
        &session,
        &turn_context,
        SamplingExecutionMode::Hidden,
    )
    .await;

    assert!(
        rx.try_recv().is_err(),
        "hidden context-window handling should not emit visible token counts"
    );
    let token_info_after = {
        let state = session.state.lock().await;
        state.token_info()
    };
    assert_eq!(token_info_after, token_info_before);
}

#[tokio::test]
async fn prompt_gc_sidecar_recovers_noted_apply_outcome_after_stream_failure() {
    let (session, turn_context, rx) = make_session_and_context_with_rx().await;
    let sidecar = install_prompt_gc_active_turn(&session, &turn_context).await;
    let reasoning = prompt_gc_large_reasoning_unit("reasoning-1");
    let phase_message = prompt_gc_phase_message("phase-1");
    let observed_items = session
        .record_into_history(&[reasoning, phase_message], &turn_context)
        .await;
    sidecar.lock().await.observe_recorded_batch(&observed_items);

    let checkpoint = sidecar
        .lock()
        .await
        .take_pending_checkpoint()
        .expect("checkpoint");
    sidecar
        .lock()
        .await
        .note_apply_outcome(&checkpoint.checkpoint_id, vec![7]);

    let parent_client_session = session.services.model_client.new_session();
    run_prompt_gc_sidecar_if_needed(
        &session,
        &turn_context,
        Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new())),
        &parent_client_session,
        CancellationToken::new(),
    )
    .await;

    let status = sidecar.lock().await.status.clone();
    assert_eq!(status.last_error, None);
    assert_eq!(status.last_applied_checkpoint_seq, Some(0));
    assert!(
        rx.try_recv().is_err(),
        "noted hidden prompt_gc recovery should not emit visible events"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prompt_gc_hidden_transport_fallback_warning_is_suppressed() {
    let (session, turn_context, rx) = make_session_and_context_with_rx().await;

    maybe_emit_transport_fallback_warning_for_execution_mode(
        &session,
        &turn_context,
        SamplingExecutionMode::Hidden,
        &CodexErr::Stream("websocket disconnected".to_string(), None),
    )
    .await;

    assert!(
        rx.try_recv().is_err(),
        "hidden transport fallback should not emit visible warnings"
    );
}

#[tokio::test]
async fn prompt_gc_hidden_fallback_does_not_disable_visible_websocket_transport() {
    let (mut session, mut turn_context, _rx) = make_session_and_context_with_rx().await;
    let turn_context_mut =
        Arc::get_mut(&mut turn_context).expect("turn_context arc should be unique");
    turn_context_mut.provider.supports_websockets = true;

    let session_mut = Arc::get_mut(&mut session).expect("session arc should be unique");
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(426))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(StaticSseResponder {
            calls: AtomicUsize::new(0),
            response_body: sse(&[
                response_created("resp-1"),
                response_completed_with_usage("resp-1", 1),
            ]),
            headers: Vec::new(),
        })
        .mount(&server)
        .await;
    let auth_manager = Arc::clone(&session_mut.services.auth_manager);
    session_mut.services.model_client = ModelClient::new(
        Some(auth_manager),
        session_mut.conversation_id,
        {
            let mut provider = turn_context.provider.clone();
            provider.base_url = Some(format!("{}/v1", server.uri()));
            provider
        },
        turn_context.session_source.clone(),
        turn_context.config.model_verbosity,
        turn_context
            .config
            .features
            .enabled(Feature::EnableRequestCompression),
        turn_context
            .config
            .features
            .enabled(Feature::RuntimeMetrics),
        None,
    );

    let parent_client_session = session.services.model_client.new_session();
    let mut client_session = parent_client_session.new_hidden_child_session();
    assert!(
        session.services.model_client.responses_websocket_enabled(),
        "test setup should start with websockets enabled"
    );

    let _stream = client_session
        .stream(
            &Prompt {
                input: vec![user_message("gc checkpoint")],
                tools: Vec::new(),
                parallel_tool_calls: false,
                base_instructions: BaseInstructions {
                    text: "hidden prompt gc".to_string(),
                },
                personality: None,
                output_schema: None,
            },
            &turn_context.model_info,
            &turn_context.session_telemetry,
            turn_context.reasoning_effort,
            turn_context.reasoning_summary,
            turn_context.config.service_tier,
            None,
        )
        .await
        .expect("hidden prompt_gc should fall back to HTTP without mutating visible transport");
    assert!(
        session.services.model_client.responses_websocket_enabled(),
        "hidden prompt_gc fallback handling must leave visible websocket transport enabled"
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

    assert!(
        session
            .services
            .model_client
            .new_session()
            .try_switch_fallback_transport(
                &turn_context.session_telemetry,
                &turn_context.model_info
            ),
        "visible execution should still be able to activate fallback"
    );
    assert!(
        !session.services.model_client.responses_websocket_enabled(),
        "visible fallback activation should disable websocket transport"
    );
}

fn make_mcp_tool(
    server_name: &str,
    tool_name: &str,
    connector_id: Option<&str>,
    connector_name: Option<&str>,
) -> ToolInfo {
    let tool_namespace = if server_name == CODEX_APPS_MCP_SERVER_NAME {
        connector_name
            .map(crate::connectors::sanitize_name)
            .map(|connector_name| format!("mcp__{server_name}__{connector_name}"))
            .unwrap_or_else(|| server_name.to_string())
    } else {
        server_name.to_string()
    };

    ToolInfo {
        server_name: server_name.to_string(),
        tool_name: tool_name.to_string(),
        tool_namespace,
        tool: Tool {
            name: tool_name.to_string().into(),
            title: None,
            description: Some(format!("Test tool: {tool_name}").into()),
            input_schema: Arc::new(JsonObject::default()),
            output_schema: None,
            annotations: None,
            execution: None,
            icons: None,
            meta: None,
        },
        connector_id: connector_id.map(str::to_string),
        connector_name: connector_name.map(str::to_string),
        plugin_display_names: Vec::new(),
        connector_description: None,
    }
}

fn response_created(id: &str) -> Value {
    json!({
        "type": "response.created",
        "response": {
            "id": id,
        }
    })
}

fn response_completed_with_usage(id: &str, total_tokens: u32) -> Value {
    json!({
        "type": "response.completed",
        "response": {
            "id": id,
            "usage": {
                "input_tokens": total_tokens,
                "input_tokens_details": null,
                "output_tokens": 0,
                "output_tokens_details": null,
                "total_tokens": total_tokens
            }
        }
    })
}

fn assistant_message_event(message_id: &str, text: &str) -> Value {
    json!({
        "type": "response.output_item.done",
        "item": {
            "type": "message",
            "id": message_id,
            "role": "assistant",
            "status": "completed",
            "content": [{
                "type": "output_text",
                "text": text,
                "annotations": []
            }]
        }
    })
}

fn sse(events: &[Value]) -> String {
    let mut output = String::new();
    for event in events {
        let event_type = event
            .get("type")
            .and_then(Value::as_str)
            .expect("response event type");
        writeln!(&mut output, "event: {event_type}").expect("write SSE event type");
        write!(&mut output, "data: {event}\n\n").expect("write SSE event body");
    }
    output
}

struct StaticSseResponder {
    calls: AtomicUsize,
    response_body: String,
    headers: Vec<(String, String)>,
}

impl Respond for StaticSseResponder {
    fn respond(&self, _request: &wiremock::Request) -> ResponseTemplate {
        let call_num = self.calls.fetch_add(1, Ordering::SeqCst);
        if call_num > 0 {
            panic!("unexpected extra model request {call_num}");
        }
        let mut response = ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string(self.response_body.clone());
        for (name, value) in &self.headers {
            response = response.insert_header(name.clone(), value.clone());
        }
        response
    }
}

struct PromptGcRetryResponder {
    calls: AtomicUsize,
    observed_headers: Arc<std::sync::Mutex<Vec<(String, String)>>>,
    resets_at: i64,
    response_body: String,
}

impl Respond for PromptGcRetryResponder {
    fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
        let account_id = request
            .headers
            .get("chatgpt-account-id")
            .and_then(|value| value.to_str().ok())
            .expect("chatgpt-account-id header")
            .to_string();
        let authorization = request
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .expect("authorization header")
            .to_string();
        self.observed_headers
            .lock()
            .expect("prompt_gc retry headers should be writable")
            .push((account_id, authorization));
        match self.calls.fetch_add(1, Ordering::SeqCst) {
            0 => ResponseTemplate::new(429)
                .insert_header("content-type", "application/json")
                .insert_header("x-codex-primary-used-percent", "100.0")
                .insert_header("x-codex-primary-window-minutes", "15")
                .set_body_json(json!({
                    "error": {
                        "type": "usage_limit_reached",
                        "plan_type": "pro",
                        "resets_at": self.resets_at,
                    }
                })),
            1 => ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(self.response_body.clone()),
            call_num => panic!("unexpected extra prompt_gc request {call_num}"),
        }
    }
}

struct FlushFailingPromptGcRolloutSink;

#[async_trait::async_trait]
impl PromptGcRolloutSink for FlushFailingPromptGcRolloutSink {
    async fn persist_items_atomically(&self, _items: &[RolloutItem]) -> std::io::Result<()> {
        Err(std::io::Error::other("flush failed"))
    }
}

async fn install_prompt_gc_active_turn(
    session: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
) -> Arc<tokio::sync::Mutex<crate::prompt_gc_sidecar::PromptGcSidecar>> {
    let mut active_turn = crate::state::ActiveTurn::default();
    let sidecar = active_turn.ensure_prompt_gc_sidecar();
    sidecar.lock().await.bind_turn(turn_context.sub_id.clone());
    active_turn.add_task(crate::state::RunningTask {
        done: Arc::new(tokio::sync::Notify::new()),
        kind: TaskKind::Regular,
        task: Arc::new(NeverEndingTask {
            kind: TaskKind::Regular,
            listen_to_cancellation_token: false,
        }),
        cancellation_token: CancellationToken::new(),
        handle: Arc::new(tokio_util::task::AbortOnDropHandle::new(tokio::spawn(
            async {},
        ))),
        turn_context: Arc::clone(turn_context),
        _timer: None,
    });
    *session.active_turn.lock().await = Some(active_turn);
    sidecar
}

fn configure_session_model_client_for_server(
    session: &mut Arc<Session>,
    turn_context: &Arc<TurnContext>,
    server: &MockServer,
) {
    let mut provider = crate::built_in_model_providers(None)["openai"].clone();
    provider.base_url = Some(format!("{}/v1", server.uri()));
    let session_mut = Arc::get_mut(session).expect("session arc should be unique");
    let auth_manager = Arc::clone(&session_mut.services.auth_manager);
    session_mut.services.model_client = ModelClient::new(
        Some(auth_manager),
        session_mut.conversation_id,
        provider,
        turn_context.session_source.clone(),
        turn_context.config.model_verbosity,
        turn_context
            .config
            .features
            .enabled(Feature::EnableRequestCompression),
        turn_context
            .config
            .features
            .enabled(Feature::RuntimeMetrics),
        None,
    );
}

fn prompt_gc_large_reasoning_unit(id: &str) -> ResponseItem {
    ResponseItem::Reasoning {
        id: id.to_string(),
        summary: Vec::new(),
        content: None,
        encrypted_content: Some("x".repeat(2_000)),
    }
}

fn prompt_gc_triggering_exec_command_items(
    call_id: &str,
    token_qty: usize,
    body: &str,
) -> [ResponseItem; 2] {
    [
        ResponseItem::FunctionCall {
            id: None,
            call_id: call_id.to_string(),
            name: "exec_command".to_string(),
            namespace: None,
            arguments: format!("{{\"cmd\":\"printf {body:?}\"}}"),
        },
        ResponseItem::FunctionCallOutput {
            call_id: call_id.to_string(),
            output: FunctionCallOutputPayload::from_text(prompt_gc_unified_exec_output(
                token_qty, body,
            )),
        },
    ]
}

fn prompt_gc_phase_message(id: &str) -> ResponseItem {
    ResponseItem::Message {
        id: Some(id.to_string()),
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: "checkpoint".to_string(),
        }],
        end_turn: None,
        phase: Some(codex_protocol::models::MessagePhase::Commentary),
    }
}
#[test]
fn validated_network_policy_amendment_host_allows_normalized_match() {
    let amendment = NetworkPolicyAmendment {
        host: "ExAmPlE.Com.:443".to_string(),
        action: NetworkPolicyRuleAction::Allow,
    };
    let context = NetworkApprovalContext {
        host: "example.com".to_string(),
        protocol: NetworkApprovalProtocol::Https,
    };

    let host = Session::validated_network_policy_amendment_host(&amendment, &context)
        .expect("normalized hosts should match");

    assert_eq!(host, "example.com");
}

#[test]
fn validated_network_policy_amendment_host_rejects_mismatch() {
    let amendment = NetworkPolicyAmendment {
        host: "evil.example.com".to_string(),
        action: NetworkPolicyRuleAction::Deny,
    };
    let context = NetworkApprovalContext {
        host: "api.example.com".to_string(),
        protocol: NetworkApprovalProtocol::Https,
    };

    let err = Session::validated_network_policy_amendment_host(&amendment, &context)
        .expect_err("mismatched hosts should be rejected");

    let message = err.to_string();
    assert!(message.contains("does not match approved host"));
}

#[tokio::test]
async fn start_managed_network_proxy_applies_execpolicy_network_rules() -> anyhow::Result<()> {
    let spec = crate::config::NetworkProxySpec::from_config_and_constraints(
        NetworkProxyConfig::default(),
        None,
        &SandboxPolicy::new_workspace_write_policy(),
    )?;
    let mut exec_policy = Policy::empty();
    exec_policy.add_network_rule(
        "example.com",
        NetworkRuleProtocol::Https,
        Decision::Allow,
        None,
    )?;

    let (started_proxy, _) = Session::start_managed_network_proxy(
        &spec,
        &exec_policy,
        &SandboxPolicy::new_workspace_write_policy(),
        None,
        None,
        false,
        crate::config::NetworkProxyAuditMetadata::default(),
    )
    .await?;

    let current_cfg = started_proxy.proxy().current_cfg().await?;
    assert_eq!(
        current_cfg.network.allowed_domains,
        vec!["example.com".to_string()]
    );
    Ok(())
}

#[tokio::test]
async fn start_managed_network_proxy_ignores_invalid_execpolicy_network_rules() -> anyhow::Result<()>
{
    let spec = crate::config::NetworkProxySpec::from_config_and_constraints(
        NetworkProxyConfig::default(),
        Some(NetworkConstraints {
            allowed_domains: Some(vec!["managed.example.com".to_string()]),
            managed_allowed_domains_only: Some(true),
            ..Default::default()
        }),
        &SandboxPolicy::new_workspace_write_policy(),
    )?;
    let mut exec_policy = Policy::empty();
    exec_policy.add_network_rule(
        "example.com",
        NetworkRuleProtocol::Https,
        Decision::Allow,
        None,
    )?;

    let (started_proxy, _) = Session::start_managed_network_proxy(
        &spec,
        &exec_policy,
        &SandboxPolicy::new_workspace_write_policy(),
        None,
        None,
        false,
        crate::config::NetworkProxyAuditMetadata::default(),
    )
    .await?;

    let current_cfg = started_proxy.proxy().current_cfg().await?;
    assert_eq!(
        current_cfg.network.allowed_domains,
        vec!["managed.example.com".to_string()]
    );
    Ok(())
}

#[tokio::test]
async fn get_base_instructions_no_user_content() {
    let prompt_with_apply_patch_instructions =
        include_str!("../prompt_with_apply_patch_instructions.md");
    let models_response: ModelsResponse =
        serde_json::from_str(include_str!("../models.json")).expect("valid models.json");
    let model_info_for_slug = |slug: &str, config: &Config| {
        let model = models_response
            .models
            .iter()
            .find(|candidate| candidate.slug == slug)
            .cloned()
            .unwrap_or_else(|| panic!("model slug {slug} is missing from models.json"));
        model_info::with_config_overrides(model, config)
    };
    let test_cases = vec![
        InstructionsTestCase {
            slug: "gpt-5",
            expects_apply_patch_instructions: false,
        },
        InstructionsTestCase {
            slug: "gpt-5.1",
            expects_apply_patch_instructions: false,
        },
        InstructionsTestCase {
            slug: "gpt-5.1-codex",
            expects_apply_patch_instructions: false,
        },
        InstructionsTestCase {
            slug: "gpt-5.1-codex-max",
            expects_apply_patch_instructions: false,
        },
    ];

    let (session, _turn_context) = make_session_and_context().await;
    let config = test_config();

    for test_case in test_cases {
        let model_info = model_info_for_slug(test_case.slug, &config);
        if test_case.expects_apply_patch_instructions {
            assert_eq!(
                model_info.base_instructions.as_str(),
                prompt_with_apply_patch_instructions
            );
        }

        {
            let mut state = session.state.lock().await;
            state.session_configuration.base_instructions = model_info.base_instructions.clone();
        }

        let base_instructions = session.get_base_instructions().await;
        assert_eq!(base_instructions.text, model_info.base_instructions);
    }
}

#[tokio::test]
async fn reload_user_config_layer_updates_effective_apps_config() {
    let (session, _turn_context) = make_session_and_context().await;
    let codex_home = session.codex_home().await;
    std::fs::create_dir_all(&codex_home).expect("create codex home");
    let config_toml_path = codex_home.join(CONFIG_TOML_FILE);
    std::fs::write(
        &config_toml_path,
        "[apps.calendar]\nenabled = false\ndestructive_enabled = false\n",
    )
    .expect("write user config");

    session.reload_user_config_layer().await;

    let config = session.get_config().await;
    let apps_toml = config
        .config_layer_stack
        .effective_config()
        .as_table()
        .and_then(|table| table.get("apps"))
        .cloned()
        .expect("apps table");
    let apps = crate::config::types::AppsConfigToml::deserialize(apps_toml)
        .expect("deserialize apps config");
    let app = apps
        .apps
        .get("calendar")
        .expect("calendar app config exists");

    assert!(!app.enabled);
    assert_eq!(app.destructive_enabled, Some(false));
}

#[test]
fn filter_connectors_for_input_skips_duplicate_slug_mentions() {
    let connectors = vec![
        make_connector("one", "Foo Bar"),
        make_connector("two", "Foo-Bar"),
    ];
    let input = vec![user_message("use $foo-bar")];
    let explicitly_enabled_connectors = HashSet::new();
    let skill_name_counts_lower = HashMap::new();

    let selected = filter_connectors_for_input(
        &connectors,
        &input,
        &explicitly_enabled_connectors,
        &skill_name_counts_lower,
    );

    assert_eq!(selected, Vec::new());
}

#[test]
fn filter_connectors_for_input_skips_when_skill_name_conflicts() {
    let connectors = vec![make_connector("one", "Todoist")];
    let input = vec![user_message("use $todoist")];
    let explicitly_enabled_connectors = HashSet::new();
    let skill_name_counts_lower = HashMap::from([("todoist".to_string(), 1)]);

    let selected = filter_connectors_for_input(
        &connectors,
        &input,
        &explicitly_enabled_connectors,
        &skill_name_counts_lower,
    );

    assert_eq!(selected, Vec::new());
}

#[test]
fn filter_connectors_for_input_skips_disabled_connectors() {
    let mut connector = make_connector("calendar", "Calendar");
    connector.is_enabled = false;
    let input = vec![user_message("use $calendar")];
    let explicitly_enabled_connectors = HashSet::new();
    let selected = filter_connectors_for_input(
        &[connector],
        &input,
        &explicitly_enabled_connectors,
        &HashMap::new(),
    );

    assert_eq!(selected, Vec::new());
}

#[test]
fn collect_explicit_app_ids_from_skill_items_includes_linked_mentions() {
    let connectors = vec![make_connector("calendar", "Calendar")];
    let skill_items = vec![skill_message(
        "<skill>\n<name>demo</name>\n<path>/tmp/skills/demo/SKILL.md</path>\nuse [$calendar](app://calendar)\n</skill>",
    )];

    let connector_ids =
        collect_explicit_app_ids_from_skill_items(&skill_items, &connectors, &HashMap::new());

    assert_eq!(connector_ids, HashSet::from(["calendar".to_string()]));
}

#[test]
fn collect_explicit_app_ids_from_skill_items_resolves_unambiguous_plain_mentions() {
    let connectors = vec![make_connector("calendar", "Calendar")];
    let skill_items = vec![skill_message(
        "<skill>\n<name>demo</name>\n<path>/tmp/skills/demo/SKILL.md</path>\nuse $calendar\n</skill>",
    )];

    let connector_ids =
        collect_explicit_app_ids_from_skill_items(&skill_items, &connectors, &HashMap::new());

    assert_eq!(connector_ids, HashSet::from(["calendar".to_string()]));
}

#[test]
fn collect_explicit_app_ids_from_skill_items_skips_plain_mentions_with_skill_conflicts() {
    let connectors = vec![make_connector("calendar", "Calendar")];
    let skill_items = vec![skill_message(
        "<skill>\n<name>demo</name>\n<path>/tmp/skills/demo/SKILL.md</path>\nuse $calendar\n</skill>",
    )];
    let skill_name_counts_lower = HashMap::from([("calendar".to_string(), 1)]);

    let connector_ids = collect_explicit_app_ids_from_skill_items(
        &skill_items,
        &connectors,
        &skill_name_counts_lower,
    );

    assert_eq!(connector_ids, HashSet::<String>::new());
}

#[test]
fn non_app_mcp_tools_remain_visible_without_search_selection() {
    let mcp_tools = HashMap::from([
        (
            "mcp__codex_apps__calendar_create_event".to_string(),
            make_mcp_tool(
                CODEX_APPS_MCP_SERVER_NAME,
                "calendar_create_event",
                Some("calendar"),
                Some("Calendar"),
            ),
        ),
        (
            "mcp__rmcp__echo".to_string(),
            make_mcp_tool("rmcp", "echo", None, None),
        ),
    ]);

    let mut selected_mcp_tools = mcp_tools
        .iter()
        .filter(|(_, tool)| tool.server_name != CODEX_APPS_MCP_SERVER_NAME)
        .map(|(name, tool)| (name.clone(), tool.clone()))
        .collect::<HashMap<_, _>>();

    let connectors = connectors::accessible_connectors_from_mcp_tools(&mcp_tools);
    let explicitly_enabled_connectors = HashSet::new();
    let connectors = filter_connectors_for_input(
        &connectors,
        &[user_message("run echo")],
        &explicitly_enabled_connectors,
        &HashMap::new(),
    );
    let config = test_config();
    selected_mcp_tools.extend(filter_codex_apps_mcp_tools(
        &mcp_tools,
        &connectors,
        &config,
    ));

    let mut tool_names: Vec<String> = selected_mcp_tools.into_keys().collect();
    tool_names.sort();
    assert_eq!(tool_names, vec!["mcp__rmcp__echo".to_string()]);
}

#[test]
fn search_tool_selection_keeps_codex_apps_tools_without_mentions() {
    let selected_tool_names = [
        "mcp__codex_apps__calendar_create_event".to_string(),
        "mcp__rmcp__echo".to_string(),
    ];
    let mcp_tools = HashMap::from([
        (
            "mcp__codex_apps__calendar_create_event".to_string(),
            make_mcp_tool(
                CODEX_APPS_MCP_SERVER_NAME,
                "calendar_create_event",
                Some("calendar"),
                Some("Calendar"),
            ),
        ),
        (
            "mcp__rmcp__echo".to_string(),
            make_mcp_tool("rmcp", "echo", None, None),
        ),
    ]);

    let mut selected_mcp_tools = mcp_tools
        .iter()
        .filter(|(name, _)| selected_tool_names.contains(name))
        .map(|(name, tool)| (name.clone(), tool.clone()))
        .collect::<HashMap<_, _>>();
    let connectors = connectors::accessible_connectors_from_mcp_tools(&mcp_tools);
    let explicitly_enabled_connectors = HashSet::new();
    let connectors = filter_connectors_for_input(
        &connectors,
        &[user_message("run the selected tools")],
        &explicitly_enabled_connectors,
        &HashMap::new(),
    );
    let config = test_config();
    selected_mcp_tools.extend(filter_codex_apps_mcp_tools(
        &mcp_tools,
        &connectors,
        &config,
    ));

    let mut tool_names: Vec<String> = selected_mcp_tools.into_keys().collect();
    tool_names.sort();
    assert_eq!(
        tool_names,
        vec![
            "mcp__codex_apps__calendar_create_event".to_string(),
            "mcp__rmcp__echo".to_string(),
        ]
    );
}

#[test]
fn apps_mentions_add_codex_apps_tools_to_search_selected_set() {
    let selected_tool_names = ["mcp__rmcp__echo".to_string()];
    let mcp_tools = HashMap::from([
        (
            "mcp__codex_apps__calendar_create_event".to_string(),
            make_mcp_tool(
                CODEX_APPS_MCP_SERVER_NAME,
                "calendar_create_event",
                Some("calendar"),
                Some("Calendar"),
            ),
        ),
        (
            "mcp__rmcp__echo".to_string(),
            make_mcp_tool("rmcp", "echo", None, None),
        ),
    ]);

    let mut selected_mcp_tools = mcp_tools
        .iter()
        .filter(|(name, _)| selected_tool_names.contains(name))
        .map(|(name, tool)| (name.clone(), tool.clone()))
        .collect::<HashMap<_, _>>();
    let connectors = connectors::accessible_connectors_from_mcp_tools(&mcp_tools);
    let explicitly_enabled_connectors = HashSet::new();
    let connectors = filter_connectors_for_input(
        &connectors,
        &[user_message("use $calendar and then echo the response")],
        &explicitly_enabled_connectors,
        &HashMap::new(),
    );
    let config = test_config();
    selected_mcp_tools.extend(filter_codex_apps_mcp_tools(
        &mcp_tools,
        &connectors,
        &config,
    ));

    let mut tool_names: Vec<String> = selected_mcp_tools.into_keys().collect();
    tool_names.sort();
    assert_eq!(
        tool_names,
        vec![
            "mcp__codex_apps__calendar_create_event".to_string(),
            "mcp__rmcp__echo".to_string(),
        ]
    );
}

#[tokio::test]
async fn reconstruct_history_matches_live_compactions() {
    let (session, turn_context) = make_session_and_context().await;
    let (rollout_items, expected) = sample_rollout(&session, &turn_context).await;

    let reconstruction_turn = session.new_default_turn().await;
    let reconstructed = session
        .reconstruct_history_from_rollout(reconstruction_turn.as_ref(), &rollout_items)
        .await;

    assert_eq!(expected, reconstructed.history);
}

#[tokio::test]
async fn reconstruct_history_uses_replacement_history_verbatim() {
    let (session, turn_context) = make_session_and_context().await;
    let summary_item = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "summary".to_string(),
        }],
        end_turn: None,
        phase: None,
    };
    let replacement_history = vec![
        summary_item.clone(),
        ResponseItem::Message {
            id: None,
            role: "developer".to_string(),
            content: vec![ContentItem::InputText {
                text: "stale developer instructions".to_string(),
            }],
            end_turn: None,
            phase: None,
        },
    ];
    let rollout_items = vec![RolloutItem::Compacted(CompactedItem {
        message: String::new(),
        replacement_history: Some(replacement_history.clone()),
        prompt_gc: None,
    })];

    let reconstructed = session
        .reconstruct_history_from_rollout(&turn_context, &rollout_items)
        .await;

    assert_eq!(reconstructed.history, replacement_history);
}

#[tokio::test]
async fn record_initial_history_reconstructs_resumed_transcript() {
    let (session, turn_context) = make_session_and_context().await;
    let (rollout_items, expected) = sample_rollout(&session, &turn_context).await;

    session
        .record_initial_history(InitialHistory::Resumed(ResumedHistory {
            conversation_id: ThreadId::default(),
            history: rollout_items,
            rollout_path: PathBuf::from("/tmp/resume.jsonl"),
        }))
        .await;

    let history = session.state.lock().await.clone_history();
    assert_eq!(expected, history.raw_items());
}

#[tokio::test]
async fn record_initial_history_new_defers_initial_context_until_first_turn() {
    let (session, _turn_context) = make_session_and_context().await;

    session.record_initial_history(InitialHistory::New).await;

    let history = session.clone_history().await;
    assert_eq!(history.raw_items().to_vec(), Vec::<ResponseItem>::new());
    assert!(session.reference_context_item().await.is_none());
    assert_eq!(session.previous_turn_settings().await, None);
}

#[tokio::test]
async fn resumed_history_injects_initial_context_on_first_context_update_only() {
    let (session, turn_context) = make_session_and_context().await;
    let (rollout_items, mut expected) = sample_rollout(&session, &turn_context).await;

    session
        .record_initial_history(InitialHistory::Resumed(ResumedHistory {
            conversation_id: ThreadId::default(),
            history: rollout_items,
            rollout_path: PathBuf::from("/tmp/resume.jsonl"),
        }))
        .await;

    let history_before_seed = session.state.lock().await.clone_history();
    assert_eq!(expected, history_before_seed.raw_items());

    session
        .record_context_updates_and_set_reference_context_item(&turn_context)
        .await;
    expected.extend(session.build_initial_context(&turn_context).await);
    let history_after_seed = session.clone_history().await;
    assert_eq!(expected, history_after_seed.raw_items());

    session
        .record_context_updates_and_set_reference_context_item(&turn_context)
        .await;
    let history_after_second_seed = session.clone_history().await;
    assert_eq!(
        history_after_seed.raw_items(),
        history_after_second_seed.raw_items()
    );
}

#[tokio::test]
async fn record_initial_history_seeds_token_info_from_rollout() {
    let (session, turn_context) = make_session_and_context().await;
    let (mut rollout_items, _expected) = sample_rollout(&session, &turn_context).await;

    let info1 = TokenUsageInfo {
        total_token_usage: TokenUsage {
            input_tokens: 10,
            cached_input_tokens: 0,
            output_tokens: 20,
            reasoning_output_tokens: 0,
            total_tokens: 30,
        },
        last_token_usage: TokenUsage {
            input_tokens: 3,
            cached_input_tokens: 0,
            output_tokens: 4,
            reasoning_output_tokens: 0,
            total_tokens: 7,
        },
        model_context_window: Some(1_000),
    };
    let info2 = TokenUsageInfo {
        total_token_usage: TokenUsage {
            input_tokens: 100,
            cached_input_tokens: 50,
            output_tokens: 200,
            reasoning_output_tokens: 25,
            total_tokens: 375,
        },
        last_token_usage: TokenUsage {
            input_tokens: 10,
            cached_input_tokens: 0,
            output_tokens: 20,
            reasoning_output_tokens: 5,
            total_tokens: 35,
        },
        model_context_window: Some(2_000),
    };

    rollout_items.push(RolloutItem::EventMsg(EventMsg::TokenCount(
        TokenCountEvent {
            info: Some(info1),
            rate_limits: None,
        },
    )));
    rollout_items.push(RolloutItem::EventMsg(EventMsg::TokenCount(
        TokenCountEvent {
            info: None,
            rate_limits: None,
        },
    )));
    rollout_items.push(RolloutItem::EventMsg(EventMsg::TokenCount(
        TokenCountEvent {
            info: Some(info2.clone()),
            rate_limits: None,
        },
    )));
    rollout_items.push(RolloutItem::EventMsg(EventMsg::TokenCount(
        TokenCountEvent {
            info: None,
            rate_limits: None,
        },
    )));

    session
        .record_initial_history(InitialHistory::Resumed(ResumedHistory {
            conversation_id: ThreadId::default(),
            history: rollout_items,
            rollout_path: PathBuf::from("/tmp/resume.jsonl"),
        }))
        .await;

    let actual = session.state.lock().await.token_info();
    assert_eq!(actual, Some(info2));
}

#[tokio::test]
async fn recompute_token_usage_uses_session_base_instructions() {
    let (session, turn_context) = make_session_and_context().await;

    let override_instructions = "SESSION_OVERRIDE_INSTRUCTIONS_ONLY".repeat(120);
    {
        let mut state = session.state.lock().await;
        state.session_configuration.base_instructions = override_instructions.clone();
    }

    let item = user_message("hello");
    session
        .record_into_history(std::slice::from_ref(&item), &turn_context)
        .await;

    let history = session.clone_history().await;
    let session_base_instructions = BaseInstructions {
        text: override_instructions,
    };
    let expected_tokens = history
        .estimate_token_count_with_base_instructions(&session_base_instructions)
        .expect("estimate with session base instructions");
    let model_estimated_tokens = history
        .estimate_token_count(&turn_context)
        .expect("estimate with model instructions");
    assert_ne!(expected_tokens, model_estimated_tokens);

    session.recompute_token_usage(&turn_context).await;

    let actual_tokens = session
        .state
        .lock()
        .await
        .token_info()
        .expect("token info")
        .last_token_usage
        .total_tokens;
    assert_eq!(actual_tokens, expected_tokens.max(0));
}

#[tokio::test]
async fn recompute_token_usage_updates_model_context_window() {
    let (session, mut turn_context) = make_session_and_context().await;

    {
        let mut state = session.state.lock().await;
        state.set_token_info(Some(TokenUsageInfo {
            total_token_usage: TokenUsage::default(),
            last_token_usage: TokenUsage::default(),
            model_context_window: Some(258_400),
        }));
    }

    turn_context.model_info.context_window = Some(128_000);
    turn_context.model_info.effective_context_window_percent = 100;

    session.recompute_token_usage(&turn_context).await;

    let actual = session.state.lock().await.token_info().expect("token info");
    assert_eq!(actual.model_context_window, Some(128_000));
}

#[test]
fn prompt_gc_builtin_prompt_is_summary_only() {
    assert!(crate::client_common::PROMPT_GC_PROMPT.starts_with("<!--"));
    assert!(crate::client_common::PROMPT_GC_PROMPT.contains("contract=prompt_gc_summary_v1"));
    assert!(crate::client_common::PROMPT_GC_PROMPT.contains("Return JSON only"));
    assert!(crate::client_common::PROMPT_GC_PROMPT.contains("Summarize only the chunks"));
    assert!(!crate::client_common::PROMPT_GC_PROMPT.contains("retrieve/apply loop"));
    assert!(!crate::client_common::PROMPT_GC_PROMPT.contains("manage_context"));
}

#[tokio::test]
async fn record_initial_history_reconstructs_forked_transcript() {
    let (session, turn_context) = make_session_and_context().await;
    let (rollout_items, mut expected) = sample_rollout(&session, &turn_context).await;

    session
        .record_initial_history(InitialHistory::Forked(rollout_items))
        .await;

    let reconstruction_turn = session.new_default_turn().await;
    expected.extend(
        session
            .build_initial_context(reconstruction_turn.as_ref())
            .await,
    );
    let history = session.state.lock().await.clone_history();
    assert_eq!(expected, history.raw_items());
}

#[tokio::test]
async fn record_initial_history_forked_hydrates_previous_turn_settings() {
    let (session, turn_context) = make_session_and_context().await;
    let previous_model = "forked-rollout-model";
    let previous_context_item = TurnContextItem {
        turn_id: Some(turn_context.sub_id.clone()),
        trace_id: turn_context.trace_id.clone(),
        cwd: turn_context.cwd.clone(),
        current_date: turn_context.current_date.clone(),
        timezone: turn_context.timezone.clone(),
        approval_policy: turn_context.approval_policy.value(),
        sandbox_policy: turn_context.sandbox_policy.get().clone(),
        network: None,
        model: previous_model.to_string(),
        personality: turn_context.personality,
        collaboration_mode: Some(turn_context.collaboration_mode.clone()),
        realtime_active: Some(turn_context.realtime_active),
        effort: turn_context.reasoning_effort,
        summary: turn_context.reasoning_summary,
        user_instructions: None,
        developer_instructions: None,
        final_output_json_schema: None,
        truncation_policy: Some(turn_context.truncation_policy.into()),
    };
    let turn_id = previous_context_item
        .turn_id
        .clone()
        .expect("turn context should have turn_id");
    let rollout_items = vec![
        RolloutItem::EventMsg(EventMsg::TurnStarted(
            codex_protocol::protocol::TurnStartedEvent {
                turn_id: turn_id.clone(),
                model_context_window: Some(128_000),
                collaboration_mode_kind: ModeKind::Default,
            },
        )),
        RolloutItem::EventMsg(EventMsg::UserMessage(
            codex_protocol::protocol::UserMessageEvent {
                message: "forked seed".to_string(),
                images: None,
                local_images: Vec::new(),
                text_elements: Vec::new(),
            },
        )),
        RolloutItem::TurnContext(previous_context_item),
        RolloutItem::EventMsg(EventMsg::TurnComplete(
            codex_protocol::protocol::TurnCompleteEvent {
                turn_id,
                last_agent_message: None,
            },
        )),
    ];

    session
        .record_initial_history(InitialHistory::Forked(rollout_items))
        .await;

    assert_eq!(
        session.previous_turn_settings().await,
        Some(PreviousTurnSettings {
            model: previous_model.to_string(),
            realtime_active: Some(turn_context.realtime_active),
        })
    );
}

#[tokio::test]
async fn thread_rollback_drops_last_turn_from_history() {
    let (sess, tc, rx) = make_session_and_context_with_rx().await;
    let rollout_path = attach_rollout_recorder(&sess).await;

    let initial_context = sess.build_initial_context(tc.as_ref()).await;
    let turn_1 = vec![
        user_message("turn 1 user"),
        assistant_message("turn 1 assistant"),
    ];
    let turn_2 = vec![
        user_message("turn 2 user"),
        assistant_message("turn 2 assistant"),
    ];
    let mut full_history = Vec::new();
    full_history.extend(initial_context.clone());
    full_history.extend(turn_1.clone());
    full_history.extend(turn_2);
    sess.replace_history(full_history.clone(), Some(tc.to_turn_context_item()))
        .await;
    let rollout_items: Vec<RolloutItem> = full_history
        .into_iter()
        .map(RolloutItem::ResponseItem)
        .collect();
    sess.persist_rollout_items(&rollout_items).await;
    sess.set_previous_turn_settings(Some(PreviousTurnSettings {
        model: "stale-model".to_string(),
        realtime_active: Some(tc.realtime_active),
    }))
    .await;
    {
        let mut state = sess.state.lock().await;
        state.set_reference_context_item(Some(tc.to_turn_context_item()));
    }

    handlers::thread_rollback(&sess, "sub-1".to_string(), 1).await;

    let rollback_event = wait_for_thread_rolled_back(&rx).await;
    assert_eq!(rollback_event.num_turns, 1);

    let mut expected = Vec::new();
    expected.extend(initial_context);
    expected.extend(turn_1);

    let history = sess.clone_history().await;
    assert_eq!(expected, history.raw_items());
    assert_eq!(sess.previous_turn_settings().await, None);
    assert!(sess.reference_context_item().await.is_none());

    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    assert!(resumed.history.iter().any(|item| {
        matches!(
            item,
            RolloutItem::EventMsg(EventMsg::ThreadRolledBack(rollback))
            if rollback.num_turns == 1
        )
    }));
}

#[tokio::test]
async fn thread_rollback_clears_history_when_num_turns_exceeds_existing_turns() {
    let (sess, tc, rx) = make_session_and_context_with_rx().await;
    attach_rollout_recorder(&sess).await;

    let initial_context = sess.build_initial_context(tc.as_ref()).await;
    let turn_1 = vec![user_message("turn 1 user")];
    let mut full_history = Vec::new();
    full_history.extend(initial_context.clone());
    full_history.extend(turn_1);
    sess.replace_history(full_history.clone(), Some(tc.to_turn_context_item()))
        .await;
    let rollout_items: Vec<RolloutItem> = full_history
        .into_iter()
        .map(RolloutItem::ResponseItem)
        .collect();
    sess.persist_rollout_items(&rollout_items).await;

    handlers::thread_rollback(&sess, "sub-1".to_string(), 99).await;

    let rollback_event = wait_for_thread_rolled_back(&rx).await;
    assert_eq!(rollback_event.num_turns, 99);

    let history = sess.clone_history().await;
    assert_eq!(initial_context, history.raw_items());
}

#[tokio::test]
async fn thread_rollback_fails_without_persisted_rollout_path() {
    let (sess, tc, rx) = make_session_and_context_with_rx().await;

    let initial_context = sess.build_initial_context(tc.as_ref()).await;
    sess.record_into_history(&initial_context, tc.as_ref())
        .await;

    handlers::thread_rollback(&sess, "sub-1".to_string(), 1).await;

    let error_event = wait_for_thread_rollback_failed(&rx).await;
    assert_eq!(
        error_event.message,
        "thread rollback requires a persisted rollout path"
    );
    assert_eq!(
        error_event.codex_error_info,
        Some(CodexErrorInfo::ThreadRollbackFailed)
    );
    assert_eq!(sess.clone_history().await.raw_items(), initial_context);
}

#[tokio::test]
async fn thread_rollback_recomputes_previous_turn_settings_and_reference_context_from_replay() {
    let (sess, tc, rx) = make_session_and_context_with_rx().await;
    attach_rollout_recorder(&sess).await;

    let first_context_item = tc.to_turn_context_item();
    let first_turn_id = first_context_item
        .turn_id
        .clone()
        .expect("turn context should have turn_id");
    let mut rolled_back_context_item = first_context_item.clone();
    rolled_back_context_item.turn_id = Some("rolled-back-turn".to_string());
    rolled_back_context_item.model = "rolled-back-model".to_string();
    let rolled_back_turn_id = rolled_back_context_item
        .turn_id
        .clone()
        .expect("turn context should have turn_id");
    let turn_one_user = user_message("turn 1 user");
    let turn_one_assistant = assistant_message("turn 1 assistant");
    let turn_two_user = user_message("turn 2 user");
    let turn_two_assistant = assistant_message("turn 2 assistant");

    sess.persist_rollout_items(&[
        RolloutItem::EventMsg(EventMsg::TurnStarted(
            codex_protocol::protocol::TurnStartedEvent {
                turn_id: first_turn_id.clone(),
                model_context_window: Some(128_000),
                collaboration_mode_kind: ModeKind::Default,
            },
        )),
        RolloutItem::EventMsg(EventMsg::UserMessage(
            codex_protocol::protocol::UserMessageEvent {
                message: "turn 1 user".to_string(),
                images: None,
                local_images: Vec::new(),
                text_elements: Vec::new(),
            },
        )),
        RolloutItem::TurnContext(first_context_item.clone()),
        RolloutItem::ResponseItem(turn_one_user.clone()),
        RolloutItem::ResponseItem(turn_one_assistant.clone()),
        RolloutItem::EventMsg(EventMsg::TurnComplete(TurnCompleteEvent {
            turn_id: first_turn_id,
            last_agent_message: None,
        })),
        RolloutItem::EventMsg(EventMsg::TurnStarted(
            codex_protocol::protocol::TurnStartedEvent {
                turn_id: rolled_back_turn_id.clone(),
                model_context_window: Some(128_000),
                collaboration_mode_kind: ModeKind::Default,
            },
        )),
        RolloutItem::EventMsg(EventMsg::UserMessage(
            codex_protocol::protocol::UserMessageEvent {
                message: "turn 2 user".to_string(),
                images: None,
                local_images: Vec::new(),
                text_elements: Vec::new(),
            },
        )),
        RolloutItem::TurnContext(rolled_back_context_item),
        RolloutItem::ResponseItem(turn_two_user),
        RolloutItem::ResponseItem(turn_two_assistant),
        RolloutItem::EventMsg(EventMsg::TurnComplete(TurnCompleteEvent {
            turn_id: rolled_back_turn_id,
            last_agent_message: None,
        })),
    ])
    .await;
    sess.replace_history(
        vec![assistant_message("stale history")],
        Some(first_context_item.clone()),
    )
    .await;
    sess.set_previous_turn_settings(Some(PreviousTurnSettings {
        model: "stale-model".to_string(),
        realtime_active: None,
    }))
    .await;

    handlers::thread_rollback(&sess, "sub-1".to_string(), 1).await;
    let rollback_event = wait_for_thread_rolled_back(&rx).await;
    assert_eq!(rollback_event.num_turns, 1);

    assert_eq!(
        sess.clone_history().await.raw_items(),
        vec![turn_one_user, turn_one_assistant]
    );
    assert_eq!(
        sess.previous_turn_settings().await,
        Some(PreviousTurnSettings {
            model: tc.model_info.slug.clone(),
            realtime_active: Some(tc.realtime_active),
        })
    );
    assert_eq!(
        serde_json::to_value(sess.reference_context_item().await)
            .expect("serialize replay reference context item"),
        serde_json::to_value(Some(first_context_item))
            .expect("serialize expected reference context item")
    );
}

#[tokio::test]
async fn thread_rollback_restores_cleared_reference_context_item_after_compaction() {
    let (sess, tc, rx) = make_session_and_context_with_rx().await;
    attach_rollout_recorder(&sess).await;

    let first_context_item = tc.to_turn_context_item();
    let first_turn_id = first_context_item
        .turn_id
        .clone()
        .expect("turn context should have turn_id");
    let compact_turn_id = "compact-turn".to_string();
    let rolled_back_turn_id = "rolled-back-turn".to_string();
    let compacted_history = vec![
        user_message("turn 1 user"),
        user_message("summary after compaction"),
    ];

    sess.persist_rollout_items(&[
        RolloutItem::EventMsg(EventMsg::TurnStarted(
            codex_protocol::protocol::TurnStartedEvent {
                turn_id: first_turn_id.clone(),
                model_context_window: Some(128_000),
                collaboration_mode_kind: ModeKind::Default,
            },
        )),
        RolloutItem::EventMsg(EventMsg::UserMessage(UserMessageEvent {
            message: "turn 1 user".to_string(),
            images: None,
            local_images: Vec::new(),
            text_elements: Vec::new(),
        })),
        RolloutItem::TurnContext(first_context_item.clone()),
        RolloutItem::ResponseItem(user_message("turn 1 user")),
        RolloutItem::ResponseItem(assistant_message("turn 1 assistant")),
        RolloutItem::EventMsg(EventMsg::TurnComplete(TurnCompleteEvent {
            turn_id: first_turn_id,
            last_agent_message: None,
        })),
        RolloutItem::EventMsg(EventMsg::TurnStarted(
            codex_protocol::protocol::TurnStartedEvent {
                turn_id: compact_turn_id.clone(),
                model_context_window: Some(128_000),
                collaboration_mode_kind: ModeKind::Default,
            },
        )),
        RolloutItem::Compacted(CompactedItem {
            message: "summary after compaction".to_string(),
            replacement_history: Some(compacted_history.clone()),
            prompt_gc: None,
        }),
        RolloutItem::EventMsg(EventMsg::TurnComplete(TurnCompleteEvent {
            turn_id: compact_turn_id,
            last_agent_message: None,
        })),
        RolloutItem::EventMsg(EventMsg::TurnStarted(
            codex_protocol::protocol::TurnStartedEvent {
                turn_id: rolled_back_turn_id.clone(),
                model_context_window: Some(128_000),
                collaboration_mode_kind: ModeKind::Default,
            },
        )),
        RolloutItem::EventMsg(EventMsg::UserMessage(UserMessageEvent {
            message: "turn 2 user".to_string(),
            images: None,
            local_images: Vec::new(),
            text_elements: Vec::new(),
        })),
        RolloutItem::TurnContext(TurnContextItem {
            turn_id: Some(rolled_back_turn_id.clone()),
            model: "rolled-back-model".to_string(),
            ..first_context_item.clone()
        }),
        RolloutItem::ResponseItem(user_message("turn 2 user")),
        RolloutItem::ResponseItem(assistant_message("turn 2 assistant")),
        RolloutItem::EventMsg(EventMsg::TurnComplete(TurnCompleteEvent {
            turn_id: rolled_back_turn_id,
            last_agent_message: None,
        })),
    ])
    .await;
    sess.replace_history(
        vec![assistant_message("stale history")],
        Some(first_context_item),
    )
    .await;

    handlers::thread_rollback(&sess, "sub-1".to_string(), 1).await;
    let rollback_event = wait_for_thread_rolled_back(&rx).await;
    assert_eq!(rollback_event.num_turns, 1);

    assert_eq!(sess.clone_history().await.raw_items(), compacted_history);
    assert!(sess.reference_context_item().await.is_none());
}

#[tokio::test]
async fn thread_rollback_persists_marker_and_replays_cumulatively() {
    let (sess, tc, rx) = make_session_and_context_with_rx().await;
    let rollout_path = attach_rollout_recorder(&sess).await;
    let turn_context_item = tc.to_turn_context_item();

    sess.persist_rollout_items(&[
        RolloutItem::EventMsg(EventMsg::TurnStarted(
            codex_protocol::protocol::TurnStartedEvent {
                turn_id: "turn-1".to_string(),
                model_context_window: Some(128_000),
                collaboration_mode_kind: ModeKind::Default,
            },
        )),
        RolloutItem::EventMsg(EventMsg::UserMessage(UserMessageEvent {
            message: "turn 1 user".to_string(),
            images: None,
            local_images: Vec::new(),
            text_elements: Vec::new(),
        })),
        RolloutItem::TurnContext(turn_context_item.clone()),
        RolloutItem::ResponseItem(user_message("turn 1 user")),
        RolloutItem::ResponseItem(assistant_message("turn 1 assistant")),
        RolloutItem::EventMsg(EventMsg::TurnComplete(TurnCompleteEvent {
            turn_id: "turn-1".to_string(),
            last_agent_message: None,
        })),
        RolloutItem::EventMsg(EventMsg::TurnStarted(
            codex_protocol::protocol::TurnStartedEvent {
                turn_id: "turn-2".to_string(),
                model_context_window: Some(128_000),
                collaboration_mode_kind: ModeKind::Default,
            },
        )),
        RolloutItem::EventMsg(EventMsg::UserMessage(UserMessageEvent {
            message: "turn 2 user".to_string(),
            images: None,
            local_images: Vec::new(),
            text_elements: Vec::new(),
        })),
        RolloutItem::TurnContext(turn_context_item.clone()),
        RolloutItem::ResponseItem(user_message("turn 2 user")),
        RolloutItem::ResponseItem(assistant_message("turn 2 assistant")),
        RolloutItem::EventMsg(EventMsg::TurnComplete(TurnCompleteEvent {
            turn_id: "turn-2".to_string(),
            last_agent_message: None,
        })),
        RolloutItem::EventMsg(EventMsg::TurnStarted(
            codex_protocol::protocol::TurnStartedEvent {
                turn_id: "turn-3".to_string(),
                model_context_window: Some(128_000),
                collaboration_mode_kind: ModeKind::Default,
            },
        )),
        RolloutItem::EventMsg(EventMsg::UserMessage(UserMessageEvent {
            message: "turn 3 user".to_string(),
            images: None,
            local_images: Vec::new(),
            text_elements: Vec::new(),
        })),
        RolloutItem::TurnContext(turn_context_item),
        RolloutItem::ResponseItem(user_message("turn 3 user")),
        RolloutItem::ResponseItem(assistant_message("turn 3 assistant")),
        RolloutItem::EventMsg(EventMsg::TurnComplete(TurnCompleteEvent {
            turn_id: "turn-3".to_string(),
            last_agent_message: None,
        })),
    ])
    .await;

    handlers::thread_rollback(&sess, "sub-1".to_string(), 1).await;
    let first_rollback = wait_for_thread_rolled_back(&rx).await;
    assert_eq!(first_rollback.num_turns, 1);
    handlers::thread_rollback(&sess, "sub-1".to_string(), 1).await;
    let second_rollback = wait_for_thread_rolled_back(&rx).await;
    assert_eq!(second_rollback.num_turns, 1);

    assert_eq!(
        sess.clone_history().await.raw_items(),
        vec![
            user_message("turn 1 user"),
            assistant_message("turn 1 assistant")
        ]
    );

    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let rollback_markers = resumed
        .history
        .iter()
        .filter(|item| matches!(item, RolloutItem::EventMsg(EventMsg::ThreadRolledBack(_))))
        .count();
    assert_eq!(rollback_markers, 2);
}

#[tokio::test]
async fn thread_rollback_fails_when_turn_in_progress() {
    let (sess, tc, rx) = make_session_and_context_with_rx().await;

    let initial_context = sess.build_initial_context(tc.as_ref()).await;
    sess.record_into_history(&initial_context, tc.as_ref())
        .await;

    *sess.active_turn.lock().await = Some(crate::state::ActiveTurn::default());
    handlers::thread_rollback(&sess, "sub-1".to_string(), 1).await;

    let error_event = wait_for_thread_rollback_failed(&rx).await;
    assert_eq!(
        error_event.codex_error_info,
        Some(CodexErrorInfo::ThreadRollbackFailed)
    );

    let history = sess.clone_history().await;
    assert_eq!(initial_context, history.raw_items());
}

#[tokio::test]
async fn thread_rollback_fails_when_num_turns_is_zero() {
    let (sess, tc, rx) = make_session_and_context_with_rx().await;

    let initial_context = sess.build_initial_context(tc.as_ref()).await;
    sess.record_into_history(&initial_context, tc.as_ref())
        .await;

    handlers::thread_rollback(&sess, "sub-1".to_string(), 0).await;

    let error_event = wait_for_thread_rollback_failed(&rx).await;
    assert_eq!(error_event.message, "num_turns must be >= 1");
    assert_eq!(
        error_event.codex_error_info,
        Some(CodexErrorInfo::ThreadRollbackFailed)
    );

    let history = sess.clone_history().await;
    assert_eq!(initial_context, history.raw_items());
}

#[tokio::test]
async fn set_rate_limits_retains_previous_credits() {
    let codex_home = tempfile::tempdir().expect("create temp dir");
    let config = build_test_config(codex_home.path()).await;
    let config = Arc::new(config);
    let model = ModelsManager::get_model_offline_for_tests(config.model.as_deref());
    let model_info = ModelsManager::construct_model_info_offline_for_tests(model.as_str(), &config);
    let reasoning_effort = config.model_reasoning_effort;
    let collaboration_mode = CollaborationMode {
        mode: ModeKind::Default,
        settings: Settings {
            model,
            reasoning_effort,
            developer_instructions: None,
        },
    };
    let session_configuration = SessionConfiguration {
        provider: config.model_provider.clone(),
        collaboration_mode,
        collaboration_mode_reasoning_effort_explicit: false,
        model_reasoning_summary: config.model_reasoning_summary,
        developer_instructions: config.developer_instructions.clone(),
        user_instructions: config.user_instructions.clone(),
        service_tier: None,
        personality: config.personality,
        base_instructions: config
            .base_instructions
            .clone()
            .unwrap_or_else(|| model_info.get_model_instructions(config.personality)),
        compact_prompt: config.compact_prompt.clone(),
        approval_policy: config.permissions.approval_policy.clone(),
        approvals_reviewer: config.approvals_reviewer,
        sandbox_policy: config.permissions.sandbox_policy.clone(),
        file_system_sandbox_policy: config.permissions.file_system_sandbox_policy.clone(),
        network_sandbox_policy: config.permissions.network_sandbox_policy,
        windows_sandbox_level: WindowsSandboxLevel::from_config(&config),
        cwd: config.cwd.clone(),
        config_path: config
            .active_user_config_path()
            .expect("active user config path"),
        codex_home: config.codex_home.clone(),
        thread_name: None,
        original_config_do_not_use: Arc::clone(&config),
        metrics_service_name: None,
        app_server_client_name: None,
        session_source: SessionSource::Exec,
        dynamic_tools: Vec::new(),
        persist_extended_history: false,
        inherited_shell_snapshot: None,
        user_shell_override: None,
    };

    let mut state = SessionState::new(session_configuration);
    let initial = RateLimitSnapshot {
        limit_id: None,
        limit_name: None,
        primary: Some(RateLimitWindow {
            used_percent: 10.0,
            window_minutes: Some(15),
            resets_at: Some(1_700),
        }),
        secondary: None,
        credits: Some(CreditsSnapshot {
            has_credits: true,
            unlimited: false,
            balance: Some("10.00".to_string()),
        }),
        plan_type: Some(codex_protocol::account::PlanType::Plus),
    };
    state.set_rate_limits(initial.clone());

    let update = RateLimitSnapshot {
        limit_id: Some("codex_other".to_string()),
        limit_name: Some("codex_other".to_string()),
        primary: Some(RateLimitWindow {
            used_percent: 40.0,
            window_minutes: Some(30),
            resets_at: Some(1_800),
        }),
        secondary: Some(RateLimitWindow {
            used_percent: 5.0,
            window_minutes: Some(60),
            resets_at: Some(1_900),
        }),
        credits: None,
        plan_type: None,
    };
    state.set_rate_limits(update.clone());

    assert_eq!(
        state.latest_rate_limits,
        Some(RateLimitSnapshot {
            limit_id: Some("codex_other".to_string()),
            limit_name: Some("codex_other".to_string()),
            primary: update.primary.clone(),
            secondary: update.secondary,
            credits: initial.credits,
            plan_type: initial.plan_type,
        })
    );
}

#[tokio::test]
async fn set_rate_limits_updates_plan_type_when_present() {
    let codex_home = tempfile::tempdir().expect("create temp dir");
    let config = build_test_config(codex_home.path()).await;
    let config = Arc::new(config);
    let model = ModelsManager::get_model_offline_for_tests(config.model.as_deref());
    let model_info = ModelsManager::construct_model_info_offline_for_tests(model.as_str(), &config);
    let reasoning_effort = config.model_reasoning_effort;
    let collaboration_mode = CollaborationMode {
        mode: ModeKind::Default,
        settings: Settings {
            model,
            reasoning_effort,
            developer_instructions: None,
        },
    };
    let session_configuration = SessionConfiguration {
        provider: config.model_provider.clone(),
        collaboration_mode,
        collaboration_mode_reasoning_effort_explicit: false,
        model_reasoning_summary: config.model_reasoning_summary,
        developer_instructions: config.developer_instructions.clone(),
        user_instructions: config.user_instructions.clone(),
        service_tier: None,
        personality: config.personality,
        base_instructions: config
            .base_instructions
            .clone()
            .unwrap_or_else(|| model_info.get_model_instructions(config.personality)),
        compact_prompt: config.compact_prompt.clone(),
        approval_policy: config.permissions.approval_policy.clone(),
        approvals_reviewer: config.approvals_reviewer,
        sandbox_policy: config.permissions.sandbox_policy.clone(),
        file_system_sandbox_policy: config.permissions.file_system_sandbox_policy.clone(),
        network_sandbox_policy: config.permissions.network_sandbox_policy,
        windows_sandbox_level: WindowsSandboxLevel::from_config(&config),
        cwd: config.cwd.clone(),
        config_path: config
            .active_user_config_path()
            .expect("active user config path"),
        codex_home: config.codex_home.clone(),
        thread_name: None,
        original_config_do_not_use: Arc::clone(&config),
        metrics_service_name: None,
        app_server_client_name: None,
        session_source: SessionSource::Exec,
        dynamic_tools: Vec::new(),
        persist_extended_history: false,
        inherited_shell_snapshot: None,
        user_shell_override: None,
    };

    let mut state = SessionState::new(session_configuration);
    let initial = RateLimitSnapshot {
        limit_id: None,
        limit_name: None,
        primary: Some(RateLimitWindow {
            used_percent: 15.0,
            window_minutes: Some(20),
            resets_at: Some(1_600),
        }),
        secondary: Some(RateLimitWindow {
            used_percent: 5.0,
            window_minutes: Some(45),
            resets_at: Some(1_650),
        }),
        credits: Some(CreditsSnapshot {
            has_credits: true,
            unlimited: false,
            balance: Some("15.00".to_string()),
        }),
        plan_type: Some(codex_protocol::account::PlanType::Plus),
    };
    state.set_rate_limits(initial.clone());

    let update = RateLimitSnapshot {
        limit_id: None,
        limit_name: None,
        primary: Some(RateLimitWindow {
            used_percent: 35.0,
            window_minutes: Some(25),
            resets_at: Some(1_700),
        }),
        secondary: None,
        credits: None,
        plan_type: Some(codex_protocol::account::PlanType::Pro),
    };
    state.set_rate_limits(update.clone());

    assert_eq!(
        state.latest_rate_limits,
        Some(RateLimitSnapshot {
            limit_id: Some("codex".to_string()),
            limit_name: None,
            primary: update.primary,
            secondary: update.secondary,
            credits: initial.credits,
            plan_type: update.plan_type,
        })
    );
}

#[test]
fn prefers_structured_content_when_present() {
    let ctr = McpCallToolResult {
        // Content present but should be ignored because structured_content is set.
        content: vec![text_block("ignored")],
        is_error: None,
        structured_content: Some(json!({
            "ok": true,
            "value": 42
        })),
        meta: None,
    };

    let got = ctr.into_function_call_output_payload();
    let expected = FunctionCallOutputPayload {
        body: FunctionCallOutputBody::Text(
            serde_json::to_string(&json!({
                "ok": true,
                "value": 42
            }))
            .unwrap(),
        ),
        success: Some(true),
    };

    assert_eq!(expected, got);
}

#[tokio::test]
async fn includes_timed_out_message() {
    let exec = ExecToolCallOutput {
        exit_code: 0,
        stdout: StreamOutput::new(String::new()),
        stderr: StreamOutput::new(String::new()),
        aggregated_output: StreamOutput::new("Command output".to_string()),
        duration: StdDuration::from_secs(1),
        timed_out: true,
    };
    let (_, turn_context) = make_session_and_context().await;

    let out = format_exec_output_str(&exec, turn_context.truncation_policy);

    assert_eq!(
        out,
        "command timed out after 1000 milliseconds\nCommand output"
    );
}

#[tokio::test]
async fn turn_context_with_model_updates_model_fields() {
    let (session, mut turn_context) = make_session_and_context().await;
    turn_context.reasoning_effort = Some(ReasoningEffortConfig::Minimal);
    let updated = turn_context
        .with_model("gpt-5.1".to_string(), &session.services.models_manager)
        .await;
    let expected_model_info = session
        .services
        .models_manager
        .get_model_info("gpt-5.1", updated.config.as_ref())
        .await;

    assert_eq!(updated.config.model.as_deref(), Some("gpt-5.1"));
    assert_eq!(updated.collaboration_mode.model(), "gpt-5.1");
    assert_eq!(updated.model_info, expected_model_info);
    assert_eq!(
        updated.reasoning_effort,
        Some(ReasoningEffortConfig::Medium)
    );
    assert_eq!(
        updated.collaboration_mode.reasoning_effort(),
        Some(ReasoningEffortConfig::Medium)
    );
    assert_eq!(
        updated.config.model_reasoning_effort,
        Some(ReasoningEffortConfig::Medium)
    );
    assert_eq!(
        updated.truncation_policy,
        expected_model_info.truncation_policy.into()
    );
    assert!(!Arc::ptr_eq(
        &updated.tool_call_gate,
        &turn_context.tool_call_gate
    ));
}

#[test]
fn falls_back_to_content_when_structured_is_null() {
    let ctr = McpCallToolResult {
        content: vec![text_block("hello"), text_block("world")],
        is_error: None,
        structured_content: Some(serde_json::Value::Null),
        meta: None,
    };

    let got = ctr.into_function_call_output_payload();
    let expected = FunctionCallOutputPayload {
        body: FunctionCallOutputBody::Text(
            serde_json::to_string(&vec![text_block("hello"), text_block("world")]).unwrap(),
        ),
        success: Some(true),
    };

    assert_eq!(expected, got);
}

#[test]
fn success_flag_reflects_is_error_true() {
    let ctr = McpCallToolResult {
        content: vec![text_block("unused")],
        is_error: Some(true),
        structured_content: Some(json!({ "message": "bad" })),
        meta: None,
    };

    let got = ctr.into_function_call_output_payload();
    let expected = FunctionCallOutputPayload {
        body: FunctionCallOutputBody::Text(
            serde_json::to_string(&json!({ "message": "bad" })).unwrap(),
        ),
        success: Some(false),
    };

    assert_eq!(expected, got);
}

#[test]
fn success_flag_true_with_no_error_and_content_used() {
    let ctr = McpCallToolResult {
        content: vec![text_block("alpha")],
        is_error: Some(false),
        structured_content: None,
        meta: None,
    };

    let got = ctr.into_function_call_output_payload();
    let expected = FunctionCallOutputPayload {
        body: FunctionCallOutputBody::Text(
            serde_json::to_string(&vec![text_block("alpha")]).unwrap(),
        ),
        success: Some(true),
    };

    assert_eq!(expected, got);
}

async fn wait_for_thread_rolled_back(
    rx: &async_channel::Receiver<Event>,
) -> crate::protocol::ThreadRolledBackEvent {
    let deadline = StdDuration::from_secs(2);
    let start = std::time::Instant::now();
    loop {
        let remaining = deadline.saturating_sub(start.elapsed());
        let evt = tokio::time::timeout(remaining, rx.recv())
            .await
            .expect("timeout waiting for event")
            .expect("event");
        match evt.msg {
            EventMsg::ThreadRolledBack(payload) => return payload,
            _ => continue,
        }
    }
}

async fn wait_for_thread_rollback_failed(rx: &async_channel::Receiver<Event>) -> ErrorEvent {
    let deadline = StdDuration::from_secs(2);
    let start = std::time::Instant::now();
    loop {
        let remaining = deadline.saturating_sub(start.elapsed());
        let evt = tokio::time::timeout(remaining, rx.recv())
            .await
            .expect("timeout waiting for event")
            .expect("event");
        match evt.msg {
            EventMsg::Error(payload)
                if payload.codex_error_info == Some(CodexErrorInfo::ThreadRollbackFailed) =>
            {
                return payload;
            }
            _ => continue,
        }
    }
}

async fn attach_rollout_recorder(session: &Arc<Session>) -> PathBuf {
    let config = session.get_config().await;
    let recorder = RolloutRecorder::new(
        config.as_ref(),
        RolloutRecorderParams::new(
            ThreadId::default(),
            None,
            SessionSource::Exec,
            BaseInstructions::default(),
            Vec::new(),
            EventPersistenceMode::Limited,
        ),
        None,
        None,
    )
    .await
    .expect("create rollout recorder");
    let rollout_path = recorder.rollout_path().to_path_buf();
    {
        let mut rollout = session.services.rollout.lock().await;
        *rollout = Some(recorder);
    }
    session.ensure_rollout_materialized().await;
    session.flush_rollout().await;
    rollout_path
}

async fn prompt_gc_rollout_markers(rollout_path: &Path) -> Vec<PromptGcCompactionMetadata> {
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };

    resumed
        .history
        .into_iter()
        .filter_map(|item| match item {
            RolloutItem::Compacted(compacted) => compacted.prompt_gc,
            _ => None,
        })
        .collect()
}

fn text_block(s: &str) -> serde_json::Value {
    json!({
        "type": "text",
        "text": s,
    })
}

fn init_test_tracing() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let provider = SdkTracerProvider::builder().build();
        let tracer = provider.tracer("codex-core-tests");
        let subscriber =
            tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(tracer));
        tracing::subscriber::set_global_default(subscriber)
            .expect("global tracing subscriber should only be installed once");
    });
}

async fn build_test_config(codex_home: &Path) -> Config {
    ConfigBuilder::default()
        .codex_home(codex_home.to_path_buf())
        .build()
        .await
        .expect("load default test config")
}

fn session_telemetry(
    conversation_id: ThreadId,
    config: &Config,
    model_info: &ModelInfo,
    session_source: SessionSource,
) -> SessionTelemetry {
    SessionTelemetry::new(
        conversation_id,
        ModelsManager::get_model_offline_for_tests(config.model.as_deref()).as_str(),
        model_info.slug.as_str(),
        None,
        Some("test@test.com".to_string()),
        Some(TelemetryAuthMode::Chatgpt),
        "test_originator".to_string(),
        false,
        "test".to_string(),
        session_source,
    )
}

pub(crate) async fn make_session_configuration_for_tests() -> SessionConfiguration {
    let codex_home = tempfile::tempdir().expect("create temp dir");
    let config = build_test_config(codex_home.path()).await;
    let config = Arc::new(config);
    let model = ModelsManager::get_model_offline_for_tests(config.model.as_deref());
    let model_info = ModelsManager::construct_model_info_offline_for_tests(model.as_str(), &config);
    let reasoning_effort = config.model_reasoning_effort;
    let collaboration_mode = CollaborationMode {
        mode: ModeKind::Default,
        settings: Settings {
            model,
            reasoning_effort,
            developer_instructions: None,
        },
    };

    SessionConfiguration {
        provider: config.model_provider.clone(),
        collaboration_mode,
        collaboration_mode_reasoning_effort_explicit: false,
        model_reasoning_summary: config.model_reasoning_summary,
        developer_instructions: config.developer_instructions.clone(),
        user_instructions: config.user_instructions.clone(),
        service_tier: None,
        personality: config.personality,
        base_instructions: config
            .base_instructions
            .clone()
            .unwrap_or_else(|| model_info.get_model_instructions(config.personality)),
        compact_prompt: config.compact_prompt.clone(),
        approval_policy: config.permissions.approval_policy.clone(),
        approvals_reviewer: config.approvals_reviewer,
        sandbox_policy: config.permissions.sandbox_policy.clone(),
        file_system_sandbox_policy: config.permissions.file_system_sandbox_policy.clone(),
        network_sandbox_policy: config.permissions.network_sandbox_policy,
        windows_sandbox_level: WindowsSandboxLevel::from_config(&config),
        cwd: config.cwd.clone(),
        config_path: config
            .active_user_config_path()
            .expect("active user config path"),
        codex_home: config.codex_home.clone(),
        thread_name: None,
        original_config_do_not_use: Arc::clone(&config),
        metrics_service_name: None,
        app_server_client_name: None,
        session_source: SessionSource::Exec,
        dynamic_tools: Vec::new(),
        persist_extended_history: false,
        inherited_shell_snapshot: None,
        user_shell_override: None,
    }
}

#[tokio::test]
async fn session_configuration_apply_preserves_split_file_system_policy_on_cwd_only_update() {
    let mut session_configuration = make_session_configuration_for_tests().await;
    let workspace = tempfile::tempdir().expect("create temp dir");
    let project_root = workspace.path().join("project");
    let original_cwd = project_root.join("subdir");
    let docs_dir = original_cwd.join("docs");
    std::fs::create_dir_all(&docs_dir).expect("create docs dir");
    let docs_dir =
        codex_utils_absolute_path::AbsolutePathBuf::from_absolute_path(&docs_dir).expect("docs");

    session_configuration.cwd = original_cwd;
    session_configuration.sandbox_policy =
        codex_config::Constrained::allow_any(SandboxPolicy::WorkspaceWrite {
            writable_roots: Vec::new(),
            read_only_access: ReadOnlyAccess::Restricted {
                include_platform_defaults: true,
                readable_roots: vec![docs_dir.clone()],
            },
            network_access: false,
            exclude_tmpdir_env_var: true,
            exclude_slash_tmp: true,
        });
    session_configuration.file_system_sandbox_policy = FileSystemSandboxPolicy::restricted(vec![
        FileSystemSandboxEntry {
            path: FileSystemPath::Special {
                value: FileSystemSpecialPath::CurrentWorkingDirectory,
            },
            access: FileSystemAccessMode::Write,
        },
        FileSystemSandboxEntry {
            path: FileSystemPath::Path { path: docs_dir },
            access: FileSystemAccessMode::Read,
        },
    ]);

    let updated = session_configuration
        .apply(&SessionSettingsUpdate {
            cwd: Some(project_root),
            ..Default::default()
        })
        .expect("cwd-only update should succeed");

    assert_eq!(
        updated.file_system_sandbox_policy,
        session_configuration.file_system_sandbox_policy
    );
}

#[tokio::test]
async fn spawn_agent_preserves_explicit_reasoning_effort_clear_in_session_config() {
    let mut session_configuration = make_session_configuration_for_tests().await;
    let mut config = (*session_configuration.original_config_do_not_use).clone();
    config.model_reasoning_effort = Some(ReasoningEffortConfig::High);
    session_configuration.original_config_do_not_use = Arc::new(config);

    let cleared_mode = session_configuration.collaboration_mode.with_updates(
        /*model*/ None,
        Some(/*effort*/ None),
        /*developer_instructions*/ None,
    );
    let updated = session_configuration
        .apply(&SessionSettingsUpdate {
            collaboration_mode: Some(cleared_mode),
            collaboration_mode_explicit: true,
            ..Default::default()
        })
        .expect("apply explicit reasoning clear");

    assert_eq!(updated.thread_config_snapshot().reasoning_effort, None);
    assert_eq!(
        Session::build_per_turn_config(&updated).model_reasoning_effort,
        None
    );
}

#[tokio::test]
async fn synthesized_user_turn_update_preserves_explicit_reasoning_effort_clear() {
    let mut session_configuration = make_session_configuration_for_tests().await;
    let mut config = (*session_configuration.original_config_do_not_use).clone();
    config.model_reasoning_effort = Some(ReasoningEffortConfig::High);
    session_configuration.original_config_do_not_use = Arc::new(config);

    let cleared_mode = session_configuration.collaboration_mode.with_updates(
        /*model*/ None,
        Some(/*effort*/ None),
        /*developer_instructions*/ None,
    );
    let cleared_configuration = session_configuration
        .apply(&SessionSettingsUpdate {
            collaboration_mode: Some(cleared_mode),
            collaboration_mode_explicit: true,
            ..Default::default()
        })
        .expect("apply explicit reasoning clear");

    let (collaboration_mode, collaboration_mode_explicit) =
        handlers::resolve_collaboration_mode_update(
            &cleared_configuration,
            Some(cleared_configuration.collaboration_mode.model().to_string()),
            /*effort*/ None,
            /*collaboration_mode*/ None,
        );
    let updated = cleared_configuration
        .apply(&SessionSettingsUpdate {
            collaboration_mode: Some(collaboration_mode),
            collaboration_mode_explicit,
            ..Default::default()
        })
        .expect("apply synthesized user-turn update");

    assert_eq!(updated.thread_config_snapshot().reasoning_effort, None);
    assert_eq!(
        Session::build_per_turn_config(&updated).model_reasoning_effort,
        None
    );
}

async fn make_synthesized_user_turn_update_with_inherited_reasoning_effort(
    inherited_reasoning_effort: ReasoningEffortConfig,
) -> SessionConfiguration {
    let mut session_configuration = make_session_configuration_for_tests().await;
    let mut config = (*session_configuration.original_config_do_not_use).clone();
    config.model_reasoning_effort = Some(inherited_reasoning_effort);
    session_configuration.original_config_do_not_use = Arc::new(config);
    session_configuration.collaboration_mode =
        session_configuration.collaboration_mode.with_updates(
            /*model*/ None,
            Some(/*effort*/ None),
            /*developer_instructions*/ None,
        );
    session_configuration.collaboration_mode_reasoning_effort_explicit = false;

    let (collaboration_mode, collaboration_mode_explicit) =
        handlers::resolve_collaboration_mode_update(
            &session_configuration,
            Some(session_configuration.collaboration_mode.model().to_string()),
            /*effort*/ None,
            /*collaboration_mode*/ None,
        );
    session_configuration
        .apply(&SessionSettingsUpdate {
            collaboration_mode: Some(collaboration_mode),
            collaboration_mode_explicit,
            ..Default::default()
        })
        .expect("apply synthesized user-turn update")
}

#[tokio::test]
async fn synthesized_user_turn_update_preserves_inherited_reasoning_effort_in_turn_context() {
    let updated = make_synthesized_user_turn_update_with_inherited_reasoning_effort(
        ReasoningEffortConfig::High,
    )
    .await;
    let config = Arc::clone(&updated.original_config_do_not_use);
    let conversation_id = ThreadId::default();
    let auth_manager = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("Test API Key"));
    let models_manager = Arc::new(ModelsManager::new(
        config.codex_home.clone(),
        auth_manager.clone(),
        None,
        CollaborationModesConfig::default(),
    ));
    let per_turn_config = Session::build_per_turn_config(&updated);
    let model_info = ModelsManager::construct_model_info_offline_for_tests(
        updated.collaboration_mode.model(),
        &per_turn_config,
    );
    let session_telemetry = session_telemetry(
        conversation_id,
        config.as_ref(),
        &model_info,
        updated.session_source.clone(),
    );
    let plugins_manager = Arc::new(PluginsManager::new(config.codex_home.clone()));
    let skills_manager = Arc::new(SkillsManager::new(
        config.codex_home.clone(),
        Arc::clone(&plugins_manager),
        true,
    ));
    let skills_outcome = Arc::new(skills_manager.skills_for_config(&per_turn_config));
    let environment = Arc::new(codex_environment::Environment);
    let js_repl = Arc::new(JsReplHandle::with_node_path(
        config.js_repl_node_path.clone(),
        config.js_repl_node_module_dirs.clone(),
    ));
    let user_shell = default_user_shell();

    let turn_context = Session::make_turn_context(
        Some(Arc::clone(&auth_manager)),
        &session_telemetry,
        updated.provider.clone(),
        &updated,
        &user_shell,
        /*shell_zsh_path*/ None,
        config.main_execve_wrapper_exe.as_ref(),
        per_turn_config,
        model_info,
        &models_manager,
        /*network*/ None,
        environment,
        "turn_id".to_string(),
        js_repl,
        skills_outcome,
    );

    assert_eq!(updated.collaboration_mode.reasoning_effort(), None);
    assert_eq!(
        updated.thread_config_snapshot().reasoning_effort,
        Some(ReasoningEffortConfig::High)
    );
    assert_eq!(turn_context.collaboration_mode.reasoning_effort(), None);
    assert_eq!(
        turn_context.reasoning_effort,
        Some(ReasoningEffortConfig::High)
    );
    assert_eq!(
        turn_context.config.model_reasoning_effort,
        Some(ReasoningEffortConfig::High)
    );
}

#[tokio::test]
async fn session_configured_event_preserves_inherited_reasoning_effort_after_synthesized_update() {
    let session_configuration = make_synthesized_user_turn_update_with_inherited_reasoning_effort(
        ReasoningEffortConfig::High,
    )
    .await;
    let config = Arc::clone(&session_configuration.original_config_do_not_use);
    let (tx_event, rx_event) = async_channel::unbounded();
    let auth_manager = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("Test API Key"));
    let models_manager = Arc::new(ModelsManager::new(
        config.codex_home.clone(),
        auth_manager.clone(),
        None,
        CollaborationModesConfig::default(),
    ));
    let plugins_manager = Arc::new(PluginsManager::new(config.codex_home.clone()));
    let mcp_manager = Arc::new(McpManager::new(Arc::clone(&plugins_manager)));
    let skills_manager = Arc::new(SkillsManager::new(
        config.codex_home.clone(),
        Arc::clone(&plugins_manager),
        true,
    ));
    let (agent_status_tx, _agent_status_rx) = watch::channel(AgentStatus::PendingInit);
    let (agent_last_activity_tx, _agent_last_activity_rx) =
        watch::channel::<Option<codex_protocol::protocol::CollabAgentActivity>>(None);
    let (prompt_gc_active_tx, _prompt_gc_active_rx) = watch::channel(false);
    let (prompt_gc_activity_edges, _) = tokio::sync::broadcast::channel(16);

    let session = Session::new(
        session_configuration,
        Arc::clone(&config),
        auth_manager,
        models_manager,
        Arc::new(ExecPolicyManager::default()),
        tx_event,
        agent_status_tx,
        agent_last_activity_tx,
        prompt_gc_active_tx,
        prompt_gc_activity_edges,
        InitialHistory::New,
        SessionSource::Exec,
        skills_manager,
        plugins_manager,
        mcp_manager,
        Arc::new(FileWatcher::noop()),
        AgentControl::default(),
    )
    .await
    .expect("session should start");

    let event = rx_event.recv().await.expect("session configured event");
    let EventMsg::SessionConfigured(session_configured) = event.msg else {
        panic!("expected SessionConfiguredEvent");
    };

    assert_eq!(
        session
            .state
            .lock()
            .await
            .session_configuration
            .thread_config_snapshot()
            .reasoning_effort,
        Some(ReasoningEffortConfig::High)
    );
    assert_eq!(
        session_configured.reasoning_effort,
        Some(ReasoningEffortConfig::High)
    );
}

#[tokio::test]
async fn synthesized_override_turn_context_update_preserves_explicit_reasoning_effort_clear() {
    let mut session_configuration = make_session_configuration_for_tests().await;
    let mut config = (*session_configuration.original_config_do_not_use).clone();
    config.model_reasoning_effort = Some(ReasoningEffortConfig::High);
    session_configuration.original_config_do_not_use = Arc::new(config);

    let cleared_mode = session_configuration.collaboration_mode.with_updates(
        /*model*/ None,
        Some(/*effort*/ None),
        /*developer_instructions*/ None,
    );
    let cleared_configuration = session_configuration
        .apply(&SessionSettingsUpdate {
            collaboration_mode: Some(cleared_mode),
            collaboration_mode_explicit: true,
            ..Default::default()
        })
        .expect("apply explicit reasoning clear");

    let (collaboration_mode, collaboration_mode_explicit) =
        handlers::resolve_collaboration_mode_update(
            &cleared_configuration,
            /*model*/ None,
            /*effort*/ None,
            /*collaboration_mode*/ None,
        );
    let updated = cleared_configuration
        .apply(&SessionSettingsUpdate {
            collaboration_mode: Some(collaboration_mode),
            collaboration_mode_explicit,
            ..Default::default()
        })
        .expect("apply synthesized override-turn-context update");

    assert_eq!(updated.thread_config_snapshot().reasoning_effort, None);
    assert_eq!(
        Session::build_per_turn_config(&updated).model_reasoning_effort,
        None
    );
}

#[cfg_attr(windows, ignore)]
#[tokio::test]
async fn new_default_turn_uses_config_aware_skills_for_role_overrides() {
    let (session, _turn_context) = make_session_and_context().await;
    let parent_config = session.get_config().await;
    let codex_home = parent_config.codex_home.clone();
    let skill_dir = codex_home.join("skills").join("demo");
    std::fs::create_dir_all(&skill_dir).expect("create skill dir");
    let skill_path = skill_dir.join("SKILL.md");
    std::fs::write(
        &skill_path,
        "---\nname: demo-skill\ndescription: demo description\n---\n\n# Body\n",
    )
    .expect("write skill");

    let parent_outcome = session
        .services
        .skills_manager
        .skills_for_cwd(&parent_config.cwd, true)
        .await;
    let parent_skill = parent_outcome
        .skills
        .iter()
        .find(|skill| skill.name == "demo-skill")
        .expect("demo skill should be discovered");
    assert_eq!(parent_outcome.is_skill_enabled(parent_skill), true);

    let role_path = codex_home.join("skills-role.toml");
    std::fs::write(
        &role_path,
        format!(
            r#"developer_instructions = "Stay focused"

[[skills.config]]
path = "{}"
enabled = false
"#,
            skill_path.display()
        ),
    )
    .expect("write role config");

    let mut child_config = (*parent_config).clone();
    child_config.agent_roles.insert(
        "custom".to_string(),
        crate::config::AgentRoleConfig {
            description: None,
            config_file: Some(role_path),
            nickname_candidates: None,
        },
    );
    crate::agent::role::apply_role_to_config(&mut child_config, Some("custom"))
        .await
        .expect("custom role should apply");

    {
        let mut state = session.state.lock().await;
        state.session_configuration.original_config_do_not_use = Arc::new(child_config);
    }

    let child_turn = session
        .new_default_turn_with_sub_id("role-skill-turn".to_string())
        .await;
    let child_skill = child_turn
        .turn_skills
        .outcome
        .skills
        .iter()
        .find(|skill| skill.name == "demo-skill")
        .expect("demo skill should be discovered");
    assert_eq!(
        child_turn.turn_skills.outcome.is_skill_enabled(child_skill),
        false
    );
}

#[tokio::test]
async fn session_configuration_apply_rederives_legacy_file_system_policy_on_cwd_update() {
    let mut session_configuration = make_session_configuration_for_tests().await;
    let workspace = tempfile::tempdir().expect("create temp dir");
    let project_root = workspace.path().join("project");
    let original_cwd = project_root.join("subdir");
    let docs_dir = original_cwd.join("docs");
    std::fs::create_dir_all(&docs_dir).expect("create docs dir");
    let docs_dir =
        codex_utils_absolute_path::AbsolutePathBuf::from_absolute_path(&docs_dir).expect("docs");

    session_configuration.cwd = original_cwd;
    session_configuration.sandbox_policy =
        codex_config::Constrained::allow_any(SandboxPolicy::WorkspaceWrite {
            writable_roots: Vec::new(),
            read_only_access: ReadOnlyAccess::Restricted {
                include_platform_defaults: true,
                readable_roots: vec![docs_dir],
            },
            network_access: false,
            exclude_tmpdir_env_var: true,
            exclude_slash_tmp: true,
        });
    session_configuration.file_system_sandbox_policy =
        FileSystemSandboxPolicy::from_legacy_sandbox_policy(
            session_configuration.sandbox_policy.get(),
            &session_configuration.cwd,
        );

    let updated = session_configuration
        .apply(&SessionSettingsUpdate {
            cwd: Some(project_root.clone()),
            ..Default::default()
        })
        .expect("cwd-only update should succeed");

    assert_eq!(
        updated.file_system_sandbox_policy,
        FileSystemSandboxPolicy::from_legacy_sandbox_policy(
            updated.sandbox_policy.get(),
            &project_root,
        )
    );
}

#[tokio::test]
async fn session_new_fails_when_zsh_fork_enabled_without_zsh_path() {
    let codex_home = tempfile::tempdir().expect("create temp dir");
    let mut config = build_test_config(codex_home.path()).await;
    config
        .features
        .enable(Feature::ShellZshFork)
        .expect("test config should allow shell_zsh_fork");
    config.zsh_path = None;
    let config = Arc::new(config);

    let auth_manager = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("Test API Key"));
    let models_manager = Arc::new(ModelsManager::new(
        config.codex_home.clone(),
        auth_manager.clone(),
        None,
        CollaborationModesConfig::default(),
    ));
    let model = ModelsManager::get_model_offline_for_tests(config.model.as_deref());
    let model_info = ModelsManager::construct_model_info_offline_for_tests(model.as_str(), &config);
    let collaboration_mode = CollaborationMode {
        mode: ModeKind::Default,
        settings: Settings {
            model,
            reasoning_effort: config.model_reasoning_effort,
            developer_instructions: None,
        },
    };
    let session_configuration = SessionConfiguration {
        provider: config.model_provider.clone(),
        collaboration_mode,
        collaboration_mode_reasoning_effort_explicit: false,
        model_reasoning_summary: config.model_reasoning_summary,
        developer_instructions: config.developer_instructions.clone(),
        user_instructions: config.user_instructions.clone(),
        service_tier: None,
        personality: config.personality,
        base_instructions: config
            .base_instructions
            .clone()
            .unwrap_or_else(|| model_info.get_model_instructions(config.personality)),
        compact_prompt: config.compact_prompt.clone(),
        approval_policy: config.permissions.approval_policy.clone(),
        approvals_reviewer: config.approvals_reviewer,
        sandbox_policy: config.permissions.sandbox_policy.clone(),
        file_system_sandbox_policy: config.permissions.file_system_sandbox_policy.clone(),
        network_sandbox_policy: config.permissions.network_sandbox_policy,
        windows_sandbox_level: WindowsSandboxLevel::from_config(&config),
        cwd: config.cwd.clone(),
        config_path: config
            .active_user_config_path()
            .expect("active user config path"),
        codex_home: config.codex_home.clone(),
        thread_name: None,
        original_config_do_not_use: Arc::clone(&config),
        metrics_service_name: None,
        app_server_client_name: None,
        session_source: SessionSource::Exec,
        dynamic_tools: Vec::new(),
        persist_extended_history: false,
        inherited_shell_snapshot: None,
        user_shell_override: None,
    };

    let (tx_event, _rx_event) = async_channel::unbounded();
    let (agent_status_tx, _agent_status_rx) = watch::channel(AgentStatus::PendingInit);
    let plugins_manager = Arc::new(PluginsManager::new(config.codex_home.clone()));
    let mcp_manager = Arc::new(McpManager::new(Arc::clone(&plugins_manager)));
    let skills_manager = Arc::new(SkillsManager::new(
        config.codex_home.clone(),
        Arc::clone(&plugins_manager),
        true,
    ));
    let (agent_last_activity_tx, _agent_last_activity_rx) =
        watch::channel::<Option<codex_protocol::protocol::CollabAgentActivity>>(None);
    let (prompt_gc_active_tx, _prompt_gc_active_rx) = watch::channel(false);
    let (prompt_gc_activity_edges, _) = tokio::sync::broadcast::channel(16);
    let result = Session::new(
        session_configuration,
        Arc::clone(&config),
        auth_manager,
        models_manager,
        Arc::new(ExecPolicyManager::default()),
        tx_event,
        agent_status_tx,
        agent_last_activity_tx,
        prompt_gc_active_tx,
        prompt_gc_activity_edges,
        InitialHistory::New,
        SessionSource::Exec,
        skills_manager,
        plugins_manager,
        mcp_manager,
        Arc::new(FileWatcher::noop()),
        AgentControl::default(),
    )
    .await;

    let err = match result {
        Ok(_) => panic!("expected startup to fail"),
        Err(err) => err,
    };
    let msg = format!("{err:#}");
    assert!(msg.contains("zsh fork feature enabled, but `zsh_path` is not configured"));
}

// todo: use online model info
pub(crate) async fn make_session_and_context() -> (Session, TurnContext) {
    let (tx_event, _rx_event) = async_channel::unbounded();
    let codex_home = tempfile::tempdir().expect("create temp dir");
    let config = build_test_config(codex_home.path()).await;
    let config = Arc::new(config);
    let conversation_id = ThreadId::default();
    let auth_manager = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("Test API Key"));
    let models_manager = Arc::new(ModelsManager::new(
        config.codex_home.clone(),
        auth_manager.clone(),
        None,
        CollaborationModesConfig::default(),
    ));
    let agent_control = AgentControl::default();
    let exec_policy = Arc::new(ExecPolicyManager::default());
    let (agent_status_tx, _agent_status_rx) = watch::channel(AgentStatus::PendingInit);
    let (agent_last_activity_tx, _agent_last_activity_rx) =
        watch::channel::<Option<codex_protocol::protocol::CollabAgentActivity>>(None);
    let model = ModelsManager::get_model_offline_for_tests(config.model.as_deref());
    let model_info = ModelsManager::construct_model_info_offline_for_tests(model.as_str(), &config);
    let reasoning_effort = config.model_reasoning_effort;
    let collaboration_mode = CollaborationMode {
        mode: ModeKind::Default,
        settings: Settings {
            model,
            reasoning_effort,
            developer_instructions: None,
        },
    };
    let session_configuration = SessionConfiguration {
        provider: config.model_provider.clone(),
        collaboration_mode,
        collaboration_mode_reasoning_effort_explicit: false,
        model_reasoning_summary: config.model_reasoning_summary,
        developer_instructions: config.developer_instructions.clone(),
        user_instructions: config.user_instructions.clone(),
        service_tier: None,
        personality: config.personality,
        base_instructions: config
            .base_instructions
            .clone()
            .unwrap_or_else(|| model_info.get_model_instructions(config.personality)),
        compact_prompt: config.compact_prompt.clone(),
        approval_policy: config.permissions.approval_policy.clone(),
        approvals_reviewer: config.approvals_reviewer,
        sandbox_policy: config.permissions.sandbox_policy.clone(),
        file_system_sandbox_policy: config.permissions.file_system_sandbox_policy.clone(),
        network_sandbox_policy: config.permissions.network_sandbox_policy,
        windows_sandbox_level: WindowsSandboxLevel::from_config(&config),
        cwd: config.cwd.clone(),
        config_path: config
            .active_user_config_path()
            .expect("active user config path"),
        codex_home: config.codex_home.clone(),
        thread_name: None,
        original_config_do_not_use: Arc::clone(&config),
        metrics_service_name: None,
        app_server_client_name: None,
        session_source: SessionSource::Exec,
        dynamic_tools: Vec::new(),
        persist_extended_history: false,
        inherited_shell_snapshot: None,
        user_shell_override: None,
    };
    let per_turn_config = Session::build_per_turn_config(&session_configuration);
    let model_info = ModelsManager::construct_model_info_offline_for_tests(
        session_configuration.collaboration_mode.model(),
        &per_turn_config,
    );
    let session_telemetry = session_telemetry(
        conversation_id,
        config.as_ref(),
        &model_info,
        session_configuration.session_source.clone(),
    );

    let state = SessionState::new(session_configuration.clone());
    let plugins_manager = Arc::new(PluginsManager::new(config.codex_home.clone()));
    let mcp_manager = Arc::new(McpManager::new(Arc::clone(&plugins_manager)));
    let skills_manager = Arc::new(SkillsManager::new(
        config.codex_home.clone(),
        Arc::clone(&plugins_manager),
        true,
    ));
    let network_approval = Arc::new(NetworkApprovalService::default());
    let environment = Arc::new(codex_environment::Environment);

    let file_watcher = Arc::new(FileWatcher::noop());
    let services = SessionServices {
        mcp_connection_manager: Arc::new(RwLock::new(
            McpConnectionManager::new_mcp_connection_manager_for_tests(
                &config.permissions.approval_policy,
            ),
        )),
        mcp_startup_cancellation_token: Mutex::new(CancellationToken::new()),
        unified_exec_manager: UnifiedExecProcessManager::new(
            config.background_terminal_max_timeout,
        ),
        shell_zsh_path: None,
        main_execve_wrapper_exe: config.main_execve_wrapper_exe.clone(),
        analytics_events_client: AnalyticsEventsClient::new(
            Arc::clone(&config),
            Arc::clone(&auth_manager),
        ),
        hooks: Hooks::new(HooksConfig {
            legacy_notify_argv: config.notify.clone(),
            ..HooksConfig::default()
        }),
        rollout: Mutex::new(None),
        user_shell: Arc::new(default_user_shell()),
        shell_snapshot_tx: watch::channel(None).0,
        show_raw_agent_reasoning: config.show_raw_agent_reasoning,
        exec_policy,
        auth_manager: auth_manager.clone(),
        session_telemetry: session_telemetry.clone(),
        models_manager: Arc::clone(&models_manager),
        tool_approvals: Mutex::new(ApprovalStore::default()),
        execve_session_approvals: RwLock::new(HashMap::new()),
        skills_manager,
        plugins_manager,
        mcp_manager,
        file_watcher,
        agent_control,
        network_proxy: None,
        network_approval: Arc::clone(&network_approval),
        state_db: None,
        model_client: ModelClient::new(
            Some(auth_manager.clone()),
            conversation_id,
            session_configuration.provider.clone(),
            session_configuration.session_source.clone(),
            config.model_verbosity,
            config.features.enabled(Feature::EnableRequestCompression),
            config.features.enabled(Feature::RuntimeMetrics),
            Session::build_model_client_beta_features_header(config.as_ref()),
        ),
        code_mode_service: crate::tools::code_mode::CodeModeService::new(
            config.js_repl_node_path.clone(),
        ),
        environment: Arc::clone(&environment),
    };
    let js_repl = Arc::new(JsReplHandle::with_node_path(
        config.js_repl_node_path.clone(),
        config.js_repl_node_module_dirs.clone(),
    ));

    let skills_outcome = Arc::new(services.skills_manager.skills_for_config(&per_turn_config));
    let turn_context = Session::make_turn_context(
        Some(Arc::clone(&auth_manager)),
        &session_telemetry,
        session_configuration.provider.clone(),
        &session_configuration,
        services.user_shell.as_ref(),
        services.shell_zsh_path.as_ref(),
        services.main_execve_wrapper_exe.as_ref(),
        per_turn_config,
        model_info,
        &models_manager,
        None,
        environment,
        "turn_id".to_string(),
        Arc::clone(&js_repl),
        skills_outcome,
    );
    let (prompt_gc_active_tx, _prompt_gc_active_rx) = watch::channel(false);
    let (prompt_gc_activity_edges, _) = tokio::sync::broadcast::channel(16);

    let session = Session {
        conversation_id,
        tx_event,
        agent_status: agent_status_tx,
        agent_last_activity: agent_last_activity_tx,
        prompt_gc_active: prompt_gc_active_tx,
        prompt_gc_activity_edges,
        out_of_band_elicitation_paused: watch::channel(false).0,
        state: Mutex::new(state),
        features: config.features.clone(),
        pending_mcp_server_refresh_config: Mutex::new(None),
        conversation: Arc::new(RealtimeConversationManager::new()),
        active_turn: Mutex::new(None),
        guardian_review_session: crate::guardian::GuardianReviewSessionManager::default(),
        services,
        js_repl,
        next_internal_sub_id: AtomicU64::new(0),
    };

    (session, turn_context)
}

#[tokio::test]
async fn notify_request_permissions_response_ignores_unmatched_call_id() {
    let (session, _turn_context) = make_session_and_context().await;
    *session.active_turn.lock().await = Some(ActiveTurn::default());

    session
        .notify_request_permissions_response(
            "missing",
            codex_protocol::request_permissions::RequestPermissionsResponse {
                permissions: RequestPermissionProfile {
                    network: Some(codex_protocol::models::NetworkPermissions {
                        enabled: Some(true),
                    }),
                    ..RequestPermissionProfile::default()
                },
                scope: PermissionGrantScope::Turn,
            },
        )
        .await;

    assert_eq!(session.granted_turn_permissions().await, None);
}

#[tokio::test]
async fn request_permissions_emits_event_when_granular_policy_allows_requests() {
    let (session, mut turn_context, rx) = make_session_and_context_with_rx().await;
    *session.active_turn.lock().await = Some(ActiveTurn::default());
    Arc::get_mut(&mut turn_context)
        .expect("single turn context ref")
        .approval_policy
        .set(crate::protocol::AskForApproval::Granular(
            crate::protocol::GranularApprovalConfig {
                sandbox_approval: true,
                rules: true,
                skill_approval: true,
                request_permissions: true,
                mcp_elicitations: true,
            },
        ))
        .expect("test setup should allow updating approval policy");

    let session = Arc::new(session);
    let turn_context = Arc::new(turn_context);
    let call_id = "call-1".to_string();
    let expected_response = codex_protocol::request_permissions::RequestPermissionsResponse {
        permissions: RequestPermissionProfile {
            network: Some(codex_protocol::models::NetworkPermissions {
                enabled: Some(true),
            }),
            ..RequestPermissionProfile::default()
        },
        scope: PermissionGrantScope::Turn,
    };

    let handle = tokio::spawn({
        let session = Arc::clone(&session);
        let turn_context = Arc::clone(&turn_context);
        let call_id = call_id.clone();
        async move {
            session
                .request_permissions(
                    turn_context.as_ref(),
                    call_id,
                    codex_protocol::request_permissions::RequestPermissionsArgs {
                        reason: Some("need network".to_string()),
                        permissions: RequestPermissionProfile {
                            network: Some(codex_protocol::models::NetworkPermissions {
                                enabled: Some(true),
                            }),
                            ..RequestPermissionProfile::default()
                        },
                    },
                )
                .await
        }
    });

    let request_event = tokio::time::timeout(StdDuration::from_secs(1), rx.recv())
        .await
        .expect("request_permissions event timed out")
        .expect("request_permissions event missing");
    let EventMsg::RequestPermissions(request) = request_event.msg else {
        panic!("expected request_permissions event");
    };
    assert_eq!(request.call_id, call_id);

    session
        .notify_request_permissions_response(&request.call_id, expected_response.clone())
        .await;

    let response = tokio::time::timeout(StdDuration::from_secs(1), handle)
        .await
        .expect("request_permissions future timed out")
        .expect("request_permissions join error");

    assert_eq!(response, Some(expected_response));
}

#[tokio::test]
async fn request_permissions_is_auto_denied_when_granular_policy_blocks_tool_requests() {
    let (session, mut turn_context, rx) = make_session_and_context_with_rx().await;
    *session.active_turn.lock().await = Some(ActiveTurn::default());
    Arc::get_mut(&mut turn_context)
        .expect("single turn context ref")
        .approval_policy
        .set(crate::protocol::AskForApproval::Granular(
            crate::protocol::GranularApprovalConfig {
                sandbox_approval: true,
                rules: true,
                skill_approval: true,
                request_permissions: false,
                mcp_elicitations: true,
            },
        ))
        .expect("test setup should allow updating approval policy");

    let session = Arc::new(session);
    let turn_context = Arc::new(turn_context);
    let call_id = "call-1".to_string();
    let response = session
        .request_permissions(
            turn_context.as_ref(),
            call_id,
            codex_protocol::request_permissions::RequestPermissionsArgs {
                reason: Some("need network".to_string()),
                permissions: RequestPermissionProfile {
                    network: Some(codex_protocol::models::NetworkPermissions {
                        enabled: Some(true),
                    }),
                    ..RequestPermissionProfile::default()
                },
            },
        )
        .await;

    assert_eq!(
        response,
        Some(
            codex_protocol::request_permissions::RequestPermissionsResponse {
                permissions: RequestPermissionProfile::default(),
                scope: PermissionGrantScope::Turn,
            }
        )
    );
    assert!(
        tokio::time::timeout(StdDuration::from_millis(100), rx.recv())
            .await
            .is_err(),
        "request_permissions should not emit an event when granular.request_permissions is false"
    );
}

#[tokio::test]
async fn submit_with_id_captures_current_span_trace_context() {
    let (session, _turn_context) = make_session_and_context().await;
    let (tx_sub, rx_sub) = async_channel::bounded(1);
    let (_tx_event, rx_event) = async_channel::unbounded();
    let (_agent_status_tx, agent_status) = watch::channel(AgentStatus::PendingInit);
    let (_agent_last_activity_tx, agent_last_activity) =
        watch::channel::<Option<codex_protocol::protocol::CollabAgentActivity>>(None);
    let (_prompt_gc_active_tx, prompt_gc_active) = watch::channel(false);
    let (prompt_gc_activity_edges, _) = tokio::sync::broadcast::channel(16);
    let codex = Codex {
        tx_sub,
        rx_event,
        agent_status,
        agent_last_activity,
        prompt_gc_active,
        prompt_gc_activity_edges,
        session: Arc::new(session),
        session_loop_termination: completed_session_loop_termination(),
    };

    init_test_tracing();

    let request_parent = W3cTraceContext {
        traceparent: Some("00-00000000000000000000000000000011-0000000000000022-01".into()),
        tracestate: Some("vendor=value".into()),
    };
    let request_span = info_span!("app_server.request");
    assert!(set_parent_from_w3c_trace_context(
        &request_span,
        &request_parent
    ));

    let expected_trace = async {
        let expected_trace =
            current_span_w3c_trace_context().expect("current span should have trace context");
        codex
            .submit_with_id(Submission {
                id: "sub-1".into(),
                op: Op::Interrupt,
                trace: None,
            })
            .await
            .expect("submit should succeed");
        expected_trace
    }
    .instrument(request_span)
    .await;

    let submitted = rx_sub.recv().await.expect("submission");
    assert_eq!(submitted.trace, Some(expected_trace));
}

#[tokio::test]
async fn new_default_turn_captures_current_span_trace_id() {
    let (session, _turn_context) = make_session_and_context().await;

    init_test_tracing();

    let request_parent = W3cTraceContext {
        traceparent: Some("00-00000000000000000000000000000011-0000000000000022-01".into()),
        tracestate: Some("vendor=value".into()),
    };
    let request_span = info_span!("app_server.request");
    assert!(set_parent_from_w3c_trace_context(
        &request_span,
        &request_parent
    ));

    let turn_context_item = async {
        let expected_trace_id = Span::current()
            .context()
            .span()
            .span_context()
            .trace_id()
            .to_string();
        let turn_context = session.new_default_turn().await;
        let turn_context_item = turn_context.to_turn_context_item();
        assert_eq!(turn_context_item.trace_id, Some(expected_trace_id));
        turn_context_item
    }
    .instrument(request_span)
    .await;

    assert_eq!(
        turn_context_item.trace_id.as_deref(),
        Some("00000000000000000000000000000011")
    );
}

#[test]
fn submission_dispatch_span_prefers_submission_trace_context() {
    init_test_tracing();

    let ambient_parent = W3cTraceContext {
        traceparent: Some("00-00000000000000000000000000000033-0000000000000044-01".into()),
        tracestate: None,
    };
    let ambient_span = info_span!("ambient");
    assert!(set_parent_from_w3c_trace_context(
        &ambient_span,
        &ambient_parent
    ));

    let submission_trace = W3cTraceContext {
        traceparent: Some("00-00000000000000000000000000000055-0000000000000066-01".into()),
        tracestate: Some("vendor=value".into()),
    };
    let dispatch_span = ambient_span.in_scope(|| {
        submission_dispatch_span(&Submission {
            id: "sub-1".into(),
            op: Op::Interrupt,
            trace: Some(submission_trace),
        })
    });

    let trace_id = dispatch_span.context().span().span_context().trace_id();
    assert_eq!(
        trace_id,
        TraceId::from_hex("00000000000000000000000000000055").expect("trace id")
    );
}

#[test]
fn submission_dispatch_span_uses_debug_for_realtime_audio() {
    init_test_tracing();

    let dispatch_span = submission_dispatch_span(&Submission {
        id: "sub-1".into(),
        op: Op::RealtimeConversationAudio(ConversationAudioParams {
            frame: RealtimeAudioFrame {
                data: "ZmFrZQ==".into(),
                sample_rate: 16_000,
                num_channels: 1,
                samples_per_channel: Some(160),
                item_id: None,
            },
        }),
        trace: None,
    });

    assert_eq!(
        dispatch_span.metadata().expect("span metadata").level(),
        &tracing::Level::DEBUG
    );
}

#[test]
fn op_kind_distinguishes_turn_ops() {
    assert_eq!(
        Op::OverrideTurnContext {
            cwd: None,
            approval_policy: None,
            approvals_reviewer: None,
            sandbox_policy: None,
            windows_sandbox_level: None,
            model: None,
            effort: None,
            summary: None,
            service_tier: None,
            collaboration_mode: None,
            personality: None,
        }
        .kind(),
        "override_turn_context"
    );
    assert_eq!(
        Op::UserInput {
            items: vec![],
            final_output_json_schema: None,
        }
        .kind(),
        "user_input"
    );
}

#[tokio::test]
async fn spawn_task_turn_span_inherits_dispatch_trace_context() {
    struct TraceCaptureTask {
        captured_trace: Arc<std::sync::Mutex<Option<W3cTraceContext>>>,
    }

    #[async_trait::async_trait]
    impl SessionTask for TraceCaptureTask {
        fn kind(&self) -> TaskKind {
            TaskKind::Regular
        }

        fn span_name(&self) -> &'static str {
            "session_task.trace_capture"
        }

        async fn run(
            self: Arc<Self>,
            _session: Arc<SessionTaskContext>,
            _ctx: Arc<TurnContext>,
            _input: Vec<UserInput>,
            _cancellation_token: CancellationToken,
        ) -> Option<String> {
            let mut trace = self
                .captured_trace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *trace = current_span_w3c_trace_context();
            None
        }
    }

    init_test_tracing();

    let request_parent = W3cTraceContext {
        traceparent: Some("00-00000000000000000000000000000011-0000000000000022-01".into()),
        tracestate: Some("vendor=value".into()),
    };
    let request_span = tracing::info_span!("app_server.request");
    assert!(set_parent_from_w3c_trace_context(
        &request_span,
        &request_parent
    ));

    let submission_trace =
        async { current_span_w3c_trace_context().expect("request span should have trace context") }
            .instrument(request_span)
            .await;

    let dispatch_span = submission_dispatch_span(&Submission {
        id: "sub-1".into(),
        op: Op::Interrupt,
        trace: Some(submission_trace.clone()),
    });
    let dispatch_span_id = dispatch_span.context().span().span_context().span_id();

    let (sess, tc, rx) = make_session_and_context_with_rx().await;
    let captured_trace = Arc::new(std::sync::Mutex::new(None));

    async {
        sess.spawn_task(
            Arc::clone(&tc),
            vec![UserInput::Text {
                text: "hello".to_string(),
                text_elements: Vec::new(),
            }],
            TraceCaptureTask {
                captured_trace: Arc::clone(&captured_trace),
            },
        )
        .await;
    }
    .instrument(dispatch_span)
    .await;

    let evt = tokio::time::timeout(StdDuration::from_secs(2), rx.recv())
        .await
        .expect("timeout waiting for turn completion")
        .expect("event");
    assert!(matches!(evt.msg, EventMsg::TurnComplete(_)));

    let task_trace = captured_trace
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .expect("turn task should capture the current span trace context");
    let submission_context =
        codex_otel::context_from_w3c_trace_context(&submission_trace).expect("submission");
    let task_context = codex_otel::context_from_w3c_trace_context(&task_trace).expect("task trace");

    assert_eq!(
        task_context.span().span_context().trace_id(),
        submission_context.span().span_context().trace_id()
    );
    assert_ne!(
        task_context.span().span_context().span_id(),
        dispatch_span_id
    );
}

#[tokio::test]
async fn shutdown_and_wait_allows_multiple_waiters() {
    let (session, _turn_context) = make_session_and_context().await;
    let (tx_sub, rx_sub) = async_channel::bounded(4);
    let (_tx_event, rx_event) = async_channel::unbounded();
    let (_agent_status_tx, agent_status) = watch::channel(AgentStatus::PendingInit);
    let (_agent_last_activity_tx, agent_last_activity) =
        watch::channel::<Option<codex_protocol::protocol::CollabAgentActivity>>(None);
    let (_prompt_gc_active_tx, prompt_gc_active) = watch::channel(false);
    let (prompt_gc_activity_edges, _) = tokio::sync::broadcast::channel(16);
    let session_loop_handle = tokio::spawn(async move {
        let shutdown: Submission = rx_sub.recv().await.expect("shutdown submission");
        assert_eq!(shutdown.op, Op::Shutdown);
        tokio::time::sleep(StdDuration::from_millis(50)).await;
    });
    let codex = Arc::new(Codex {
        tx_sub,
        rx_event,
        agent_status,
        agent_last_activity,
        prompt_gc_active,
        prompt_gc_activity_edges,
        session: Arc::new(session),
        session_loop_termination: session_loop_termination_from_handle(session_loop_handle),
    });

    let waiter_1 = {
        let codex = Arc::clone(&codex);
        tokio::spawn(async move { codex.shutdown_and_wait().await })
    };
    let waiter_2 = {
        let codex = Arc::clone(&codex);
        tokio::spawn(async move { codex.shutdown_and_wait().await })
    };

    waiter_1
        .await
        .expect("first shutdown waiter join")
        .expect("first shutdown waiter");
    waiter_2
        .await
        .expect("second shutdown waiter join")
        .expect("second shutdown waiter");
}

#[tokio::test]
async fn shutdown_and_wait_waits_when_shutdown_is_already_in_progress() {
    let (session, _turn_context) = make_session_and_context().await;
    let (tx_sub, rx_sub) = async_channel::bounded(4);
    drop(rx_sub);
    let (_tx_event, rx_event) = async_channel::unbounded();
    let (_agent_status_tx, agent_status) = watch::channel(AgentStatus::PendingInit);
    let (_agent_last_activity_tx, agent_last_activity) =
        watch::channel::<Option<codex_protocol::protocol::CollabAgentActivity>>(None);
    let (_prompt_gc_active_tx, prompt_gc_active) = watch::channel(false);
    let (prompt_gc_activity_edges, _) = tokio::sync::broadcast::channel(16);
    let (shutdown_complete_tx, shutdown_complete_rx) = tokio::sync::oneshot::channel();
    let session_loop_handle = tokio::spawn(async move {
        let _ = shutdown_complete_rx.await;
    });
    let codex = Arc::new(Codex {
        tx_sub,
        rx_event,
        agent_status,
        agent_last_activity,
        prompt_gc_active,
        prompt_gc_activity_edges,
        session: Arc::new(session),
        session_loop_termination: session_loop_termination_from_handle(session_loop_handle),
    });

    let waiter = {
        let codex = Arc::clone(&codex);
        tokio::spawn(async move { codex.shutdown_and_wait().await })
    };

    tokio::time::sleep(StdDuration::from_millis(10)).await;
    assert!(!waiter.is_finished());

    shutdown_complete_tx
        .send(())
        .expect("session loop should still be waiting to terminate");

    waiter
        .await
        .expect("shutdown waiter join")
        .expect("shutdown waiter");
}

#[tokio::test]
async fn shutdown_and_wait_shuts_down_cached_guardian_subagent() {
    let (parent_session, parent_turn_context) = make_session_and_context().await;
    let parent_session = Arc::new(parent_session);
    let parent_config = Arc::clone(&parent_turn_context.config);
    let (parent_tx_sub, parent_rx_sub) = async_channel::bounded(4);
    let (_parent_tx_event, parent_rx_event) = async_channel::unbounded();
    let (_parent_status_tx, parent_agent_status) = watch::channel(AgentStatus::PendingInit);
    let (_parent_agent_last_activity_tx, parent_agent_last_activity) =
        watch::channel::<Option<codex_protocol::protocol::CollabAgentActivity>>(None);
    let (_parent_prompt_gc_active_tx, parent_prompt_gc_active) = watch::channel(false);
    let (parent_prompt_gc_activity_edges, _) = tokio::sync::broadcast::channel(16);
    let parent_session_for_loop = Arc::clone(&parent_session);
    let parent_session_loop_handle = tokio::spawn(async move {
        submission_loop(parent_session_for_loop, parent_config, parent_rx_sub).await;
    });
    let parent_codex = Codex {
        tx_sub: parent_tx_sub,
        rx_event: parent_rx_event,
        agent_status: parent_agent_status,
        agent_last_activity: parent_agent_last_activity,
        prompt_gc_active: parent_prompt_gc_active,
        prompt_gc_activity_edges: parent_prompt_gc_activity_edges,
        session: Arc::clone(&parent_session),
        session_loop_termination: session_loop_termination_from_handle(parent_session_loop_handle),
    };

    let (child_session, _child_turn_context) = make_session_and_context().await;
    let (child_tx_sub, child_rx_sub) = async_channel::bounded(4);
    let (_child_tx_event, child_rx_event) = async_channel::unbounded();
    let (_child_status_tx, child_agent_status) = watch::channel(AgentStatus::PendingInit);
    let (_child_agent_last_activity_tx, child_agent_last_activity) =
        watch::channel::<Option<codex_protocol::protocol::CollabAgentActivity>>(None);
    let (_child_prompt_gc_active_tx, child_prompt_gc_active) = watch::channel(false);
    let (child_prompt_gc_activity_edges, _) = tokio::sync::broadcast::channel(16);
    let (child_shutdown_tx, child_shutdown_rx) = tokio::sync::oneshot::channel();
    let child_session_loop_handle = tokio::spawn(async move {
        let shutdown: Submission = child_rx_sub
            .recv()
            .await
            .expect("child shutdown submission");
        assert_eq!(shutdown.op, Op::Shutdown);
        child_shutdown_tx
            .send(())
            .expect("child shutdown signal should be delivered");
    });
    let child_codex = Codex {
        tx_sub: child_tx_sub,
        rx_event: child_rx_event,
        agent_status: child_agent_status,
        agent_last_activity: child_agent_last_activity,
        prompt_gc_active: child_prompt_gc_active,
        prompt_gc_activity_edges: child_prompt_gc_activity_edges,
        session: Arc::new(child_session),
        session_loop_termination: session_loop_termination_from_handle(child_session_loop_handle),
    };
    parent_session
        .guardian_review_session
        .cache_for_test(child_codex)
        .await;

    parent_codex
        .shutdown_and_wait()
        .await
        .expect("parent shutdown should succeed");

    child_shutdown_rx
        .await
        .expect("guardian subagent should receive a shutdown op");
}

#[tokio::test]
async fn shutdown_and_wait_shuts_down_tracked_ephemeral_guardian_review() {
    let (parent_session, parent_turn_context) = make_session_and_context().await;
    let parent_session = Arc::new(parent_session);
    let parent_config = Arc::clone(&parent_turn_context.config);
    let (parent_tx_sub, parent_rx_sub) = async_channel::bounded(4);
    let (_parent_tx_event, parent_rx_event) = async_channel::unbounded();
    let (_parent_status_tx, parent_agent_status) = watch::channel(AgentStatus::PendingInit);
    let (_parent_agent_last_activity_tx, parent_agent_last_activity) =
        watch::channel::<Option<codex_protocol::protocol::CollabAgentActivity>>(None);
    let (_parent_prompt_gc_active_tx, parent_prompt_gc_active) = watch::channel(false);
    let (parent_prompt_gc_activity_edges, _) = tokio::sync::broadcast::channel(16);
    let parent_session_for_loop = Arc::clone(&parent_session);
    let parent_session_loop_handle = tokio::spawn(async move {
        submission_loop(parent_session_for_loop, parent_config, parent_rx_sub).await;
    });
    let parent_codex = Codex {
        tx_sub: parent_tx_sub,
        rx_event: parent_rx_event,
        agent_status: parent_agent_status,
        agent_last_activity: parent_agent_last_activity,
        prompt_gc_active: parent_prompt_gc_active,
        prompt_gc_activity_edges: parent_prompt_gc_activity_edges,
        session: Arc::clone(&parent_session),
        session_loop_termination: session_loop_termination_from_handle(parent_session_loop_handle),
    };

    let (child_session, _child_turn_context) = make_session_and_context().await;
    let (child_tx_sub, child_rx_sub) = async_channel::bounded(4);
    let (_child_tx_event, child_rx_event) = async_channel::unbounded();
    let (_child_status_tx, child_agent_status) = watch::channel(AgentStatus::PendingInit);
    let (_child_agent_last_activity_tx, child_agent_last_activity) =
        watch::channel::<Option<codex_protocol::protocol::CollabAgentActivity>>(None);
    let (_child_prompt_gc_active_tx, child_prompt_gc_active) = watch::channel(false);
    let (child_prompt_gc_activity_edges, _) = tokio::sync::broadcast::channel(16);
    let (child_shutdown_tx, child_shutdown_rx) = tokio::sync::oneshot::channel();
    let child_session_loop_handle = tokio::spawn(async move {
        let shutdown: Submission = child_rx_sub
            .recv()
            .await
            .expect("child shutdown submission");
        assert_eq!(shutdown.op, Op::Shutdown);
        child_shutdown_tx
            .send(())
            .expect("child shutdown signal should be delivered");
    });
    let child_codex = Codex {
        tx_sub: child_tx_sub,
        rx_event: child_rx_event,
        agent_status: child_agent_status,
        agent_last_activity: child_agent_last_activity,
        prompt_gc_active: child_prompt_gc_active,
        prompt_gc_activity_edges: child_prompt_gc_activity_edges,
        session: Arc::new(child_session),
        session_loop_termination: session_loop_termination_from_handle(child_session_loop_handle),
    };
    parent_session
        .guardian_review_session
        .register_ephemeral_for_test(child_codex)
        .await;

    parent_codex
        .shutdown_and_wait()
        .await
        .expect("parent shutdown should succeed");

    child_shutdown_rx
        .await
        .expect("ephemeral guardian review should receive a shutdown op");
}

pub(crate) async fn make_session_and_context_with_dynamic_tools_and_rx(
    dynamic_tools: Vec<DynamicToolSpec>,
) -> (
    Arc<Session>,
    Arc<TurnContext>,
    async_channel::Receiver<Event>,
) {
    let (tx_event, rx_event) = async_channel::unbounded();
    let codex_home = tempfile::tempdir().expect("create temp dir");
    let config = build_test_config(codex_home.path()).await;
    let config = Arc::new(config);
    let conversation_id = ThreadId::default();
    let auth_manager = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("Test API Key"));
    let models_manager = Arc::new(ModelsManager::new(
        config.codex_home.clone(),
        auth_manager.clone(),
        None,
        CollaborationModesConfig::default(),
    ));
    let agent_control = AgentControl::default();
    let exec_policy = Arc::new(ExecPolicyManager::default());
    let (agent_status_tx, _agent_status_rx) = watch::channel(AgentStatus::PendingInit);
    let (agent_last_activity_tx, _agent_last_activity_rx) =
        watch::channel::<Option<codex_protocol::protocol::CollabAgentActivity>>(None);
    let model = ModelsManager::get_model_offline_for_tests(config.model.as_deref());
    let model_info = ModelsManager::construct_model_info_offline_for_tests(model.as_str(), &config);
    let reasoning_effort = config.model_reasoning_effort;
    let collaboration_mode = CollaborationMode {
        mode: ModeKind::Default,
        settings: Settings {
            model,
            reasoning_effort,
            developer_instructions: None,
        },
    };
    let session_configuration = SessionConfiguration {
        provider: config.model_provider.clone(),
        collaboration_mode,
        collaboration_mode_reasoning_effort_explicit: false,
        model_reasoning_summary: config.model_reasoning_summary,
        developer_instructions: config.developer_instructions.clone(),
        user_instructions: config.user_instructions.clone(),
        service_tier: None,
        personality: config.personality,
        base_instructions: config
            .base_instructions
            .clone()
            .unwrap_or_else(|| model_info.get_model_instructions(config.personality)),
        compact_prompt: config.compact_prompt.clone(),
        approval_policy: config.permissions.approval_policy.clone(),
        approvals_reviewer: config.approvals_reviewer,
        sandbox_policy: config.permissions.sandbox_policy.clone(),
        file_system_sandbox_policy: config.permissions.file_system_sandbox_policy.clone(),
        network_sandbox_policy: config.permissions.network_sandbox_policy,
        windows_sandbox_level: WindowsSandboxLevel::from_config(&config),
        cwd: config.cwd.clone(),
        config_path: config
            .active_user_config_path()
            .expect("active user config path"),
        codex_home: config.codex_home.clone(),
        thread_name: None,
        original_config_do_not_use: Arc::clone(&config),
        metrics_service_name: None,
        app_server_client_name: None,
        session_source: SessionSource::Exec,
        dynamic_tools,
        persist_extended_history: false,
        inherited_shell_snapshot: None,
        user_shell_override: None,
    };
    let per_turn_config = Session::build_per_turn_config(&session_configuration);
    let model_info = ModelsManager::construct_model_info_offline_for_tests(
        session_configuration.collaboration_mode.model(),
        &per_turn_config,
    );
    let session_telemetry = session_telemetry(
        conversation_id,
        config.as_ref(),
        &model_info,
        session_configuration.session_source.clone(),
    );

    let state = SessionState::new(session_configuration.clone());
    let plugins_manager = Arc::new(PluginsManager::new(config.codex_home.clone()));
    let mcp_manager = Arc::new(McpManager::new(Arc::clone(&plugins_manager)));
    let skills_manager = Arc::new(SkillsManager::new(
        config.codex_home.clone(),
        Arc::clone(&plugins_manager),
        true,
    ));
    let network_approval = Arc::new(NetworkApprovalService::default());
    let environment = Arc::new(codex_environment::Environment);

    let file_watcher = Arc::new(FileWatcher::noop());
    let services = SessionServices {
        mcp_connection_manager: Arc::new(RwLock::new(
            McpConnectionManager::new_mcp_connection_manager_for_tests(
                &config.permissions.approval_policy,
            ),
        )),
        mcp_startup_cancellation_token: Mutex::new(CancellationToken::new()),
        unified_exec_manager: UnifiedExecProcessManager::new(
            config.background_terminal_max_timeout,
        ),
        shell_zsh_path: None,
        main_execve_wrapper_exe: config.main_execve_wrapper_exe.clone(),
        analytics_events_client: AnalyticsEventsClient::new(
            Arc::clone(&config),
            Arc::clone(&auth_manager),
        ),
        hooks: Hooks::new(HooksConfig {
            legacy_notify_argv: config.notify.clone(),
            ..HooksConfig::default()
        }),
        rollout: Mutex::new(None),
        user_shell: Arc::new(default_user_shell()),
        shell_snapshot_tx: watch::channel(None).0,
        show_raw_agent_reasoning: config.show_raw_agent_reasoning,
        exec_policy,
        auth_manager: Arc::clone(&auth_manager),
        session_telemetry: session_telemetry.clone(),
        models_manager: Arc::clone(&models_manager),
        tool_approvals: Mutex::new(ApprovalStore::default()),
        execve_session_approvals: RwLock::new(HashMap::new()),
        skills_manager,
        plugins_manager,
        mcp_manager,
        file_watcher,
        agent_control,
        network_proxy: None,
        network_approval: Arc::clone(&network_approval),
        state_db: None,
        model_client: ModelClient::new(
            Some(Arc::clone(&auth_manager)),
            conversation_id,
            session_configuration.provider.clone(),
            session_configuration.session_source.clone(),
            config.model_verbosity,
            config.features.enabled(Feature::EnableRequestCompression),
            config.features.enabled(Feature::RuntimeMetrics),
            Session::build_model_client_beta_features_header(config.as_ref()),
        ),
        code_mode_service: crate::tools::code_mode::CodeModeService::new(
            config.js_repl_node_path.clone(),
        ),
        environment: Arc::clone(&environment),
    };
    let js_repl = Arc::new(JsReplHandle::with_node_path(
        config.js_repl_node_path.clone(),
        config.js_repl_node_module_dirs.clone(),
    ));

    let skills_outcome = Arc::new(services.skills_manager.skills_for_config(&per_turn_config));
    let turn_context = Arc::new(Session::make_turn_context(
        Some(Arc::clone(&auth_manager)),
        &session_telemetry,
        session_configuration.provider.clone(),
        &session_configuration,
        services.user_shell.as_ref(),
        services.shell_zsh_path.as_ref(),
        services.main_execve_wrapper_exe.as_ref(),
        per_turn_config,
        model_info,
        &models_manager,
        None,
        environment,
        "turn_id".to_string(),
        Arc::clone(&js_repl),
        skills_outcome,
    ));
    let (prompt_gc_active_tx, _prompt_gc_active_rx) = watch::channel(false);
    let (prompt_gc_activity_edges, _) = tokio::sync::broadcast::channel(16);

    let session = Arc::new(Session {
        conversation_id,
        tx_event,
        agent_status: agent_status_tx,
        agent_last_activity: agent_last_activity_tx,
        prompt_gc_active: prompt_gc_active_tx,
        prompt_gc_activity_edges,
        out_of_band_elicitation_paused: watch::channel(false).0,
        state: Mutex::new(state),
        features: config.features.clone(),
        pending_mcp_server_refresh_config: Mutex::new(None),
        conversation: Arc::new(RealtimeConversationManager::new()),
        active_turn: Mutex::new(None),
        guardian_review_session: crate::guardian::GuardianReviewSessionManager::default(),
        services,
        js_repl,
        next_internal_sub_id: AtomicU64::new(0),
    });

    (session, turn_context, rx_event)
}

// Like make_session_and_context, but returns Arc<Session> and the event receiver
// so tests can assert on emitted events.
pub(crate) async fn make_session_and_context_with_rx() -> (
    Arc<Session>,
    Arc<TurnContext>,
    async_channel::Receiver<Event>,
) {
    make_session_and_context_with_dynamic_tools_and_rx(Vec::new()).await
}

#[tokio::test]
async fn refresh_mcp_servers_is_deferred_until_next_turn() {
    let (session, turn_context) = make_session_and_context().await;
    let old_token = session.mcp_startup_cancellation_token().await;
    assert!(!old_token.is_cancelled());

    let mcp_oauth_credentials_store_mode =
        serde_json::to_value(OAuthCredentialsStoreMode::Auto).expect("serialize store mode");
    let refresh_config = McpServerRefreshConfig {
        mcp_servers: json!({}),
        mcp_oauth_credentials_store_mode,
    };
    {
        let mut guard = session.pending_mcp_server_refresh_config.lock().await;
        *guard = Some(refresh_config);
    }

    assert!(!old_token.is_cancelled());
    assert!(
        session
            .pending_mcp_server_refresh_config
            .lock()
            .await
            .is_some()
    );

    session
        .refresh_mcp_servers_if_requested(&turn_context)
        .await;

    assert!(old_token.is_cancelled());
    assert!(
        session
            .pending_mcp_server_refresh_config
            .lock()
            .await
            .is_none()
    );
    let new_token = session.mcp_startup_cancellation_token().await;
    assert!(!new_token.is_cancelled());
}

#[tokio::test]
async fn record_model_warning_appends_user_message() {
    let (mut session, turn_context) = make_session_and_context().await;
    let features = crate::features::Features::with_defaults().into();
    session.features = features;

    session
        .record_model_warning("too many unified exec processes", &turn_context)
        .await;

    let history = session.clone_history().await;
    let history_items = history.raw_items();
    let last = history_items.last().expect("warning recorded");

    match last {
        ResponseItem::Message { role, content, .. } => {
            assert_eq!(role, "user");
            assert_eq!(
                content,
                &vec![ContentItem::InputText {
                    text: "Warning: too many unified exec processes".to_string(),
                }]
            );
        }
        other => panic!("expected user message, got {other:?}"),
    }
}

#[tokio::test]
async fn spawn_task_does_not_update_previous_turn_settings_for_non_run_turn_tasks() {
    let (sess, tc, _rx) = make_session_and_context_with_rx().await;
    sess.set_previous_turn_settings(None).await;
    let input = vec![UserInput::Text {
        text: "hello".to_string(),
        text_elements: Vec::new(),
    }];

    sess.spawn_task(
        Arc::clone(&tc),
        input,
        NeverEndingTask {
            kind: TaskKind::Regular,
            listen_to_cancellation_token: true,
        },
    )
    .await;

    sess.abort_all_tasks(TurnAbortReason::Interrupted).await;
    assert_eq!(sess.previous_turn_settings().await, None);
}

#[tokio::test]
async fn build_settings_update_items_emits_environment_item_for_network_changes() {
    let (session, previous_context) = make_session_and_context().await;
    let previous_context = Arc::new(previous_context);
    let mut current_context = previous_context
        .with_model(
            previous_context.model_info.slug.clone(),
            &session.services.models_manager,
        )
        .await;

    let mut config = (*current_context.config).clone();
    let mut requirements = config.config_layer_stack.requirements().clone();
    requirements.network = Some(Sourced::new(
        NetworkConstraints {
            allowed_domains: Some(vec!["api.example.com".to_string()]),
            denied_domains: Some(vec!["blocked.example.com".to_string()]),
            ..Default::default()
        },
        RequirementSource::CloudRequirements,
    ));
    let layers = config
        .config_layer_stack
        .get_layers(ConfigLayerStackOrdering::LowestPrecedenceFirst, true)
        .into_iter()
        .cloned()
        .collect();
    config.config_layer_stack = ConfigLayerStack::new(
        layers,
        requirements,
        config.config_layer_stack.requirements_toml().clone(),
    )
    .expect("rebuild config layer stack with network requirements");
    current_context.config = Arc::new(config);

    let reference_context_item = previous_context.to_turn_context_item();
    let update_items = session
        .build_settings_update_items(Some(&reference_context_item), &current_context)
        .await;

    let environment_update = update_items
        .iter()
        .find_map(|item| match item {
            ResponseItem::Message { role, content, .. } if role == "user" => {
                let [ContentItem::InputText { text }] = content.as_slice() else {
                    return None;
                };
                text.contains("<environment_context>").then_some(text)
            }
            _ => None,
        })
        .expect("environment update item should be emitted");
    assert!(environment_update.contains("<network enabled=\"true\">"));
    assert!(environment_update.contains("<allowed>api.example.com</allowed>"));
    assert!(environment_update.contains("<denied>blocked.example.com</denied>"));
}

#[tokio::test]
async fn build_settings_update_items_emits_environment_item_for_time_changes() {
    let (session, previous_context) = make_session_and_context().await;
    let previous_context = Arc::new(previous_context);
    let mut current_context = previous_context
        .with_model(
            previous_context.model_info.slug.clone(),
            &session.services.models_manager,
        )
        .await;
    current_context.current_date = Some("2026-02-27".to_string());
    current_context.timezone = Some("Europe/Berlin".to_string());

    let reference_context_item = previous_context.to_turn_context_item();
    let update_items = session
        .build_settings_update_items(Some(&reference_context_item), &current_context)
        .await;

    let environment_update = update_items
        .iter()
        .find_map(|item| match item {
            ResponseItem::Message { role, content, .. } if role == "user" => {
                let [ContentItem::InputText { text }] = content.as_slice() else {
                    return None;
                };
                text.contains("<environment_context>").then_some(text)
            }
            _ => None,
        })
        .expect("environment update item should be emitted");
    assert!(environment_update.contains("<current_date>2026-02-27</current_date>"));
    assert!(environment_update.contains("<timezone>Europe/Berlin</timezone>"));
}

#[tokio::test]
async fn build_settings_update_items_emits_realtime_start_when_session_becomes_live() {
    let (session, previous_context) = make_session_and_context().await;
    let previous_context = Arc::new(previous_context);
    let mut current_context = previous_context
        .with_model(
            previous_context.model_info.slug.clone(),
            &session.services.models_manager,
        )
        .await;
    current_context.realtime_active = true;

    let update_items = session
        .build_settings_update_items(
            Some(&previous_context.to_turn_context_item()),
            &current_context,
        )
        .await;

    let developer_texts = developer_input_texts(&update_items);
    assert!(
        developer_texts
            .iter()
            .any(|text| text.contains("<realtime_conversation>")),
        "expected a realtime start update, got {developer_texts:?}"
    );
}

#[tokio::test]
async fn build_settings_update_items_emits_realtime_end_when_session_stops_being_live() {
    let (session, mut previous_context) = make_session_and_context().await;
    previous_context.realtime_active = true;
    let mut current_context = previous_context
        .with_model(
            previous_context.model_info.slug.clone(),
            &session.services.models_manager,
        )
        .await;
    current_context.realtime_active = false;

    let update_items = session
        .build_settings_update_items(
            Some(&previous_context.to_turn_context_item()),
            &current_context,
        )
        .await;

    let developer_texts = developer_input_texts(&update_items);
    assert!(
        developer_texts
            .iter()
            .any(|text| text.contains("Reason: inactive")),
        "expected a realtime end update, got {developer_texts:?}"
    );
}

#[tokio::test]
async fn build_settings_update_items_uses_previous_turn_settings_for_realtime_end() {
    let (session, previous_context) = make_session_and_context().await;
    let mut previous_context_item = previous_context.to_turn_context_item();
    previous_context_item.realtime_active = None;
    let previous_turn_settings = PreviousTurnSettings {
        model: previous_context.model_info.slug.clone(),
        realtime_active: Some(true),
    };
    let mut current_context = previous_context
        .with_model(
            previous_context.model_info.slug.clone(),
            &session.services.models_manager,
        )
        .await;
    current_context.realtime_active = false;

    session
        .set_previous_turn_settings(Some(previous_turn_settings))
        .await;
    let update_items = session
        .build_settings_update_items(Some(&previous_context_item), &current_context)
        .await;

    let developer_texts = developer_input_texts(&update_items);
    assert!(
        developer_texts
            .iter()
            .any(|text| text.contains("Reason: inactive")),
        "expected a realtime end update from previous turn settings, got {developer_texts:?}"
    );
}

#[tokio::test]
async fn build_initial_context_uses_previous_realtime_state() {
    let (session, mut turn_context) = make_session_and_context().await;
    turn_context.realtime_active = true;

    let initial_context = session.build_initial_context(&turn_context).await;
    let developer_texts = developer_input_texts(&initial_context);
    assert!(
        developer_texts
            .iter()
            .any(|text| text.contains("<realtime_conversation>")),
        "expected initial context to describe active realtime state, got {developer_texts:?}"
    );

    let previous_context_item = turn_context.to_turn_context_item();
    {
        let mut state = session.state.lock().await;
        state.set_reference_context_item(Some(previous_context_item));
    }
    let resumed_context = session.build_initial_context(&turn_context).await;
    let resumed_developer_texts = developer_input_texts(&resumed_context);
    assert!(
        !resumed_developer_texts
            .iter()
            .any(|text| text.contains("<realtime_conversation>")),
        "did not expect a duplicate realtime update, got {resumed_developer_texts:?}"
    );
}

#[tokio::test]
async fn build_initial_context_omits_default_image_save_location_with_image_history() {
    let (session, turn_context) = make_session_and_context().await;
    session
        .replace_history(
            vec![ResponseItem::ImageGenerationCall {
                id: "ig-test".to_string(),
                status: "completed".to_string(),
                revised_prompt: Some("a tiny blue square".to_string()),
                result: "Zm9v".to_string(),
            }],
            None,
        )
        .await;

    let initial_context = session.build_initial_context(&turn_context).await;
    let developer_texts = developer_input_texts(&initial_context);
    assert!(
        !developer_texts
            .iter()
            .any(|text| text.contains("Generated images are saved to")),
        "expected initial context to omit image save instructions even with image history, got {developer_texts:?}"
    );
}

#[tokio::test]
async fn build_initial_context_omits_default_image_save_location_without_image_history() {
    let (session, turn_context) = make_session_and_context().await;

    let initial_context = session.build_initial_context(&turn_context).await;
    let developer_texts = developer_input_texts(&initial_context);

    assert!(
        !developer_texts
            .iter()
            .any(|text| text.contains("Generated images are saved to")),
        "expected initial context to omit image save instructions without image history, got {developer_texts:?}"
    );
}

#[tokio::test]
async fn handle_output_item_done_records_image_save_history_message() {
    let (session, turn_context) = make_session_and_context().await;
    let session = Arc::new(session);
    let turn_context = Arc::new(turn_context);
    let call_id = "ig_history_records_message";
    let expected_saved_path = std::env::temp_dir().join(format!("{call_id}.png"));
    let _ = std::fs::remove_file(&expected_saved_path);
    let item = ResponseItem::ImageGenerationCall {
        id: call_id.to_string(),
        status: "completed".to_string(),
        revised_prompt: Some("a tiny blue square".to_string()),
        result: "Zm9v".to_string(),
    };

    let mut ctx = HandleOutputCtx {
        sess: Arc::clone(&session),
        turn_context: Arc::clone(&turn_context),
        tool_runtime: test_tool_runtime(Arc::clone(&session), Arc::clone(&turn_context)),
        cancellation_token: CancellationToken::new(),
        execution_mode: SamplingExecutionMode::Visible,
    };
    handle_output_item_done(&mut ctx, item.clone(), None)
        .await
        .expect("image generation item should succeed");

    let history = session.clone_history().await;
    let save_message: ResponseItem = DeveloperInstructions::new(format!(
        "Generated images are saved to {} as {} by default.",
        std::env::temp_dir().display(),
        std::env::temp_dir().join("<image_id>.png").display(),
    ))
    .into();
    assert_eq!(history.raw_items(), &[save_message, item]);
    assert_eq!(
        std::fs::read(&expected_saved_path).expect("saved file"),
        b"foo"
    );
    let _ = std::fs::remove_file(&expected_saved_path);
}

#[tokio::test]
async fn handle_output_item_done_skips_image_save_message_when_save_fails() {
    let (session, turn_context) = make_session_and_context().await;
    let session = Arc::new(session);
    let turn_context = Arc::new(turn_context);
    let call_id = "ig_history_no_message";
    let expected_saved_path = std::env::temp_dir().join(format!("{call_id}.png"));
    let _ = std::fs::remove_file(&expected_saved_path);
    let item = ResponseItem::ImageGenerationCall {
        id: call_id.to_string(),
        status: "completed".to_string(),
        revised_prompt: Some("broken payload".to_string()),
        result: "_-8".to_string(),
    };

    let mut ctx = HandleOutputCtx {
        sess: Arc::clone(&session),
        turn_context: Arc::clone(&turn_context),
        tool_runtime: test_tool_runtime(Arc::clone(&session), Arc::clone(&turn_context)),
        cancellation_token: CancellationToken::new(),
        execution_mode: SamplingExecutionMode::Visible,
    };
    handle_output_item_done(&mut ctx, item.clone(), None)
        .await
        .expect("image generation item should still complete");

    let history = session.clone_history().await;
    assert_eq!(history.raw_items(), &[item]);
    assert!(!expected_saved_path.exists());
}

#[tokio::test]
async fn build_initial_context_uses_previous_turn_settings_for_realtime_end() {
    let (session, turn_context) = make_session_and_context().await;
    let previous_turn_settings = PreviousTurnSettings {
        model: turn_context.model_info.slug.clone(),
        realtime_active: Some(true),
    };

    session
        .set_previous_turn_settings(Some(previous_turn_settings))
        .await;
    let initial_context = session.build_initial_context(&turn_context).await;
    let developer_texts = developer_input_texts(&initial_context);
    assert!(
        developer_texts
            .iter()
            .any(|text| text.contains("Reason: inactive")),
        "expected initial context to describe an ended realtime session, got {developer_texts:?}"
    );
}

#[tokio::test]
async fn build_initial_context_restates_realtime_start_when_reference_context_is_missing() {
    let (session, mut turn_context) = make_session_and_context().await;
    turn_context.realtime_active = true;
    let previous_turn_settings = PreviousTurnSettings {
        model: turn_context.model_info.slug.clone(),
        realtime_active: Some(true),
    };

    session
        .set_previous_turn_settings(Some(previous_turn_settings))
        .await;
    let initial_context = session.build_initial_context(&turn_context).await;
    let developer_texts = developer_input_texts(&initial_context);
    assert!(
        developer_texts
            .iter()
            .any(|text| text.contains("<realtime_conversation>")),
        "expected initial context to restate active realtime when the reference context is missing, got {developer_texts:?}"
    );
}

#[tokio::test]
async fn record_context_updates_and_set_reference_context_item_injects_full_context_when_baseline_missing()
 {
    let (session, turn_context) = make_session_and_context().await;
    session
        .record_context_updates_and_set_reference_context_item(&turn_context)
        .await;
    let history = session.clone_history().await;
    let initial_context = session.build_initial_context(&turn_context).await;
    assert_eq!(history.raw_items().to_vec(), initial_context);

    let current_context = session.reference_context_item().await;
    assert_eq!(
        serde_json::to_value(current_context).expect("serialize current context item"),
        serde_json::to_value(Some(turn_context.to_turn_context_item()))
            .expect("serialize expected context item")
    );
}

#[tokio::test]
async fn record_context_updates_and_set_reference_context_item_reinjects_full_context_after_clear()
{
    let (session, turn_context) = make_session_and_context().await;
    let compacted_summary = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: format!("{}\nsummary", crate::compact::SUMMARY_PREFIX),
        }],
        end_turn: None,
        phase: None,
    };
    session
        .record_into_history(std::slice::from_ref(&compacted_summary), &turn_context)
        .await;
    session
        .record_context_updates_and_set_reference_context_item(&turn_context)
        .await;
    {
        let mut state = session.state.lock().await;
        state.set_reference_context_item(None);
    }
    session
        .replace_history(vec![compacted_summary.clone()], None)
        .await;

    session
        .record_context_updates_and_set_reference_context_item(&turn_context)
        .await;

    let history = session.clone_history().await;
    let mut expected_history = vec![compacted_summary];
    expected_history.extend(session.build_initial_context(&turn_context).await);
    assert_eq!(history.raw_items().to_vec(), expected_history);
}

#[tokio::test]
async fn record_context_updates_and_set_reference_context_item_persists_baseline_without_emitting_diffs()
 {
    let (session, previous_context) = make_session_and_context().await;
    let next_model = if previous_context.model_info.slug == "gpt-5.1" {
        "gpt-5"
    } else {
        "gpt-5.1"
    };
    let turn_context = previous_context
        .with_model(next_model.to_string(), &session.services.models_manager)
        .await;
    let previous_context_item = previous_context.to_turn_context_item();
    {
        let mut state = session.state.lock().await;
        state.set_reference_context_item(Some(previous_context_item.clone()));
    }
    let config = session.get_config().await;
    let recorder = RolloutRecorder::new(
        config.as_ref(),
        RolloutRecorderParams::new(
            ThreadId::default(),
            None,
            SessionSource::Exec,
            BaseInstructions::default(),
            Vec::new(),
            EventPersistenceMode::Limited,
        ),
        None,
        None,
    )
    .await
    .expect("create rollout recorder");
    let rollout_path = recorder.rollout_path().to_path_buf();
    {
        let mut rollout = session.services.rollout.lock().await;
        *rollout = Some(recorder);
    }

    let update_items = session
        .build_settings_update_items(Some(&previous_context_item), &turn_context)
        .await;
    assert_eq!(update_items, Vec::new());

    session
        .record_context_updates_and_set_reference_context_item(&turn_context)
        .await;

    assert_eq!(
        session.clone_history().await.raw_items().to_vec(),
        Vec::new()
    );
    assert_eq!(
        serde_json::to_value(session.reference_context_item().await)
            .expect("serialize current context item"),
        serde_json::to_value(Some(turn_context.to_turn_context_item()))
            .expect("serialize expected context item")
    );
    session.ensure_rollout_materialized().await;
    session.flush_rollout().await;

    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let persisted_turn_context = resumed.history.iter().find_map(|item| match item {
        RolloutItem::TurnContext(ctx) => Some(ctx.clone()),
        _ => None,
    });
    assert_eq!(
        serde_json::to_value(persisted_turn_context)
            .expect("serialize persisted turn context item"),
        serde_json::to_value(Some(turn_context.to_turn_context_item()))
            .expect("serialize expected turn context item")
    );
}

#[tokio::test]
async fn build_initial_context_prepends_model_switch_message() {
    let (session, turn_context) = make_session_and_context().await;
    let previous_turn_settings = PreviousTurnSettings {
        model: "previous-regular-model".to_string(),
        realtime_active: None,
    };

    session
        .set_previous_turn_settings(Some(previous_turn_settings))
        .await;
    let initial_context = session.build_initial_context(&turn_context).await;

    let ResponseItem::Message { role, content, .. } = &initial_context[0] else {
        panic!("expected developer message");
    };
    assert_eq!(role, "developer");
    let [ContentItem::InputText { text }, ..] = content.as_slice() else {
        panic!("expected developer text");
    };
    assert!(text.contains("<model_switch>"));
}

#[tokio::test]
async fn record_context_updates_and_set_reference_context_item_persists_full_reinjection_to_rollout()
 {
    let (session, previous_context) = make_session_and_context().await;
    let next_model = if previous_context.model_info.slug == "gpt-5.1" {
        "gpt-5"
    } else {
        "gpt-5.1"
    };
    let turn_context = previous_context
        .with_model(next_model.to_string(), &session.services.models_manager)
        .await;
    let config = session.get_config().await;
    let recorder = RolloutRecorder::new(
        config.as_ref(),
        RolloutRecorderParams::new(
            ThreadId::default(),
            None,
            SessionSource::Exec,
            BaseInstructions::default(),
            Vec::new(),
            EventPersistenceMode::Limited,
        ),
        None,
        None,
    )
    .await
    .expect("create rollout recorder");
    let rollout_path = recorder.rollout_path().to_path_buf();
    {
        let mut rollout = session.services.rollout.lock().await;
        *rollout = Some(recorder);
    }

    session
        .persist_rollout_items(&[RolloutItem::EventMsg(EventMsg::UserMessage(
            UserMessageEvent {
                message: "seed rollout".to_string(),
                images: None,
                local_images: Vec::new(),
                text_elements: Vec::new(),
            },
        ))])
        .await;
    {
        let mut state = session.state.lock().await;
        state.set_reference_context_item(None);
    }

    session
        .set_previous_turn_settings(Some(PreviousTurnSettings {
            model: previous_context.model_info.slug.clone(),
            realtime_active: Some(previous_context.realtime_active),
        }))
        .await;
    session
        .record_context_updates_and_set_reference_context_item(&turn_context)
        .await;
    session.ensure_rollout_materialized().await;
    session.flush_rollout().await;

    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let persisted_turn_context = resumed.history.iter().find_map(|item| match item {
        RolloutItem::TurnContext(ctx) => Some(ctx.clone()),
        _ => None,
    });

    assert_eq!(
        serde_json::to_value(persisted_turn_context)
            .expect("serialize persisted turn context item"),
        serde_json::to_value(Some(turn_context.to_turn_context_item()))
            .expect("serialize expected turn context item")
    );
}

#[tokio::test]
async fn run_user_shell_command_does_not_set_reference_context_item() {
    let (session, _turn_context, rx) = make_session_and_context_with_rx().await;
    {
        let mut state = session.state.lock().await;
        state.set_reference_context_item(None);
    }

    handlers::run_user_shell_command(&session, "sub-id".to_string(), "echo shell".to_string())
        .await;

    let deadline = StdDuration::from_secs(15);
    let start = std::time::Instant::now();
    loop {
        let remaining = deadline.saturating_sub(start.elapsed());
        let evt = tokio::time::timeout(remaining, rx.recv())
            .await
            .expect("timeout waiting for event")
            .expect("event");
        if matches!(evt.msg, EventMsg::TurnComplete(_)) {
            break;
        }
    }

    assert!(
        session.reference_context_item().await.is_none(),
        "standalone shell tasks should not mutate previous context"
    );
}

#[derive(Clone, Copy)]
struct NeverEndingTask {
    kind: TaskKind,
    listen_to_cancellation_token: bool,
}

#[async_trait::async_trait]
impl SessionTask for NeverEndingTask {
    fn kind(&self) -> TaskKind {
        self.kind
    }

    fn span_name(&self) -> &'static str {
        "session_task.never_ending"
    }

    async fn run(
        self: Arc<Self>,
        _session: Arc<SessionTaskContext>,
        _ctx: Arc<TurnContext>,
        _input: Vec<UserInput>,
        cancellation_token: CancellationToken,
    ) -> Option<String> {
        if self.listen_to_cancellation_token {
            cancellation_token.cancelled().await;
            return None;
        }
        loop {
            sleep(Duration::from_secs(60)).await;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[test_log::test]
async fn abort_regular_task_emits_turn_aborted_only() {
    let (sess, tc, rx) = make_session_and_context_with_rx().await;
    let input = vec![UserInput::Text {
        text: "hello".to_string(),
        text_elements: Vec::new(),
    }];
    sess.spawn_task(
        Arc::clone(&tc),
        input,
        NeverEndingTask {
            kind: TaskKind::Regular,
            listen_to_cancellation_token: false,
        },
    )
    .await;

    sess.abort_all_tasks(TurnAbortReason::Interrupted).await;

    // Interrupts persist a model-visible `<turn_aborted>` marker into history, but there is no
    // separate client-visible event for that marker (only `EventMsg::TurnAborted`).
    let evt = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("timeout waiting for event")
        .expect("event");
    match evt.msg {
        EventMsg::TurnAborted(e) => assert_eq!(TurnAbortReason::Interrupted, e.reason),
        other => panic!("unexpected event: {other:?}"),
    }
    // No extra events should be emitted after an abort.
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn abort_gracefully_emits_turn_aborted_only() {
    let (sess, tc, rx) = make_session_and_context_with_rx().await;
    let input = vec![UserInput::Text {
        text: "hello".to_string(),
        text_elements: Vec::new(),
    }];
    sess.spawn_task(
        Arc::clone(&tc),
        input,
        NeverEndingTask {
            kind: TaskKind::Regular,
            listen_to_cancellation_token: true,
        },
    )
    .await;

    sess.abort_all_tasks(TurnAbortReason::Interrupted).await;

    // Even if tasks handle cancellation gracefully, interrupts still result in `TurnAborted`
    // being the only client-visible signal.
    let evt = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("timeout waiting for event")
        .expect("event");
    match evt.msg {
        EventMsg::TurnAborted(e) => assert_eq!(TurnAbortReason::Interrupted, e.reason),
        other => panic!("unexpected event: {other:?}"),
    }
    // No extra events should be emitted after an abort.
    assert!(rx.try_recv().is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn task_finish_emits_turn_item_lifecycle_for_leftover_pending_user_input() {
    let (sess, tc, rx) = make_session_and_context_with_rx().await;
    let input = vec![UserInput::Text {
        text: "hello".to_string(),
        text_elements: Vec::new(),
    }];
    sess.spawn_task(
        Arc::clone(&tc),
        input,
        NeverEndingTask {
            kind: TaskKind::Regular,
            listen_to_cancellation_token: false,
        },
    )
    .await;

    while rx.try_recv().is_ok() {}

    sess.inject_response_items(vec![ResponseInputItem::Message {
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "late pending input".to_string(),
        }],
    }])
    .await
    .expect("inject pending input into active turn");

    sess.on_task_finished(Arc::clone(&tc), None).await;

    let history = sess.clone_history().await;
    let expected = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "late pending input".to_string(),
        }],
        end_turn: None,
        phase: None,
    };
    assert!(
        history.raw_items().iter().any(|item| item == &expected),
        "expected pending input to be persisted into history on turn completion"
    );

    let first = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("expected raw response item event")
        .expect("channel open");
    assert!(matches!(first.msg, EventMsg::RawResponseItem(_)));

    let second = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("expected item started event")
        .expect("channel open");
    assert!(matches!(
        second.msg,
        EventMsg::ItemStarted(ItemStartedEvent {
            item: TurnItem::UserMessage(UserMessageItem { content, .. }),
            ..
        }) if content == vec![UserInput::Text {
            text: "late pending input".to_string(),
            text_elements: Vec::new(),
        }]
    ));

    let third = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("expected item completed event")
        .expect("channel open");
    assert!(matches!(
        third.msg,
        EventMsg::ItemCompleted(ItemCompletedEvent {
            item: TurnItem::UserMessage(UserMessageItem { content, .. }),
            ..
        }) if content == vec![UserInput::Text {
            text: "late pending input".to_string(),
            text_elements: Vec::new(),
        }]
    ));

    let fourth = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("expected legacy user message event")
        .expect("channel open");
    assert!(matches!(
        fourth.msg,
        EventMsg::UserMessage(UserMessageEvent {
            message,
            images,
            text_elements,
            local_images,
        }) if message == "late pending input"
            && images == Some(Vec::new())
            && text_elements.is_empty()
            && local_images.is_empty()
    ));

    let fifth = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("expected turn complete event")
        .expect("channel open");
    assert!(matches!(
        fifth.msg,
        EventMsg::TurnComplete(TurnCompleteEvent {
            turn_id,
            last_agent_message: None,
        }) if turn_id == tc.sub_id
    ));
}

#[tokio::test]
async fn steer_input_requires_active_turn() {
    let (sess, _tc, _rx) = make_session_and_context_with_rx().await;
    let input = vec![UserInput::Text {
        text: "steer".to_string(),
        text_elements: Vec::new(),
    }];

    let err = sess
        .steer_input(input, None)
        .await
        .expect_err("steering without active turn should fail");

    assert!(matches!(err, SteerInputError::NoActiveTurn(_)));
}

#[tokio::test]
async fn steer_input_enforces_expected_turn_id() {
    let (sess, tc, _rx) = make_session_and_context_with_rx().await;
    let input = vec![UserInput::Text {
        text: "hello".to_string(),
        text_elements: Vec::new(),
    }];
    sess.spawn_task(
        Arc::clone(&tc),
        input,
        NeverEndingTask {
            kind: TaskKind::Regular,
            listen_to_cancellation_token: false,
        },
    )
    .await;

    let steer_input = vec![UserInput::Text {
        text: "steer".to_string(),
        text_elements: Vec::new(),
    }];
    let err = sess
        .steer_input(steer_input, Some("different-turn-id"))
        .await
        .expect_err("mismatched expected turn id should fail");

    match err {
        SteerInputError::ExpectedTurnMismatch { expected, actual } => {
            assert_eq!(
                (expected, actual),
                ("different-turn-id".to_string(), tc.sub_id.clone())
            );
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[tokio::test]
async fn steer_input_returns_active_turn_id() {
    let (sess, tc, _rx) = make_session_and_context_with_rx().await;
    let input = vec![UserInput::Text {
        text: "hello".to_string(),
        text_elements: Vec::new(),
    }];
    sess.spawn_task(
        Arc::clone(&tc),
        input,
        NeverEndingTask {
            kind: TaskKind::Regular,
            listen_to_cancellation_token: false,
        },
    )
    .await;

    let steer_input = vec![UserInput::Text {
        text: "steer".to_string(),
        text_elements: Vec::new(),
    }];
    let turn_id = sess
        .steer_input(steer_input, Some(&tc.sub_id))
        .await
        .expect("steering with matching expected turn id should succeed");

    assert_eq!(turn_id, tc.sub_id);
    assert!(sess.has_pending_input().await);
}

#[tokio::test]
async fn prepend_pending_input_keeps_older_tail_ahead_of_newer_input() {
    let (sess, tc, _rx) = make_session_and_context_with_rx().await;
    let input = vec![UserInput::Text {
        text: "hello".to_string(),
        text_elements: Vec::new(),
    }];
    sess.spawn_task(
        Arc::clone(&tc),
        input,
        NeverEndingTask {
            kind: TaskKind::Regular,
            listen_to_cancellation_token: false,
        },
    )
    .await;

    let blocked = ResponseInputItem::Message {
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "blocked queued prompt".to_string(),
        }],
    };
    let later = ResponseInputItem::Message {
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "later queued prompt".to_string(),
        }],
    };
    let newer = ResponseInputItem::Message {
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "newer queued prompt".to_string(),
        }],
    };

    sess.inject_response_items(vec![blocked.clone(), later.clone()])
        .await
        .expect("inject initial pending input into active turn");

    let drained = sess.get_pending_input().await;
    assert_eq!(drained, vec![blocked, later.clone()]);

    sess.inject_response_items(vec![newer.clone()])
        .await
        .expect("inject newer pending input into active turn");

    let mut drained_iter = drained.into_iter();
    let _blocked = drained_iter.next().expect("blocked prompt should exist");
    sess.prepend_pending_input(drained_iter.collect())
        .await
        .expect("requeue later pending input at the front of the queue");

    assert_eq!(sess.get_pending_input().await, vec![later, newer]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn abort_review_task_emits_exited_then_aborted_and_records_history() {
    let (sess, tc, rx) = make_session_and_context_with_rx().await;
    let input = vec![UserInput::Text {
        text: "start review".to_string(),
        text_elements: Vec::new(),
    }];
    sess.spawn_task(Arc::clone(&tc), input, ReviewTask::new())
        .await;

    sess.abort_all_tasks(TurnAbortReason::Interrupted).await;

    // Aborting a review task should exit review mode before surfacing the abort to the client.
    // We scan for these events (rather than relying on fixed ordering) since unrelated events
    // may interleave.
    let mut exited_review_mode_idx = None;
    let mut turn_aborted_idx = None;
    let mut idx = 0usize;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let evt = tokio::time::timeout(remaining, rx.recv())
            .await
            .expect("timeout waiting for event")
            .expect("event");
        let event_idx = idx;
        idx = idx.saturating_add(1);
        match evt.msg {
            EventMsg::ExitedReviewMode(ev) => {
                assert!(ev.review_output.is_none());
                exited_review_mode_idx = Some(event_idx);
            }
            EventMsg::TurnAborted(ev) => {
                assert_eq!(TurnAbortReason::Interrupted, ev.reason);
                turn_aborted_idx = Some(event_idx);
                break;
            }
            _ => {}
        }
    }
    assert!(
        exited_review_mode_idx.is_some(),
        "expected ExitedReviewMode after abort"
    );
    assert!(
        turn_aborted_idx.is_some(),
        "expected TurnAborted after abort"
    );
    assert!(
        exited_review_mode_idx.unwrap() < turn_aborted_idx.unwrap(),
        "expected ExitedReviewMode before TurnAborted"
    );

    let history = sess.clone_history().await;
    // The `<turn_aborted>` marker is silent in the event stream, so verify it is still
    // recorded in history for the model.
    assert!(
        history.raw_items().iter().any(|item| {
            let ResponseItem::Message { role, content, .. } = item else {
                return false;
            };
            if role != "user" {
                return false;
            }
            content.iter().any(|content_item| {
                let ContentItem::InputText { text } = content_item else {
                    return false;
                };
                text.contains(crate::contextual_user_message::TURN_ABORTED_OPEN_TAG)
            })
        }),
        "expected a model-visible turn aborted marker in history after interrupt"
    );
}

#[tokio::test]
async fn fatal_tool_error_stops_turn_and_reports_error() {
    let (session, turn_context, _rx) = make_session_and_context_with_rx().await;
    let tools = {
        session
            .services
            .mcp_connection_manager
            .read()
            .await
            .list_all_tools()
            .await
    };
    let app_tools = Some(tools.clone());
    let router = ToolRouter::from_config(
        &turn_context.tools_config,
        crate::tools::router::ToolRouterParams {
            mcp_tools: Some(
                tools
                    .into_iter()
                    .map(|(name, tool)| (name, tool.tool))
                    .collect(),
            ),
            app_tools,
            discoverable_tools: None,
            dynamic_tools: turn_context.dynamic_tools.as_slice(),
        },
    );
    let item = ResponseItem::CustomToolCall {
        id: None,
        status: None,
        call_id: "call-1".to_string(),
        name: "shell".to_string(),
        input: "{}".to_string(),
    };

    let call = ToolRouter::build_tool_call(session.as_ref(), item.clone())
        .await
        .expect("build tool call")
        .expect("tool call present");
    let tracker = Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new()));
    let err = router
        .dispatch_tool_call(
            Arc::clone(&session),
            Arc::clone(&turn_context),
            tracker,
            call,
            ToolCallSource::Direct,
        )
        .await
        .expect_err("expected fatal error");

    match err {
        FunctionCallError::Fatal(message) => {
            assert_eq!(message, "tool shell invoked with incompatible payload");
        }
        other => panic!("expected FunctionCallError::Fatal, got {other:?}"),
    }
}

async fn sample_rollout(
    session: &Session,
    _turn_context: &TurnContext,
) -> (Vec<RolloutItem>, Vec<ResponseItem>) {
    let mut rollout_items = Vec::new();
    let mut live_history = ContextManager::new();

    // Use the same turn_context source as record_initial_history so model_info (and thus
    // personality_spec) matches reconstruction.
    let reconstruction_turn = session.new_default_turn().await;
    let mut initial_context = session
        .build_initial_context(reconstruction_turn.as_ref())
        .await;
    // Ensure personality_spec is present when Personality is enabled, so expected matches
    // what reconstruction produces (build_initial_context may omit it when baked into model).
    if !initial_context.iter().any(|m| {
        matches!(m, ResponseItem::Message { role, content, .. }
        if role == "developer"
            && content.iter().any(|c| {
                matches!(c, ContentItem::InputText { text } if text.contains("<personality_spec>"))
            }))
    }) && let Some(p) = reconstruction_turn.personality
        && session.features.enabled(Feature::Personality)
        && let Some(personality_message) = reconstruction_turn
            .model_info
            .model_messages
            .as_ref()
            .and_then(|m| m.get_personality_message(Some(p)).filter(|s| !s.is_empty()))
    {
        let msg = DeveloperInstructions::personality_spec_message(personality_message).into();
        let insert_at = initial_context
            .iter()
            .position(|m| matches!(m, ResponseItem::Message { role, .. } if role == "developer"))
            .map(|i| i + 1)
            .unwrap_or(0);
        initial_context.insert(insert_at, msg);
    }
    for item in &initial_context {
        rollout_items.push(RolloutItem::ResponseItem(item.clone()));
    }
    live_history.record_items(
        initial_context.iter(),
        reconstruction_turn.truncation_policy,
    );

    let user1 = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "first user".to_string(),
        }],
        end_turn: None,
        phase: None,
    };
    live_history.record_items(
        std::iter::once(&user1),
        reconstruction_turn.truncation_policy,
    );
    rollout_items.push(RolloutItem::ResponseItem(user1.clone()));

    let assistant1 = ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: "assistant reply one".to_string(),
        }],
        end_turn: None,
        phase: None,
    };
    live_history.record_items(
        std::iter::once(&assistant1),
        reconstruction_turn.truncation_policy,
    );
    rollout_items.push(RolloutItem::ResponseItem(assistant1.clone()));

    let summary1 = "summary one";
    let snapshot1 = live_history
        .clone()
        .for_prompt(&reconstruction_turn.model_info.input_modalities);
    let user_messages1 = collect_user_messages(&snapshot1);
    let rebuilt1 = compact::build_compacted_history(Vec::new(), &user_messages1, summary1);
    live_history.replace(rebuilt1);
    rollout_items.push(RolloutItem::Compacted(CompactedItem {
        message: summary1.to_string(),
        replacement_history: None,
        prompt_gc: None,
    }));

    let user2 = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "second user".to_string(),
        }],
        end_turn: None,
        phase: None,
    };
    live_history.record_items(
        std::iter::once(&user2),
        reconstruction_turn.truncation_policy,
    );
    rollout_items.push(RolloutItem::ResponseItem(user2.clone()));

    let assistant2 = ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: "assistant reply two".to_string(),
        }],
        end_turn: None,
        phase: None,
    };
    live_history.record_items(
        std::iter::once(&assistant2),
        reconstruction_turn.truncation_policy,
    );
    rollout_items.push(RolloutItem::ResponseItem(assistant2.clone()));

    let summary2 = "summary two";
    let snapshot2 = live_history
        .clone()
        .for_prompt(&reconstruction_turn.model_info.input_modalities);
    let user_messages2 = collect_user_messages(&snapshot2);
    let rebuilt2 = compact::build_compacted_history(Vec::new(), &user_messages2, summary2);
    live_history.replace(rebuilt2);
    rollout_items.push(RolloutItem::Compacted(CompactedItem {
        message: summary2.to_string(),
        replacement_history: None,
        prompt_gc: None,
    }));

    let user3 = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "third user".to_string(),
        }],
        end_turn: None,
        phase: None,
    };
    live_history.record_items(
        std::iter::once(&user3),
        reconstruction_turn.truncation_policy,
    );
    rollout_items.push(RolloutItem::ResponseItem(user3));

    let assistant3 = ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: "assistant reply three".to_string(),
        }],
        end_turn: None,
        phase: None,
    };
    live_history.record_items(
        std::iter::once(&assistant3),
        reconstruction_turn.truncation_policy,
    );
    rollout_items.push(RolloutItem::ResponseItem(assistant3));

    (
        rollout_items,
        live_history.for_prompt(&reconstruction_turn.model_info.input_modalities),
    )
}

#[tokio::test]
async fn rejects_escalated_permissions_when_policy_not_on_request() {
    use crate::exec::ExecParams;
    use crate::protocol::AskForApproval;
    use crate::protocol::SandboxPolicy;
    use crate::sandboxing::SandboxPermissions;
    use crate::turn_diff_tracker::TurnDiffTracker;
    use std::collections::HashMap;

    let (session, mut turn_context_raw) = make_session_and_context().await;
    // Ensure policy is NOT OnRequest so the early rejection path triggers
    turn_context_raw
        .approval_policy
        .set(AskForApproval::OnFailure)
        .expect("test setup should allow updating approval policy");
    let session = Arc::new(session);
    let mut turn_context = Arc::new(turn_context_raw);

    let timeout_ms = 1000;
    let sandbox_permissions = SandboxPermissions::RequireEscalated;
    let params = ExecParams {
        command: if cfg!(windows) {
            vec![
                "cmd.exe".to_string(),
                "/C".to_string(),
                "echo hi".to_string(),
            ]
        } else {
            vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "echo hi".to_string(),
            ]
        },
        cwd: turn_context.cwd.clone(),
        expiration: timeout_ms.into(),
        env: HashMap::new(),
        network: None,
        sandbox_permissions,
        windows_sandbox_level: turn_context.windows_sandbox_level,
        windows_sandbox_private_desktop: turn_context
            .config
            .permissions
            .windows_sandbox_private_desktop,
        justification: Some("test".to_string()),
        arg0: None,
    };

    let params2 = ExecParams {
        sandbox_permissions: SandboxPermissions::UseDefault,
        command: params.command.clone(),
        cwd: params.cwd.clone(),
        expiration: timeout_ms.into(),
        env: HashMap::new(),
        network: None,
        windows_sandbox_level: turn_context.windows_sandbox_level,
        windows_sandbox_private_desktop: turn_context
            .config
            .permissions
            .windows_sandbox_private_desktop,
        justification: params.justification.clone(),
        arg0: None,
    };

    let turn_diff_tracker = Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new()));

    let tool_name = "shell";
    let call_id = "test-call".to_string();

    let handler = ShellHandler;
    let resp = handler
        .handle(ToolInvocation {
            session: Arc::clone(&session),
            turn: Arc::clone(&turn_context),
            tracker: Arc::clone(&turn_diff_tracker),
            call_id,
            tool_name: tool_name.to_string(),
            tool_namespace: None,
            payload: ToolPayload::Function {
                arguments: serde_json::json!({
                    "command": params.command.clone(),
                    "workdir": Some(turn_context.cwd.to_string_lossy().to_string()),
                    "timeout_ms": params.expiration.timeout_ms(),
                    "sandbox_permissions": params.sandbox_permissions,
                    "justification": params.justification.clone(),
                })
                .to_string(),
            },
        })
        .await;

    let Err(FunctionCallError::RespondToModel(output)) = resp else {
        panic!("expected error result");
    };

    let expected = format!(
        "approval policy is {policy:?}; reject command — you should not ask for escalated permissions if the approval policy is {policy:?}",
        policy = turn_context.approval_policy.value()
    );

    pretty_assertions::assert_eq!(output, expected);

    // Now retry the same command WITHOUT escalated permissions; should succeed.
    // Force DangerFullAccess to avoid platform sandbox dependencies in tests.
    let turn_context_mut = Arc::get_mut(&mut turn_context).expect("unique turn context Arc");
    turn_context_mut
        .sandbox_policy
        .set(SandboxPolicy::DangerFullAccess)
        .expect("test setup should allow updating sandbox policy");
    turn_context_mut.file_system_sandbox_policy =
        FileSystemSandboxPolicy::from(turn_context_mut.sandbox_policy.get());
    turn_context_mut.network_sandbox_policy =
        NetworkSandboxPolicy::from(turn_context_mut.sandbox_policy.get());

    let resp2 = handler
        .handle(ToolInvocation {
            session: Arc::clone(&session),
            turn: Arc::clone(&turn_context),
            tracker: Arc::clone(&turn_diff_tracker),
            call_id: "test-call-2".to_string(),
            tool_name: tool_name.to_string(),
            tool_namespace: None,
            payload: ToolPayload::Function {
                arguments: serde_json::json!({
                    "command": params2.command.clone(),
                    "workdir": Some(turn_context.cwd.to_string_lossy().to_string()),
                    "timeout_ms": params2.expiration.timeout_ms(),
                    "sandbox_permissions": params2.sandbox_permissions,
                    "justification": params2.justification.clone(),
                })
                .to_string(),
            },
        })
        .await;

    let output = expect_text_tool_output(&resp2.expect("expected Ok result"));

    #[derive(Deserialize, PartialEq, Eq, Debug)]
    struct ResponseExecMetadata {
        exit_code: i32,
    }

    #[derive(Deserialize)]
    struct ResponseExecOutput {
        output: String,
        metadata: ResponseExecMetadata,
    }

    let exec_output: ResponseExecOutput =
        serde_json::from_str(&output).expect("valid exec output json");

    pretty_assertions::assert_eq!(exec_output.metadata, ResponseExecMetadata { exit_code: 0 });
    assert!(exec_output.output.contains("hi"));
}
#[tokio::test]
async fn unified_exec_rejects_escalated_permissions_when_policy_not_on_request() {
    use crate::protocol::AskForApproval;
    use crate::sandboxing::SandboxPermissions;
    use crate::turn_diff_tracker::TurnDiffTracker;

    let (session, mut turn_context_raw) = make_session_and_context().await;
    turn_context_raw
        .approval_policy
        .set(AskForApproval::OnFailure)
        .expect("test setup should allow updating approval policy");
    let session = Arc::new(session);
    let turn_context = Arc::new(turn_context_raw);
    let tracker = Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new()));

    let handler = UnifiedExecHandler;
    let resp = handler
        .handle(ToolInvocation {
            session: Arc::clone(&session),
            turn: Arc::clone(&turn_context),
            tracker: Arc::clone(&tracker),
            call_id: "exec-call".to_string(),
            tool_name: "exec_command".to_string(),
            tool_namespace: None,
            payload: ToolPayload::Function {
                arguments: serde_json::json!({
                    "cmd": "echo hi",
                    "sandbox_permissions": SandboxPermissions::RequireEscalated,
                    "justification": "need unsandboxed execution",
                })
                .to_string(),
            },
        })
        .await;

    let Err(FunctionCallError::RespondToModel(output)) = resp else {
        panic!("expected error result");
    };

    let expected = format!(
        "approval policy is {policy:?}; reject command — you cannot ask for escalated permissions if the approval policy is {policy:?}",
        policy = turn_context.approval_policy.value()
    );

    pretty_assertions::assert_eq!(output, expected);
}
