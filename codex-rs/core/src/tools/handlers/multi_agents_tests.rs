use super::*;
use crate::CodexThread;
use crate::ThreadManager;
use crate::agent::AgentRuntimeState;
use crate::agent::role::apply_role_to_config;
use crate::codex::make_session_and_context;
use crate::config::DEFAULT_AGENT_MAX_DEPTH;
use crate::function_tool::FunctionCallError;
use crate::session_prefix::format_subagent_notification_message;
use crate::state::TaskKind;
use crate::subagent_file_mutation::apply_file_mutation_mode_to_config;
use crate::subagent_file_mutation::denied_action_message;
use crate::tasks::SessionTask;
use crate::tasks::SessionTaskContext;
use crate::tools::context::ToolOutput;
use crate::tools::handlers::ApplyPatchHandler;
use crate::tools::handlers::multi_agents_v2::CloseAgentHandler as CloseAgentHandlerV2;
use crate::tools::handlers::multi_agents_v2::FollowupTaskHandler as FollowupTaskHandlerV2;
use crate::tools::handlers::multi_agents_v2::ListAgentsHandler as ListAgentsHandlerV2;
use crate::tools::handlers::multi_agents_v2::SendMessageHandler as SendMessageHandlerV2;
use crate::tools::handlers::multi_agents_v2::SpawnAgentHandler as SpawnAgentHandlerV2;
use crate::tools::handlers::multi_agents_v2::WaitAgentHandler as WaitAgentHandlerV2;
use crate::turn_diff_tracker::TurnDiffTracker;
use codex_config::CONFIG_TOML_FILE;
use codex_config::types::ShellEnvironmentPolicy;
use codex_features::Feature;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_model_provider_info::built_in_model_providers;
use codex_protocol::AgentPath;
use codex_protocol::ThreadId;
use codex_protocol::config_types::SubagentFileMutationMode;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::AgentStatus;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::ErrorEvent;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::FileSystemSandboxPolicy;
use codex_protocol::protocol::InitialHistory;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::protocol::NetworkSandboxPolicy;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::SandboxPolicy;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::protocol::TurnAbortReason;
use codex_protocol::protocol::TurnAbortedEvent;
use codex_protocol::protocol::TurnCompleteEvent;
use codex_protocol::user_input::UserInput;
use codex_utils_absolute_path::AbsolutePathBuf;
use core_test_support::TempDirExt;
use pretty_assertions::assert_eq;
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::sync::watch::Receiver;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use toml::toml;

// Merge-safety anchor: spawn-agent handler tests here must keep the lead-inherited-vs-profile
// contract fail-loud and must not reintroduce direct `model`/`reasoning_effort` tool args.
fn invocation(
    session: Arc<crate::codex::Session>,
    turn: Arc<TurnContext>,
    tool_name: &str,
    payload: ToolPayload,
) -> ToolInvocation {
    ToolInvocation {
        session,
        turn,
        tracker: Arc::new(Mutex::new(TurnDiffTracker::default())),
        call_id: "call-1".to_string(),
        tool_name: tool_name.to_string(),
        tool_namespace: None,
        payload,
    }
}

fn function_payload(args: serde_json::Value) -> ToolPayload {
    ToolPayload::Function {
        arguments: args.to_string(),
    }
}

fn parse_agent_id(id: &str) -> ThreadId {
    ThreadId::from_string(id).expect("agent id should be valid")
}

async fn assert_child_file_mutation_denied(child_thread: &CodexThread) {
    let snapshot = child_thread.config_snapshot().await;
    assert_eq!(
        snapshot.subagent_file_mutation_mode,
        SubagentFileMutationMode::Deny
    );
    assert!(matches!(
        snapshot.sandbox_policy,
        SandboxPolicy::ReadOnly { .. }
    ));
    assert!(
        !child_thread
            .codex
            .session
            .new_default_turn()
            .await
            .file_system_sandbox_policy
            .has_full_disk_write_access()
    );

    let child_turn = child_thread.codex.session.new_default_turn().await;
    let err = match ApplyPatchHandler
        .handle(invocation(
            child_thread.codex.session.clone(),
            child_turn,
            "apply_patch",
            function_payload(json!({
                "input": "*** Begin Patch\n*** End Patch\n",
            })),
        ))
        .await
    {
        Ok(_) => panic!("apply_patch should be rejected"),
        Err(err) => err,
    };
    assert_eq!(
        err,
        FunctionCallError::RespondToModel(denied_action_message(
            "this subagent cannot apply patches",
        ))
    );
}

fn thread_manager() -> ThreadManager {
    ThreadManager::with_models_provider_for_tests(
        CodexAuth::from_api_key("dummy"),
        built_in_model_providers(/* openai_base_url */ /*openai_base_url*/ None)["openai"].clone(),
    )
}

fn history_contains_inter_agent_communication(
    history_items: &[ResponseItem],
    expected: &InterAgentCommunication,
) -> bool {
    history_items.iter().any(|item| {
        let ResponseItem::Message { role, content, .. } = item else {
            return false;
        };
        if role != "assistant" {
            return false;
        }
        content.iter().any(|content_item| match content_item {
            ContentItem::OutputText { text } => {
                serde_json::from_str::<InterAgentCommunication>(text)
                    .ok()
                    .as_ref()
                    == Some(expected)
            }
            ContentItem::InputText { .. } | ContentItem::InputImage { .. } => false,
        })
    })
}

async fn wait_for_turn_aborted(
    thread: &Arc<CodexThread>,
    expected_turn_id: &str,
    expected_reason: TurnAbortReason,
) {
    timeout(Duration::from_secs(5), async {
        loop {
            let event = thread
                .next_event()
                .await
                .expect("child thread should emit events");
            if matches!(
                event.msg,
                EventMsg::TurnAborted(TurnAbortedEvent {
                    turn_id: Some(ref turn_id),
                    ref reason,
                    ..
                }) if turn_id == expected_turn_id && *reason == expected_reason
            ) {
                break;
            }
        }
    })
    .await
    .expect("expected child turn to be interrupted");
}

async fn wait_for_redirected_envelope_in_history(
    thread: &Arc<CodexThread>,
    expected: &InterAgentCommunication,
) {
    timeout(Duration::from_secs(5), async {
        loop {
            let history_items = thread
                .codex
                .session
                .clone_history()
                .await
                .raw_items()
                .to_vec();
            let saw_envelope =
                history_contains_inter_agent_communication(&history_items, expected);
            let saw_user_message = history_items.iter().any(|item| {
                matches!(
                    item,
                    ResponseItem::Message { role, content, .. }
                        if role == "user"
                            && content.iter().any(|content_item| matches!(
                                content_item,
                                ContentItem::InputText { text }
                                    if text == &expected.content
                            ))
                )
            });
            if saw_envelope {
                assert!(
                    !saw_user_message,
                    "redirected followup should be stored as an assistant envelope, not a plain user message"
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("redirected followup envelope should appear in history");
}

#[derive(Clone, Copy)]
struct NeverEndingTask;

impl SessionTask for NeverEndingTask {
    fn kind(&self) -> TaskKind {
        TaskKind::Regular
    }

    fn span_name(&self) -> &'static str {
        "session_task.multi_agent_never_ending"
    }

    async fn run(
        self: Arc<Self>,
        _session: Arc<SessionTaskContext>,
        _ctx: Arc<TurnContext>,
        _input: Vec<UserInput>,
        cancellation_token: CancellationToken,
    ) -> Option<String> {
        cancellation_token.cancelled().await;
        None
    }
}

fn expect_text_output<T>(output: T) -> (String, Option<bool>)
where
    T: ToolOutput,
{
    let response = output.to_response_item(
        "call-1",
        &ToolPayload::Function {
            arguments: "{}".to_string(),
        },
    );
    match response {
        ResponseInputItem::FunctionCallOutput { output, .. }
        | ResponseInputItem::CustomToolCallOutput { output, .. } => {
            let content = match output.body {
                FunctionCallOutputBody::Text(text) => text,
                FunctionCallOutputBody::ContentItems(items) => {
                    codex_protocol::models::function_call_output_content_items_to_text(&items)
                        .unwrap_or_default()
                }
            };
            (content, output.success)
        }
        other => panic!("expected function output, got {other:?}"),
    }
}

fn assert_shutdown_activity(state: &AgentRuntimeState) {
    let activity = state
        .last_activity
        .as_ref()
        .expect("shutdown state should retain last activity");
    assert_eq!(
        activity.kind,
        codex_protocol::protocol::CollabAgentActivityKind::Status
    );
    assert_eq!(activity.summary, "Shutdown complete");
    assert!(activity.occurred_at > 0);
}

fn assert_agent_status(
    agents: &HashMap<ThreadId, AgentRuntimeState>,
    thread_id: ThreadId,
    expected_status: AgentStatus,
) -> &AgentRuntimeState {
    let state = agents
        .get(&thread_id)
        .unwrap_or_else(|| panic!("missing agent state for {thread_id}"));
    assert_eq!(state.status, expected_status);
    state
}

async fn wait_for_shutdown_status(status_rx: &mut Receiver<AgentStatus>) {
    loop {
        if status_rx.borrow().clone() == AgentStatus::Shutdown {
            return;
        }
        let changed = timeout(Duration::from_secs(1), status_rx.changed())
            .await
            .expect("shutdown status should arrive");
        changed.expect("status channel should stay open");
    }
}

async fn wait_for_errored_status(status_rx: &mut Receiver<AgentStatus>, expected_message: &str) {
    let expected_status = AgentStatus::Errored(expected_message.to_string());
    loop {
        if status_rx.borrow().clone() == expected_status {
            return;
        }
        let changed = timeout(Duration::from_secs(1), status_rx.changed())
            .await
            .expect("errored status should arrive");
        changed.expect("status channel should stay open");
    }
}

async fn mark_thread_errored(thread: &CodexThread, message: &str) {
    let turn = thread.codex.session.new_default_turn().await;
    thread
        .codex
        .session
        .send_event(
            turn.as_ref(),
            EventMsg::Error(ErrorEvent {
                message: message.to_string(),
                codex_error_info: None,
            }),
        )
        .await;
}

#[derive(Debug, Deserialize)]
struct ListAgentsResult {
    agents: Vec<ListedAgentResult>,
}

#[derive(Debug, Deserialize)]
struct ListedAgentResult {
    agent_name: String,
    agent_status: serde_json::Value,
    last_task_message: Option<String>,
}

#[tokio::test]
async fn handler_rejects_non_function_payloads() {
    let (session, turn) = make_session_and_context().await;
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "spawn_agent",
        ToolPayload::Custom {
            input: "hello".to_string(),
        },
    );
    let Err(err) = SpawnAgentHandler.handle(invocation).await else {
        panic!("payload should be rejected");
    };
    assert_eq!(
        err,
        FunctionCallError::RespondToModel(
            "collab handler received unsupported payload".to_string()
        )
    );
}

#[tokio::test]
async fn spawn_agent_rejects_empty_message() {
    let (session, turn) = make_session_and_context().await;
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "spawn_agent",
        function_payload(json!({"message": "   "})),
    );
    let Err(err) = SpawnAgentHandler.handle(invocation).await else {
        panic!("empty message should be rejected");
    };
    assert_eq!(
        err,
        FunctionCallError::RespondToModel("Empty message can't be sent to an agent".to_string())
    );
}

#[tokio::test]
async fn spawn_agent_rejects_when_message_and_items_are_both_set() {
    let (session, turn) = make_session_and_context().await;
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "spawn_agent",
        function_payload(json!({
            "message": "hello",
            "items": [{"type": "mention", "name": "drive", "path": "app://drive"}]
        })),
    );
    let Err(err) = SpawnAgentHandler.handle(invocation).await else {
        panic!("message+items should be rejected");
    };
    assert_eq!(
        err,
        FunctionCallError::RespondToModel(
            "Provide either message or items, but not both".to_string()
        )
    );
}

#[tokio::test]
async fn spawn_agent_uses_explorer_role_and_preserves_approval_policy() {
    #[derive(Debug, Deserialize)]
    struct SpawnAgentResult {
        agent_id: String,
        nickname: Option<String>,
    }

    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();
    let mut config = (*turn.config).clone();
    let provider =
        built_in_model_providers(/* openai_base_url */ /*openai_base_url*/ None)["ollama"].clone();
    config.model_provider_id = "ollama".to_string();
    config.model_provider = provider.clone();
    config
        .permissions
        .approval_policy
        .set(AskForApproval::OnRequest)
        .expect("approval policy should be set");
    turn.approval_policy
        .set(AskForApproval::OnRequest)
        .expect("approval policy should be set");
    turn.provider = provider;
    turn.config = Arc::new(config);

    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "spawn_agent",
        function_payload(json!({
            "message": "inspect this repo",
            "agent_type": "explorer"
        })),
    );
    let output = SpawnAgentHandler
        .handle(invocation)
        .await
        .expect("spawn_agent should succeed");
    let (content, _) = expect_text_output(output);
    let result: SpawnAgentResult =
        serde_json::from_str(&content).expect("spawn_agent result should be json");
    let agent_id = parse_agent_id(&result.agent_id);
    assert!(
        result
            .nickname
            .as_deref()
            .is_some_and(|nickname| !nickname.is_empty())
    );
    let snapshot = manager
        .get_thread(agent_id)
        .await
        .expect("spawned agent thread should exist")
        .config_snapshot()
        .await;
    assert_eq!(snapshot.approval_policy, AskForApproval::OnRequest);
    assert_eq!(snapshot.model_provider_id, "ollama");
}

#[tokio::test]
async fn spawn_agent_returns_agent_id_without_task_name() {
    let (mut session, turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();

    let output = SpawnAgentHandler
        .handle(invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            function_payload(json!({
                "message": "inspect this repo"
            })),
        ))
        .await
        .expect("spawn_agent should succeed");
    let (content, success) = expect_text_output(output);
    let result: serde_json::Value =
        serde_json::from_str(&content).expect("spawn_agent result should be json");

    assert!(result["agent_id"].is_string());
    assert!(result.get("task_name").is_none());
    assert!(result.get("nickname").is_some());
    assert_eq!(success, Some(true));
}

#[tokio::test]
async fn multi_agent_v2_spawn_requires_task_name() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.conversation_id = root.thread_id;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    turn.config = Arc::new(config);

    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "spawn_agent",
        function_payload(json!({
            "message": "inspect this repo"
        })),
    );
    let Err(err) = SpawnAgentHandlerV2.handle(invocation).await else {
        panic!("missing task_name should be rejected");
    };
    let FunctionCallError::RespondToModel(message) = err else {
        panic!("missing task_name should surface as a model-facing error");
    };
    assert!(message.contains("missing field `task_name`"));
}

#[tokio::test]
async fn multi_agent_v2_spawn_rejects_legacy_items_field() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.conversation_id = root.thread_id;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    turn.config = Arc::new(config);

    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "spawn_agent",
        function_payload(json!({
            "message": "inspect this repo",
            "items": [{"type": "text", "text": "inspect this repo"}],
            "task_name": "worker"
        })),
    );
    let Err(err) = SpawnAgentHandlerV2.handle(invocation).await else {
        panic!("legacy items field should be rejected");
    };
    let FunctionCallError::RespondToModel(message) = err else {
        panic!("legacy items field should surface as a model-facing error");
    };
    assert!(message.contains("unknown field `items`"));
}

#[tokio::test]
async fn spawn_agent_errors_when_manager_dropped() {
    let (session, turn) = make_session_and_context().await;
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "spawn_agent",
        function_payload(json!({"message": "hello"})),
    );
    let Err(err) = SpawnAgentHandler.handle(invocation).await else {
        panic!("spawn should fail without a manager");
    };
    assert_eq!(
        err,
        FunctionCallError::RespondToModel("collab manager unavailable".to_string())
    );
}

#[tokio::test]
async fn spawn_agent_rejects_removed_model_and_reasoning_fields() {
    let (session, turn) = make_session_and_context().await;
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "spawn_agent",
        function_payload(json!({
            "message": "hello",
            "model": "gpt-5.2",
            "reasoning_effort": "medium",
        })),
    );
    let Err(FunctionCallError::RespondToModel(message)) =
        SpawnAgentHandler.handle(invocation).await
    else {
        panic!("removed fields should be rejected");
    };
    assert!(message.contains("unknown field"));
    assert!(message.contains("model"));
}

#[tokio::test]
async fn multi_agent_v2_spawn_returns_path_and_send_message_accepts_relative_path() {
    #[derive(Debug, Deserialize)]
    struct SpawnAgentResult {
        task_name: String,
        nickname: Option<String>,
    }

    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.conversation_id = root.thread_id;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    turn.config = Arc::new(config);

    let session = Arc::new(session);
    let turn = Arc::new(turn);
    let spawn_output = SpawnAgentHandlerV2
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "spawn_agent",
            function_payload(json!({
                "message": "inspect this repo",
                "task_name": "test_process"
            })),
        ))
        .await
        .expect("spawn_agent should succeed");
    let (content, _) = expect_text_output(spawn_output);
    let spawn_result: SpawnAgentResult =
        serde_json::from_str(&content).expect("spawn result should parse");
    assert_eq!(spawn_result.task_name, "/root/test_process");
    assert!(spawn_result.nickname.is_some());

    let child_thread_id = session
        .services
        .agent_control
        .resolve_agent_reference(
            session.conversation_id,
            &turn.session_source,
            "test_process",
        )
        .await
        .expect("relative path should resolve");
    let child_snapshot = manager
        .get_thread(child_thread_id)
        .await
        .expect("child thread should exist")
        .config_snapshot()
        .await;
    assert_eq!(
        child_snapshot.session_source.get_agent_path().as_deref(),
        Some("/root/test_process")
    );
    assert!(manager.captured_ops().iter().any(|(id, op)| {
        *id == child_thread_id
            && matches!(
                op,
                Op::InterAgentCommunication { communication }
                    if communication.author == AgentPath::root()
                        && communication.recipient.as_str() == "/root/test_process"
                        && communication.other_recipients.is_empty()
                        && communication.content == "inspect this repo"
                        && communication.trigger_turn
            )
    }));

    SendMessageHandlerV2
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "send_message",
            function_payload(json!({
                "target": "test_process",
                "message": "continue"
            })),
        ))
        .await
        .expect("send_message should accept v2 path");

    assert!(manager.captured_ops().iter().any(|(id, op)| {
        *id == child_thread_id
            && matches!(
                op,
                Op::InterAgentCommunication { communication }
                    if communication.author == AgentPath::root()
                        && communication.recipient.as_str() == "/root/test_process"
                        && communication.other_recipients.is_empty()
                        && communication.content == "continue"
                        && !communication.trigger_turn
            )
    }));
}

#[tokio::test]
async fn multi_agent_v2_spawn_rejects_legacy_fork_context() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.conversation_id = root.thread_id;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    turn.config = Arc::new(config);

    let err = SpawnAgentHandlerV2
        .handle(invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            function_payload(json!({
                "message": "inspect this repo",
                "task_name": "worker",
                "fork_context": true
            })),
        ))
        .await
        .expect_err("legacy fork_context should be rejected");

    assert_eq!(
        err,
        FunctionCallError::RespondToModel(
            "fork_context is not supported in MultiAgentV2; use fork_turns instead".to_string()
        )
    );
}

#[tokio::test]
async fn multi_agent_v2_spawn_rejects_invalid_fork_turns_string() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.conversation_id = root.thread_id;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    turn.config = Arc::new(config);

    let err = SpawnAgentHandlerV2
        .handle(invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            function_payload(json!({
                "message": "inspect this repo",
                "task_name": "worker",
                "fork_turns": "banana"
            })),
        ))
        .await
        .expect_err("invalid fork_turns should be rejected");

    assert_eq!(
        err,
        FunctionCallError::RespondToModel(
            "fork_turns must be `none`, `all`, or a positive integer string".to_string()
        )
    );
}

#[tokio::test]
async fn multi_agent_v2_spawn_rejects_zero_fork_turns() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.conversation_id = root.thread_id;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    turn.config = Arc::new(config);

    let err = SpawnAgentHandlerV2
        .handle(invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            function_payload(json!({
                "message": "inspect this repo",
                "task_name": "worker",
                "fork_turns": "0"
            })),
        ))
        .await
        .expect_err("zero turn count should be rejected");

    assert_eq!(
        err,
        FunctionCallError::RespondToModel(
            "fork_turns must be `none`, `all`, or a positive integer string".to_string()
        )
    );
}

#[tokio::test]
async fn multi_agent_v2_send_message_accepts_root_target_from_child() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.conversation_id = root.thread_id;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    turn.config = Arc::new(config);

    let child_path = AgentPath::try_from("/root/worker").expect("agent path");
    let child_thread_id = session
        .services
        .agent_control
        .spawn_agent_with_metadata(
            (*turn.config).clone(),
            vec![UserInput::Text {
                text: "inspect this repo".to_string(),
                text_elements: Vec::new(),
            }]
            .into(),
            Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id: root.thread_id,
                depth: 1,
                agent_path: Some(child_path.clone()),
                agent_nickname: None,
                agent_role: None,
            })),
            crate::agent::control::SpawnAgentOptions::default(),
        )
        .await
        .expect("worker spawn should succeed")
        .thread_id;
    session.conversation_id = child_thread_id;
    turn.session_source = SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
        parent_thread_id: root.thread_id,
        depth: 1,
        agent_path: Some(child_path.clone()),
        agent_nickname: None,
        agent_role: None,
    });

    SendMessageHandlerV2
        .handle(invocation(
            Arc::new(session),
            Arc::new(turn),
            "send_message",
            function_payload(json!({
                "target": "/root",
                "message": "done"
            })),
        ))
        .await
        .expect("send_message should accept the root agent path");

    assert!(manager.captured_ops().iter().any(|(id, op)| {
        *id == root.thread_id
            && matches!(
                op,
                Op::InterAgentCommunication { communication }
                    if communication.author == child_path
                        && communication.recipient == AgentPath::root()
                        && communication.other_recipients.is_empty()
                        && communication.content == "done"
                        && !communication.trigger_turn
            )
    }));
}

#[tokio::test]
async fn multi_agent_v2_followup_task_rejects_root_target_from_child() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.conversation_id = root.thread_id;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    turn.config = Arc::new(config);

    let child_path = AgentPath::try_from("/root/worker").expect("agent path");
    let child_thread_id = session
        .services
        .agent_control
        .spawn_agent_with_metadata(
            (*turn.config).clone(),
            vec![UserInput::Text {
                text: "inspect this repo".to_string(),
                text_elements: Vec::new(),
            }]
            .into(),
            Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id: root.thread_id,
                depth: 1,
                agent_path: Some(child_path.clone()),
                agent_nickname: None,
                agent_role: None,
            })),
            crate::agent::control::SpawnAgentOptions::default(),
        )
        .await
        .expect("worker spawn should succeed")
        .thread_id;
    session.conversation_id = child_thread_id;
    turn.session_source = SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
        parent_thread_id: root.thread_id,
        depth: 1,
        agent_path: Some(child_path),
        agent_nickname: None,
        agent_role: None,
    });

    let Err(err) = FollowupTaskHandlerV2
        .handle(invocation(
            Arc::new(session),
            Arc::new(turn),
            "followup_task",
            function_payload(json!({
                "target": "/root",
                "message": "run this",
                "interrupt": true
            })),
        ))
        .await
    else {
        panic!("followup_task should reject the root target");
    };

    assert_eq!(
        err,
        FunctionCallError::RespondToModel("Tasks can't be assigned to the root agent".to_string())
    );
    let root_ops = manager
        .captured_ops()
        .into_iter()
        .filter_map(|(id, op)| (id == root.thread_id).then_some(op))
        .collect::<Vec<_>>();
    assert!(!root_ops.iter().any(|op| matches!(op, Op::Interrupt)));
    assert!(
        !root_ops
            .iter()
            .any(|op| matches!(op, Op::InterAgentCommunication { .. }))
    );
}

#[tokio::test]
async fn multi_agent_v2_list_agents_returns_completed_status_and_last_task_message() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.conversation_id = root.thread_id;
    let mut config = (*turn.config).clone();
    let _ = config.features.enable(Feature::MultiAgentV2);
    turn.config = Arc::new(config);

    let session = Arc::new(session);
    let turn = Arc::new(turn);
    let spawn_output = SpawnAgentHandlerV2
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "spawn_agent",
            function_payload(json!({
                "message": "inspect this repo",
                "task_name": "worker"
            })),
        ))
        .await
        .expect("spawn_agent should succeed");
    let _ = expect_text_output(spawn_output);

    let agent_id = session
        .services
        .agent_control
        .resolve_agent_reference(session.conversation_id, &turn.session_source, "worker")
        .await
        .expect("worker path should resolve");
    let child_thread = manager
        .get_thread(agent_id)
        .await
        .expect("child thread should exist");
    let child_turn = child_thread.codex.session.new_default_turn().await;
    child_thread
        .codex
        .session
        .send_event(
            child_turn.as_ref(),
            EventMsg::TurnComplete(TurnCompleteEvent {
                turn_id: child_turn.sub_id.clone(),
                last_agent_message: Some("done".to_string()),
                completed_at: None,
                duration_ms: None,
            }),
        )
        .await;

    let output = ListAgentsHandlerV2
        .handle(invocation(
            session,
            turn,
            "list_agents",
            function_payload(json!({})),
        ))
        .await
        .expect("list_agents should succeed");
    let (content, success) = expect_text_output(output);
    let result: ListAgentsResult =
        serde_json::from_str(&content).expect("list_agents result should be json");

    let agent_names = result
        .agents
        .iter()
        .map(|agent| agent.agent_name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(agent_names, vec!["/root", "/root/worker"]);
    let root_agent = result
        .agents
        .iter()
        .find(|agent| agent.agent_name == "/root")
        .expect("root agent should be listed");
    assert_eq!(root_agent.last_task_message.as_deref(), Some("Main thread"));
    let worker = result
        .agents
        .iter()
        .find(|agent| agent.agent_name == "/root/worker")
        .expect("worker agent should be listed");
    assert_eq!(worker.agent_status, json!({"completed": "done"}));
    assert_eq!(
        worker.last_task_message.as_deref(),
        Some("inspect this repo")
    );
    assert_eq!(success, Some(true));
}

#[tokio::test]
async fn multi_agent_v2_list_agents_filters_by_relative_path_prefix() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.conversation_id = root.thread_id;
    let mut config = (*turn.config).clone();
    let _ = config.features.enable(Feature::MultiAgentV2);
    turn.config = Arc::new(config.clone());

    let researcher_path = AgentPath::from_string("/root/researcher".to_string()).expect("path");
    let worker_path = AgentPath::from_string("/root/researcher/worker".to_string()).expect("path");
    session
        .services
        .agent_control
        .spawn_agent_with_metadata(
            config.clone(),
            vec![UserInput::Text {
                text: "research".to_string(),
                text_elements: Vec::new(),
            }]
            .into(),
            Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id: root.thread_id,
                depth: 1,
                agent_path: Some(researcher_path.clone()),
                agent_nickname: None,
                agent_role: None,
            })),
            crate::agent::control::SpawnAgentOptions::default(),
        )
        .await
        .expect("researcher agent should spawn");
    session
        .services
        .agent_control
        .spawn_agent_with_metadata(
            config,
            vec![UserInput::Text {
                text: "build".to_string(),
                text_elements: Vec::new(),
            }]
            .into(),
            Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id: root.thread_id,
                depth: 2,
                agent_path: Some(worker_path.clone()),
                agent_nickname: None,
                agent_role: None,
            })),
            crate::agent::control::SpawnAgentOptions::default(),
        )
        .await
        .expect("worker agent should spawn");

    turn.session_source = SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
        parent_thread_id: root.thread_id,
        depth: 1,
        agent_path: Some(researcher_path),
        agent_nickname: None,
        agent_role: None,
    });

    let output = ListAgentsHandlerV2
        .handle(invocation(
            Arc::new(session),
            Arc::new(turn),
            "list_agents",
            function_payload(json!({
                "path_prefix": "worker"
            })),
        ))
        .await
        .expect("list_agents should succeed");
    let (content, _) = expect_text_output(output);
    let result: ListAgentsResult =
        serde_json::from_str(&content).expect("list_agents result should be json");

    assert_eq!(result.agents.len(), 1);
    assert_eq!(result.agents[0].agent_name, worker_path.as_str());
    assert_eq!(result.agents[0].last_task_message.as_deref(), Some("build"));
}

#[tokio::test]
async fn multi_agent_v2_list_agents_omits_closed_agents() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.conversation_id = root.thread_id;
    let mut config = (*turn.config).clone();
    let _ = config.features.enable(Feature::MultiAgentV2);
    turn.config = Arc::new(config);

    let session = Arc::new(session);
    let turn = Arc::new(turn);
    let spawn_output = SpawnAgentHandlerV2
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "spawn_agent",
            function_payload(json!({
                "message": "inspect this repo",
                "task_name": "worker"
            })),
        ))
        .await
        .expect("spawn_agent should succeed");
    let _ = expect_text_output(spawn_output);

    let agent_id = session
        .services
        .agent_control
        .resolve_agent_reference(session.conversation_id, &turn.session_source, "worker")
        .await
        .expect("worker path should resolve");
    session
        .services
        .agent_control
        .close_agent(agent_id)
        .await
        .expect("close_agent should succeed");

    let output = ListAgentsHandlerV2
        .handle(invocation(
            session,
            turn,
            "list_agents",
            function_payload(json!({})),
        ))
        .await
        .expect("list_agents should succeed");
    let (content, _) = expect_text_output(output);
    let result: ListAgentsResult =
        serde_json::from_str(&content).expect("list_agents result should be json");

    assert_eq!(result.agents.len(), 1);
    assert_eq!(result.agents[0].agent_name, "/root");
    assert_eq!(
        result.agents[0].last_task_message.as_deref(),
        Some("Main thread")
    );
}

#[tokio::test]
async fn multi_agent_v2_send_message_rejects_legacy_items_field() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.conversation_id = root.thread_id;
    let mut config = turn.config.as_ref().clone();
    let _ = config.features.enable(Feature::MultiAgentV2);
    turn.config = Arc::new(config);
    let session = Arc::new(session);
    let turn = Arc::new(turn);

    SpawnAgentHandlerV2
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "spawn_agent",
            function_payload(json!({
                "message": "boot worker",
                "task_name": "worker"
            })),
        ))
        .await
        .expect("spawn worker");
    let agent_id = session
        .services
        .agent_control
        .resolve_agent_reference(session.conversation_id, &turn.session_source, "worker")
        .await
        .expect("worker should resolve");
    let invocation = invocation(
        session,
        turn,
        "send_message",
        function_payload(json!({
            "target": agent_id.to_string(),
            "items": [
                {"type": "mention", "name": "drive", "path": "app://google_drive"},
                {"type": "text", "text": "read the folder"}
            ]
        })),
    );

    let Err(err) = SendMessageHandlerV2.handle(invocation).await else {
        panic!("legacy items field should be rejected in v2");
    };
    let FunctionCallError::RespondToModel(message) = err else {
        panic!("legacy items field should surface as a model-facing error");
    };
    assert!(message.contains("unknown field `items`"));
}

#[tokio::test]
async fn multi_agent_v2_send_message_rejects_interrupt_parameter() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.conversation_id = root.thread_id;
    let mut config = turn.config.as_ref().clone();
    let _ = config.features.enable(Feature::MultiAgentV2);
    turn.config = Arc::new(config);
    let session = Arc::new(session);
    let turn = Arc::new(turn);

    SpawnAgentHandlerV2
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "spawn_agent",
            function_payload(json!({
                "message": "boot worker",
                "task_name": "worker"
            })),
        ))
        .await
        .expect("spawn worker");
    let agent_id = session
        .services
        .agent_control
        .resolve_agent_reference(session.conversation_id, &turn.session_source, "worker")
        .await
        .expect("worker should resolve");

    let invocation = invocation(
        session,
        turn,
        "send_message",
        function_payload(json!({
            "target": agent_id.to_string(),
            "message": "continue",
            "interrupt": true
        })),
    );

    let Err(err) = SendMessageHandlerV2.handle(invocation).await else {
        panic!("send_message interrupt parameter should be rejected");
    };
    let FunctionCallError::RespondToModel(message) = err else {
        panic!("expected model-facing parse error");
    };
    assert!(message.starts_with(
        "failed to parse function arguments: unknown field `interrupt`, expected `target` or `message`"
    ));

    let ops = manager.captured_ops();
    let ops_for_agent: Vec<&Op> = ops
        .iter()
        .filter_map(|(id, op)| (*id == agent_id).then_some(op))
        .collect();
    assert!(!ops_for_agent.iter().any(|op| matches!(op, Op::Interrupt)));
    assert!(!ops_for_agent.iter().any(|op| matches!(
        op,
        Op::InterAgentCommunication { communication }
            if communication.author == AgentPath::root()
                && communication.recipient.as_str() == "/root/worker"
                && communication.other_recipients.is_empty()
                && communication.content == "continue"
                && !communication.trigger_turn
    )));
}

#[tokio::test]
async fn multi_agent_v2_followup_task_interrupts_busy_child_without_losing_message() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.conversation_id = root.thread_id;
    let mut config = turn.config.as_ref().clone();
    let _ = config.features.enable(Feature::MultiAgentV2);
    turn.config = Arc::new(config);
    let session = Arc::new(session);
    let turn = Arc::new(turn);

    SpawnAgentHandlerV2
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "spawn_agent",
            function_payload(json!({
                "message": "boot worker",
                "task_name": "worker"
            })),
        ))
        .await
        .expect("spawn worker");
    let agent_id = session
        .services
        .agent_control
        .resolve_agent_reference(session.conversation_id, &turn.session_source, "worker")
        .await
        .expect("worker should resolve");
    let thread = manager
        .get_thread(agent_id)
        .await
        .expect("worker thread should exist");

    let active_turn = thread.codex.session.new_default_turn().await;
    let interrupted_turn_id = active_turn.sub_id.clone();
    thread
        .codex
        .session
        .spawn_task(
            Arc::clone(&active_turn),
            vec![UserInput::Text {
                text: "working".to_string(),
                text_elements: Vec::new(),
            }],
            NeverEndingTask,
        )
        .await;

    FollowupTaskHandlerV2
        .handle(invocation(
            session,
            turn,
            "followup_task",
            function_payload(json!({
                "target": agent_id.to_string(),
                "message": "continue",
                "interrupt": true
            })),
        ))
        .await
        .expect("interrupting v2 followup_task should succeed");

    let ops = manager.captured_ops();
    let ops_for_agent: Vec<&Op> = ops
        .iter()
        .filter_map(|(id, op)| (*id == agent_id).then_some(op))
        .collect();
    assert!(ops_for_agent.iter().any(|op| matches!(op, Op::Interrupt)));
    assert!(ops_for_agent.iter().any(|op| {
        matches!(
            op,
            Op::InterAgentCommunication { communication }
                if communication.author == AgentPath::root()
                    && communication.recipient.as_str() == "/root/worker"
                    && communication.other_recipients.is_empty()
                    && communication.content == "continue"
        )
    }));

    wait_for_turn_aborted(&thread, &interrupted_turn_id, TurnAbortReason::Interrupted).await;
    wait_for_redirected_envelope_in_history(
        &thread,
        &InterAgentCommunication::new(
            AgentPath::root(),
            AgentPath::try_from("/root/worker").expect("agent path"),
            Vec::new(),
            "continue".to_string(),
            /*trigger_turn*/ true,
        ),
    )
    .await;

    let _ = thread
        .submit(Op::Shutdown {})
        .await
        .expect("shutdown should submit");
}

#[tokio::test]
async fn multi_agent_v2_followup_task_completion_notifies_parent_on_every_turn() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.conversation_id = root.thread_id;
    let mut config = turn.config.as_ref().clone();
    let _ = config.features.enable(Feature::MultiAgentV2);
    turn.config = Arc::new(config);
    let session = Arc::new(session);
    let turn = Arc::new(turn);

    SpawnAgentHandlerV2
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "spawn_agent",
            function_payload(json!({
                "message": "boot worker",
                "task_name": "worker"
            })),
        ))
        .await
        .expect("spawn worker");
    let agent_id = session
        .services
        .agent_control
        .resolve_agent_reference(session.conversation_id, &turn.session_source, "worker")
        .await
        .expect("worker should resolve");
    let thread = manager
        .get_thread(agent_id)
        .await
        .expect("worker thread should exist");
    let worker_path = AgentPath::try_from("/root/worker").expect("worker path");

    let first_turn = thread.codex.session.new_default_turn().await;
    thread
        .codex
        .session
        .send_event(
            first_turn.as_ref(),
            EventMsg::TurnComplete(TurnCompleteEvent {
                turn_id: first_turn.sub_id.clone(),
                last_agent_message: Some("first done".to_string()),
                completed_at: None,
                duration_ms: None,
            }),
        )
        .await;

    FollowupTaskHandlerV2
        .handle(invocation(
            session,
            turn,
            "followup_task",
            function_payload(json!({
                "target": agent_id.to_string(),
                "message": "continue",
            })),
        ))
        .await
        .expect("followup_task should succeed");

    let second_turn = thread.codex.session.new_default_turn().await;
    thread
        .codex
        .session
        .send_event(
            second_turn.as_ref(),
            EventMsg::TurnComplete(TurnCompleteEvent {
                turn_id: second_turn.sub_id.clone(),
                last_agent_message: Some("second done".to_string()),
                completed_at: None,
                duration_ms: None,
            }),
        )
        .await;

    let first_notification = format_subagent_notification_message(
        worker_path.as_str(),
        &AgentStatus::Completed(Some("first done".to_string())),
    );
    let second_notification = format_subagent_notification_message(
        worker_path.as_str(),
        &AgentStatus::Completed(Some("second done".to_string())),
    );

    let notifications = timeout(Duration::from_secs(5), async {
        loop {
            let notifications = manager
                .captured_ops()
                .into_iter()
                .filter_map(|(id, op)| {
                    (id == root.thread_id)
                        .then_some(op)
                        .and_then(|op| match op {
                            Op::InterAgentCommunication { communication }
                                if communication.author == worker_path
                                    && communication.recipient == AgentPath::root()
                                    && communication.other_recipients.is_empty()
                                    && !communication.trigger_turn =>
                            {
                                Some(communication.content)
                            }
                            _ => None,
                        })
                })
                .collect::<Vec<_>>();
            let first_count = notifications
                .iter()
                .filter(|message| **message == first_notification)
                .count();
            let second_count = notifications
                .iter()
                .filter(|message| **message == second_notification)
                .count();
            if first_count == 1 && second_count == 1 {
                break notifications;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("parent should receive one completion notification per child turn");

    assert_eq!(notifications.len(), 2);
}

#[tokio::test]
async fn multi_agent_v2_followup_task_rejects_legacy_items_field() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.conversation_id = root.thread_id;
    let mut config = turn.config.as_ref().clone();
    let _ = config.features.enable(Feature::MultiAgentV2);
    turn.config = Arc::new(config);
    let session = Arc::new(session);
    let turn = Arc::new(turn);

    SpawnAgentHandlerV2
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "spawn_agent",
            function_payload(json!({
                "message": "boot worker",
                "task_name": "worker"
            })),
        ))
        .await
        .expect("spawn worker");
    let agent_id = session
        .services
        .agent_control
        .resolve_agent_reference(session.conversation_id, &turn.session_source, "worker")
        .await
        .expect("worker should resolve");
    let invocation = invocation(
        session,
        turn,
        "followup_task",
        function_payload(json!({
            "target": agent_id.to_string(),
            "items": [{"type": "text", "text": "continue"}],
        })),
    );

    let Err(err) = FollowupTaskHandlerV2.handle(invocation).await else {
        panic!("legacy items field should be rejected in v2");
    };
    let FunctionCallError::RespondToModel(message) = err else {
        panic!("legacy items field should surface as a model-facing error");
    };
    assert!(message.contains("unknown field `items`"));
}

#[tokio::test]
async fn multi_agent_v2_interrupted_turn_does_not_notify_parent() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.conversation_id = root.thread_id;
    let mut config = turn.config.as_ref().clone();
    let _ = config.features.enable(Feature::MultiAgentV2);
    turn.config = Arc::new(config);
    let session = Arc::new(session);
    let turn = Arc::new(turn);

    SpawnAgentHandlerV2
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "spawn_agent",
            function_payload(json!({
                "message": "boot worker",
                "task_name": "worker"
            })),
        ))
        .await
        .expect("spawn worker");
    let agent_id = session
        .services
        .agent_control
        .resolve_agent_reference(session.conversation_id, &turn.session_source, "worker")
        .await
        .expect("worker should resolve");
    let thread = manager
        .get_thread(agent_id)
        .await
        .expect("worker thread should exist");

    let aborted_turn = thread.codex.session.new_default_turn().await;
    thread
        .codex
        .session
        .send_event(
            aborted_turn.as_ref(),
            EventMsg::TurnAborted(TurnAbortedEvent {
                turn_id: Some(aborted_turn.sub_id.clone()),
                reason: TurnAbortReason::Interrupted,
                completed_at: None,
                duration_ms: None,
            }),
        )
        .await;

    let notifications = manager
        .captured_ops()
        .into_iter()
        .filter_map(|(id, op)| {
            (id == root.thread_id)
                .then_some(op)
                .and_then(|op| match op {
                    Op::InterAgentCommunication { communication }
                        if communication.author.as_str() == "/root/worker"
                            && communication.recipient == AgentPath::root()
                            && communication.other_recipients.is_empty()
                            && !communication.trigger_turn =>
                    {
                        Some(communication.content)
                    }
                    _ => None,
                })
        })
        .collect::<Vec<_>>();

    assert_eq!(notifications, Vec::<String>::new());
}

#[tokio::test]
async fn multi_agent_v2_spawn_returns_only_task_name_and_nickname() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.conversation_id = root.thread_id;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    turn.config = Arc::new(config);

    let output = SpawnAgentHandlerV2
        .handle(invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            function_payload(json!({
                "message": "inspect this repo",
                "task_name": "test_process"
            })),
        ))
        .await
        .expect("spawn_agent should succeed");
    let (content, success) = expect_text_output(output);
    let result: serde_json::Value =
        serde_json::from_str(&content).expect("spawn_agent result should be json");

    assert_eq!(result["task_name"], "/root/test_process");
    assert!(result.get("nickname").is_some());
    assert!(result.get("agent_id").is_none());
    assert_eq!(success, Some(true));
}

#[tokio::test]
async fn multi_agent_v2_spawn_surfaces_task_name_validation_errors() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.conversation_id = root.thread_id;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    turn.config = Arc::new(config);
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "spawn_agent",
        function_payload(json!({
            "message": "inspect this repo",
            "task_name": "BadName"
        })),
    );
    let Err(err) = SpawnAgentHandlerV2.handle(invocation).await else {
        panic!("invalid agent name should be rejected");
    };
    assert_eq!(
        err,
        FunctionCallError::RespondToModel(
            "agent_name must use only lowercase letters, digits, and underscores".to_string()
        )
    );
}

#[tokio::test]
async fn spawn_agent_profile_overrides_inherited_model_and_reasoning() {
    #[derive(Debug, Deserialize)]
    struct SpawnAgentResult {
        agent_id: String,
    }

    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();

    let profile_model = "gpt-5.1";
    let profile_reasoning_effort = codex_protocol::openai_models::ReasoningEffort::Low;
    let profile_reasoning_effort_str = profile_reasoning_effort.to_string();
    let mut config = (*turn.config).clone();
    let user_config_path =
        AbsolutePathBuf::from_absolute_path(config.codex_home.join(CONFIG_TOML_FILE))
            .expect("absolute user config path");
    config.config_layer_stack = config.config_layer_stack.with_user_config(
        &user_config_path,
        toml! {
            profiles = { spawn-test = {
                model = profile_model,
                model_reasoning_effort = profile_reasoning_effort_str,
            } }
        }
        .into(),
    );
    turn.config = Arc::new(config);

    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "spawn_agent",
        function_payload(json!({
            "message": "inspect this repo",
            "profile": "spawn-test",
        })),
    );
    let output = SpawnAgentHandler
        .handle(invocation)
        .await
        .expect("spawn_agent should succeed");
    let (content, _) = expect_text_output(output);
    let result: SpawnAgentResult =
        serde_json::from_str(&content).expect("spawn_agent result should be json");
    let agent_id = parse_agent_id(&result.agent_id);
    let snapshot = manager
        .get_thread(agent_id)
        .await
        .expect("spawned agent thread should exist")
        .config_snapshot()
        .await;
    assert_eq!(snapshot.model, profile_model);
    assert_eq!(snapshot.reasoning_effort, Some(profile_reasoning_effort));
}

#[tokio::test]
async fn spawn_agent_profile_can_deny_child_file_mutation() {
    #[derive(Debug, Deserialize)]
    struct SpawnAgentResult {
        agent_id: String,
    }

    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();

    let mut config = (*turn.config).clone();
    let user_config_path =
        AbsolutePathBuf::from_absolute_path(config.codex_home.join(CONFIG_TOML_FILE))
            .expect("absolute user config path");
    config.config_layer_stack = config.config_layer_stack.with_user_config(
        &user_config_path,
        toml! {
            profiles = { recon = {
                subagent = { file_mutation = "deny" },
            } }
        }
        .into(),
    );
    turn.config = Arc::new(config);

    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "spawn_agent",
        function_payload(json!({
            "message": "inspect only",
            "profile": "recon",
        })),
    );
    let output = SpawnAgentHandler
        .handle(invocation)
        .await
        .expect("spawn_agent should succeed");
    let (content, _) = expect_text_output(output);
    let result: SpawnAgentResult =
        serde_json::from_str(&content).expect("spawn_agent result should be json");
    let agent_id = parse_agent_id(&result.agent_id);
    let child_thread = manager
        .get_thread(agent_id)
        .await
        .expect("spawned agent thread should exist");
    assert_child_file_mutation_denied(&child_thread).await;
}

#[tokio::test]
async fn spawn_agent_without_profile_preserves_denied_parent_file_mutation() {
    #[derive(Debug, Deserialize)]
    struct SpawnAgentResult {
        agent_id: String,
    }

    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();

    let mut denied_parent_config = (*turn.config).clone();
    apply_file_mutation_mode_to_config(&mut denied_parent_config, SubagentFileMutationMode::Deny)
        .expect("deny mode should apply");
    turn.sandbox_policy
        .set(
            denied_parent_config
                .permissions
                .sandbox_policy
                .get()
                .clone(),
        )
        .expect("sandbox policy set");
    turn.file_system_sandbox_policy = denied_parent_config
        .permissions
        .file_system_sandbox_policy
        .clone();
    turn.network_sandbox_policy = denied_parent_config.permissions.network_sandbox_policy;
    turn.config = Arc::new(denied_parent_config);

    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "spawn_agent",
        function_payload(json!({
            "message": "inspect only",
        })),
    );
    let output = SpawnAgentHandler
        .handle(invocation)
        .await
        .expect("spawn_agent should succeed");
    let (content, _) = expect_text_output(output);
    let result: SpawnAgentResult =
        serde_json::from_str(&content).expect("spawn_agent result should be json");
    let agent_id = parse_agent_id(&result.agent_id);
    let child_thread = manager
        .get_thread(agent_id)
        .await
        .expect("spawned agent thread should exist");
    assert_child_file_mutation_denied(&child_thread).await;
}

#[tokio::test]
async fn spawn_agent_profile_inherit_preserves_denied_parent_file_mutation() {
    #[derive(Debug, Deserialize)]
    struct SpawnAgentResult {
        agent_id: String,
    }

    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();

    let mut denied_parent_config = (*turn.config).clone();
    let user_config_path =
        AbsolutePathBuf::from_absolute_path(denied_parent_config.codex_home.join(CONFIG_TOML_FILE))
            .expect("absolute user config path");
    denied_parent_config.config_layer_stack =
        denied_parent_config.config_layer_stack.with_user_config(
            &user_config_path,
            toml! {
                profiles = { inherit-test = {
                    subagent = { file_mutation = "inherit" },
                } }
            }
            .into(),
        );
    apply_file_mutation_mode_to_config(&mut denied_parent_config, SubagentFileMutationMode::Deny)
        .expect("deny mode should apply");
    turn.sandbox_policy
        .set(
            denied_parent_config
                .permissions
                .sandbox_policy
                .get()
                .clone(),
        )
        .expect("sandbox policy set");
    turn.file_system_sandbox_policy = denied_parent_config
        .permissions
        .file_system_sandbox_policy
        .clone();
    turn.network_sandbox_policy = denied_parent_config.permissions.network_sandbox_policy;
    turn.config = Arc::new(denied_parent_config);

    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "spawn_agent",
        function_payload(json!({
            "message": "inspect only",
            "profile": "inherit-test",
        })),
    );
    let output = SpawnAgentHandler
        .handle(invocation)
        .await
        .expect("spawn_agent should succeed");
    let (content, _) = expect_text_output(output);
    let result: SpawnAgentResult =
        serde_json::from_str(&content).expect("spawn_agent result should be json");
    let agent_id = parse_agent_id(&result.agent_id);
    let child_thread = manager
        .get_thread(agent_id)
        .await
        .expect("spawned agent thread should exist");
    assert_child_file_mutation_denied(&child_thread).await;
}

#[tokio::test]
async fn spawn_agent_explorer_role_preserves_denied_parent_file_mutation_without_profile() {
    #[derive(Debug, Deserialize)]
    struct SpawnAgentResult {
        agent_id: String,
    }

    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();

    let mut denied_parent_config = (*turn.config).clone();
    apply_file_mutation_mode_to_config(&mut denied_parent_config, SubagentFileMutationMode::Deny)
        .expect("deny mode should apply");
    turn.sandbox_policy
        .set(
            denied_parent_config
                .permissions
                .sandbox_policy
                .get()
                .clone(),
        )
        .expect("sandbox policy set");
    turn.file_system_sandbox_policy = denied_parent_config
        .permissions
        .file_system_sandbox_policy
        .clone();
    turn.network_sandbox_policy = denied_parent_config.permissions.network_sandbox_policy;
    turn.config = Arc::new(denied_parent_config);

    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "spawn_agent",
        function_payload(json!({
            "message": "inspect only",
            "agent_type": "explorer",
        })),
    );
    let output = SpawnAgentHandler
        .handle(invocation)
        .await
        .expect("spawn_agent should succeed");
    let (content, _) = expect_text_output(output);
    let result: SpawnAgentResult =
        serde_json::from_str(&content).expect("spawn_agent result should be json");
    let agent_id = parse_agent_id(&result.agent_id);
    let child_thread = manager
        .get_thread(agent_id)
        .await
        .expect("spawned agent thread should exist");
    assert_child_file_mutation_denied(&child_thread).await;
}

#[tokio::test]
async fn spawn_agent_explorer_role_profile_inherit_preserves_denied_parent_file_mutation() {
    #[derive(Debug, Deserialize)]
    struct SpawnAgentResult {
        agent_id: String,
    }

    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();

    let mut denied_parent_config = (*turn.config).clone();
    let user_config_path =
        AbsolutePathBuf::from_absolute_path(denied_parent_config.codex_home.join(CONFIG_TOML_FILE))
            .expect("absolute user config path");
    denied_parent_config.config_layer_stack =
        denied_parent_config.config_layer_stack.with_user_config(
            &user_config_path,
            toml! {
                profiles = { inherit-test = {
                    subagent = { file_mutation = "inherit" },
                } }
            }
            .into(),
        );
    apply_file_mutation_mode_to_config(&mut denied_parent_config, SubagentFileMutationMode::Deny)
        .expect("deny mode should apply");
    turn.sandbox_policy
        .set(
            denied_parent_config
                .permissions
                .sandbox_policy
                .get()
                .clone(),
        )
        .expect("sandbox policy set");
    turn.file_system_sandbox_policy = denied_parent_config
        .permissions
        .file_system_sandbox_policy
        .clone();
    turn.network_sandbox_policy = denied_parent_config.permissions.network_sandbox_policy;
    turn.config = Arc::new(denied_parent_config);

    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "spawn_agent",
        function_payload(json!({
            "message": "inspect only",
            "agent_type": "explorer",
            "profile": "inherit-test",
        })),
    );
    let output = SpawnAgentHandler
        .handle(invocation)
        .await
        .expect("spawn_agent should succeed");
    let (content, _) = expect_text_output(output);
    let result: SpawnAgentResult =
        serde_json::from_str(&content).expect("spawn_agent result should be json");
    let agent_id = parse_agent_id(&result.agent_id);
    let child_thread = manager
        .get_thread(agent_id)
        .await
        .expect("spawned agent thread should exist");
    assert_child_file_mutation_denied(&child_thread).await;
}

#[tokio::test]
async fn spawn_agent_custom_role_preserves_denied_parent_file_mutation_without_profile() {
    #[derive(Debug, Deserialize)]
    struct SpawnAgentResult {
        agent_id: String,
    }

    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();

    let mut denied_parent_config = (*turn.config).clone();
    std::fs::create_dir_all(&denied_parent_config.codex_home).expect("create codex home");
    let role_path = denied_parent_config
        .codex_home
        .join("custom-deny-role.toml");
    std::fs::write(&role_path, "developer_instructions = \"role-dev\"\n")
        .expect("write role config");
    denied_parent_config.agent_roles.insert(
        "custom".to_string(),
        crate::config::AgentRoleConfig {
            description: Some("Custom role".to_string()),
            config_file: Some(role_path),
            nickname_candidates: None,
        },
    );
    apply_file_mutation_mode_to_config(&mut denied_parent_config, SubagentFileMutationMode::Deny)
        .expect("deny mode should apply");
    turn.sandbox_policy
        .set(
            denied_parent_config
                .permissions
                .sandbox_policy
                .get()
                .clone(),
        )
        .expect("sandbox policy set");
    turn.file_system_sandbox_policy = denied_parent_config
        .permissions
        .file_system_sandbox_policy
        .clone();
    turn.network_sandbox_policy = denied_parent_config.permissions.network_sandbox_policy;
    turn.config = Arc::new(denied_parent_config);

    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "spawn_agent",
        function_payload(json!({
            "message": "inspect only",
            "agent_type": "custom",
        })),
    );
    let output = SpawnAgentHandler
        .handle(invocation)
        .await
        .expect("spawn_agent should succeed");
    let (content, _) = expect_text_output(output);
    let result: SpawnAgentResult =
        serde_json::from_str(&content).expect("spawn_agent result should be json");
    let agent_id = parse_agent_id(&result.agent_id);
    let child_thread = manager
        .get_thread(agent_id)
        .await
        .expect("spawned agent thread should exist");
    assert_child_file_mutation_denied(&child_thread).await;
}

#[tokio::test]
async fn spawn_agent_custom_role_profile_inherit_preserves_denied_parent_file_mutation() {
    #[derive(Debug, Deserialize)]
    struct SpawnAgentResult {
        agent_id: String,
    }

    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();

    let mut denied_parent_config = (*turn.config).clone();
    std::fs::create_dir_all(&denied_parent_config.codex_home).expect("create codex home");
    let role_path = denied_parent_config
        .codex_home
        .join("custom-inherit-role.toml");
    std::fs::write(&role_path, "developer_instructions = \"role-dev\"\n")
        .expect("write role config");
    let role_path_str = role_path.to_string_lossy().to_string();
    let user_config_path =
        AbsolutePathBuf::from_absolute_path(denied_parent_config.codex_home.join(CONFIG_TOML_FILE))
            .expect("absolute user config path");
    denied_parent_config.config_layer_stack =
        denied_parent_config.config_layer_stack.with_user_config(
            &user_config_path,
            toml! {
                profiles = { inherit-test = {
                    subagent = { file_mutation = "inherit" },
                } }
                agents = { custom = {
                    description = "Custom role",
                    config_file = role_path_str,
                } }
            }
            .into(),
        );
    apply_file_mutation_mode_to_config(&mut denied_parent_config, SubagentFileMutationMode::Deny)
        .expect("deny mode should apply");
    turn.sandbox_policy
        .set(
            denied_parent_config
                .permissions
                .sandbox_policy
                .get()
                .clone(),
        )
        .expect("sandbox policy set");
    turn.file_system_sandbox_policy = denied_parent_config
        .permissions
        .file_system_sandbox_policy
        .clone();
    turn.network_sandbox_policy = denied_parent_config.permissions.network_sandbox_policy;
    turn.config = Arc::new(denied_parent_config);

    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "spawn_agent",
        function_payload(json!({
            "message": "inspect only",
            "agent_type": "custom",
            "profile": "inherit-test",
        })),
    );
    let output = SpawnAgentHandler
        .handle(invocation)
        .await
        .expect("spawn_agent should succeed");
    let (content, _) = expect_text_output(output);
    let result: SpawnAgentResult =
        serde_json::from_str(&content).expect("spawn_agent result should be json");
    let agent_id = parse_agent_id(&result.agent_id);
    let child_thread = manager
        .get_thread(agent_id)
        .await
        .expect("spawned agent thread should exist");
    assert_child_file_mutation_denied(&child_thread).await;
}

#[tokio::test]
async fn spawn_agent_v2_custom_role_preserves_denied_parent_file_mutation() {
    #[derive(Debug, Deserialize)]
    struct SpawnAgentResult {
        task_name: String,
        nickname: String,
    }

    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.conversation_id = root.thread_id;

    let mut denied_parent_config = (*turn.config).clone();
    std::fs::create_dir_all(&denied_parent_config.codex_home).expect("create codex home");
    let role_path = denied_parent_config.codex_home.join("custom-v2-role.toml");
    std::fs::write(&role_path, "developer_instructions = \"role-dev\"\n")
        .expect("write role config");
    denied_parent_config.agent_roles.insert(
        "custom".to_string(),
        crate::config::AgentRoleConfig {
            description: Some("Custom role".to_string()),
            config_file: Some(role_path),
            nickname_candidates: None,
        },
    );
    apply_file_mutation_mode_to_config(&mut denied_parent_config, SubagentFileMutationMode::Deny)
        .expect("deny mode should apply");
    turn.sandbox_policy
        .set(
            denied_parent_config
                .permissions
                .sandbox_policy
                .get()
                .clone(),
        )
        .expect("sandbox policy set");
    turn.file_system_sandbox_policy = denied_parent_config
        .permissions
        .file_system_sandbox_policy
        .clone();
    turn.network_sandbox_policy = denied_parent_config.permissions.network_sandbox_policy;
    denied_parent_config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    turn.config = Arc::new(denied_parent_config);

    let session = Arc::new(session);
    let turn = Arc::new(turn);
    let output = SpawnAgentHandlerV2
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "spawn_agent",
            function_payload(json!({
                "message": "inspect only",
                "task_name": "deny_task",
                "agent_type": "custom",
            })),
        ))
        .await
        .expect("spawn_agent v2 should succeed");
    let (content, _) = expect_text_output(output);
    let result: SpawnAgentResult =
        serde_json::from_str(&content).expect("spawn_agent result should be json");
    assert_eq!(result.task_name, "/root/deny_task");
    let _nickname = result.nickname;

    let agent_id = session
        .services
        .agent_control
        .resolve_agent_reference(session.conversation_id, &turn.session_source, "deny_task")
        .await
        .expect("spawned v2 agent should be discoverable by task name");
    let child_thread = manager
        .get_thread(agent_id)
        .await
        .expect("spawned agent thread should exist");
    assert_child_file_mutation_denied(&child_thread).await;
}

#[tokio::test]
async fn spawn_agent_profile_model_switch_normalizes_incompatible_inherited_reasoning_effort() {
    #[derive(Debug, Deserialize)]
    struct SpawnAgentResult {
        agent_id: String,
    }

    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();

    let inherited_reasoning_effort = codex_protocol::openai_models::ReasoningEffort::XHigh;
    let inherited_reasoning_effort_str = inherited_reasoning_effort.to_string();
    let profile_model = "gpt-5.1";
    let mut config = (*turn.config).clone();
    let user_config_path =
        AbsolutePathBuf::from_absolute_path(config.codex_home.join(CONFIG_TOML_FILE))
            .expect("absolute user config path");
    config.config_layer_stack = config.config_layer_stack.with_user_config(
        &user_config_path,
        toml! {
            model_reasoning_effort = inherited_reasoning_effort_str
            profiles = { spawn-test = {
                model = profile_model,
            } }
        }
        .into(),
    );
    turn.reasoning_effort = Some(inherited_reasoning_effort);
    turn.config = Arc::new(config);

    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "spawn_agent",
        function_payload(json!({
            "message": "inspect this repo",
            "profile": "spawn-test",
        })),
    );
    let output = SpawnAgentHandler
        .handle(invocation)
        .await
        .expect("spawn_agent should succeed");
    let (content, _) = expect_text_output(output);
    let result: SpawnAgentResult =
        serde_json::from_str(&content).expect("spawn_agent result should be json");
    let agent_id = parse_agent_id(&result.agent_id);
    let snapshot = manager
        .get_thread(agent_id)
        .await
        .expect("spawned agent thread should exist")
        .config_snapshot()
        .await;
    assert_eq!(snapshot.model, profile_model);
    assert_eq!(
        snapshot.reasoning_effort,
        Some(codex_protocol::openai_models::ReasoningEffort::Medium)
    );
}

#[tokio::test]
async fn spawn_agent_role_remains_authoritative_when_profile_is_also_set() {
    #[derive(Debug, Deserialize)]
    struct SpawnAgentResult {
        agent_id: String,
    }

    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();

    let profile_name = "spawn-test";
    let profile_model = "gpt-5.1";
    let profile_reasoning_effort = codex_protocol::openai_models::ReasoningEffort::Low;
    let profile_reasoning_effort_str = profile_reasoning_effort.to_string();
    let role_model = "gpt-5.1-codex-max";
    let role_reasoning_effort = codex_protocol::openai_models::ReasoningEffort::High;
    let mut config = (*turn.config).clone();
    std::fs::create_dir_all(&config.codex_home).expect("create codex home");
    let role_path = config.codex_home.join("custom-role.toml");
    let role_path_str = role_path.to_string_lossy().to_string();
    std::fs::write(
        &role_path,
        format!(
            "model = \"{role_model}\"\nmodel_reasoning_effort = \"{role_reasoning_effort}\"\n",
        ),
    )
    .expect("write role config");
    let user_config_path =
        AbsolutePathBuf::from_absolute_path(config.codex_home.join(CONFIG_TOML_FILE))
            .expect("absolute user config path");
    config.config_layer_stack = config.config_layer_stack.with_user_config(
        &user_config_path,
        toml! {
            profiles = { spawn-test = {
                model = profile_model,
                model_reasoning_effort = profile_reasoning_effort_str,
            } }
            agents = { custom = {
                description = "Custom role",
                config_file = role_path_str,
            } }
        }
        .into(),
    );
    turn.config = Arc::new(config);

    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "spawn_agent",
        function_payload(json!({
            "message": "inspect this repo",
            "agent_type": "custom",
            "profile": profile_name,
        })),
    );
    let output = SpawnAgentHandler
        .handle(invocation)
        .await
        .expect("spawn_agent should succeed");
    let (content, _) = expect_text_output(output);
    let result: SpawnAgentResult =
        serde_json::from_str(&content).expect("spawn_agent result should be json");
    let agent_id = parse_agent_id(&result.agent_id);
    let snapshot = manager
        .get_thread(agent_id)
        .await
        .expect("spawned agent thread should exist")
        .config_snapshot()
        .await;
    assert_eq!(snapshot.model, role_model);
    assert_eq!(snapshot.reasoning_effort, Some(role_reasoning_effort));
}

#[tokio::test]
async fn spawn_agent_role_preserves_role_developer_instructions() {
    let (session, mut turn) = make_session_and_context().await;
    let mut config = (*turn.config).clone();
    std::fs::create_dir_all(&config.codex_home).expect("create codex home");
    let role_path = config.codex_home.join("role-dev.toml");
    std::fs::write(
        &role_path,
        "developer_instructions = \"role-dev\"\nmodel_reasoning_effort = \"high\"\n",
    )
    .expect("write role config");
    config.developer_instructions = Some("lead-dev".to_string());
    config.agent_roles.insert(
        "custom".to_string(),
        crate::config::AgentRoleConfig {
            description: Some("Custom role".to_string()),
            config_file: Some(role_path),
            nickname_candidates: None,
        },
    );
    turn.config = Arc::new(config);

    let mut child_config = build_agent_spawn_config(&turn).expect("spawn config");
    apply_role_to_config(&mut child_config, Some("custom"))
        .await
        .expect("custom role should apply");
    apply_spawn_agent_runtime_overrides(&mut child_config, &turn)
        .expect("runtime overrides should apply");
    finalize_spawn_agent_prompt_config(
        &mut child_config,
        &turn,
        session.services.models_manager.as_ref(),
    )
    .await;

    assert_eq!(
        child_config.developer_instructions.as_deref(),
        Some("role-dev")
    );
}

#[tokio::test]
async fn spawn_agent_reapplies_runtime_sandbox_after_role_config() {
    fn pick_allowed_sandbox_policy(
        constraint: &crate::config::Constrained<SandboxPolicy>,
        base: SandboxPolicy,
    ) -> SandboxPolicy {
        let candidates = [
            SandboxPolicy::DangerFullAccess,
            SandboxPolicy::new_workspace_write_policy(),
            SandboxPolicy::new_read_only_policy(),
        ];
        candidates
            .into_iter()
            .find(|candidate| *candidate != base && constraint.can_set(candidate).is_ok())
            .unwrap_or(base)
    }

    #[derive(Debug, Deserialize)]
    struct SpawnAgentResult {
        agent_id: String,
        nickname: Option<String>,
    }

    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();
    let expected_sandbox = pick_allowed_sandbox_policy(
        &turn.config.permissions.sandbox_policy,
        turn.config.permissions.sandbox_policy.get().clone(),
    );
    let expected_file_system_sandbox_policy =
        FileSystemSandboxPolicy::from_legacy_sandbox_policy(&expected_sandbox, &turn.cwd);
    let expected_network_sandbox_policy = NetworkSandboxPolicy::from(&expected_sandbox);
    turn.approval_policy
        .set(AskForApproval::OnRequest)
        .expect("approval policy should be set");
    turn.sandbox_policy
        .set(expected_sandbox.clone())
        .expect("sandbox policy should be set");
    turn.file_system_sandbox_policy = expected_file_system_sandbox_policy.clone();
    turn.network_sandbox_policy = expected_network_sandbox_policy;
    assert_ne!(
        expected_sandbox,
        turn.config.permissions.sandbox_policy.get().clone(),
        "test requires a runtime sandbox override that differs from base config"
    );

    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "spawn_agent",
        function_payload(json!({
            "message": "await this command",
            "agent_type": "explorer"
        })),
    );
    let output = SpawnAgentHandler
        .handle(invocation)
        .await
        .expect("spawn_agent should succeed");
    let (content, _) = expect_text_output(output);
    let result: SpawnAgentResult =
        serde_json::from_str(&content).expect("spawn_agent result should be json");
    let agent_id = parse_agent_id(&result.agent_id);
    assert!(
        result
            .nickname
            .as_deref()
            .is_some_and(|nickname| !nickname.is_empty())
    );

    let snapshot = manager
        .get_thread(agent_id)
        .await
        .expect("spawned agent thread should exist")
        .config_snapshot()
        .await;
    assert_eq!(snapshot.sandbox_policy, expected_sandbox);
    assert_eq!(snapshot.approval_policy, AskForApproval::OnRequest);
    let child_thread = manager
        .get_thread(agent_id)
        .await
        .expect("spawned agent thread should exist");
    let child_turn = child_thread.codex.session.new_default_turn().await;
    assert_eq!(
        child_turn.file_system_sandbox_policy,
        expected_file_system_sandbox_policy
    );
    assert_eq!(
        child_turn.network_sandbox_policy,
        expected_network_sandbox_policy
    );
}

#[tokio::test]
async fn spawn_agent_rejects_when_depth_limit_exceeded() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();

    let max_depth = turn.config.agent_max_depth;
    turn.session_source = SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
        parent_thread_id: session.conversation_id,
        depth: max_depth,
        agent_path: None,
        agent_nickname: None,
        agent_role: None,
    });

    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "spawn_agent",
        function_payload(json!({"message": "hello"})),
    );
    let Err(err) = SpawnAgentHandler.handle(invocation).await else {
        panic!("spawn should fail when depth limit exceeded");
    };
    assert_eq!(
        err,
        FunctionCallError::RespondToModel(
            "Agent depth limit reached. Solve the task yourself.".to_string()
        )
    );
}

#[tokio::test]
async fn spawn_agent_allows_depth_up_to_configured_max_depth() {
    #[derive(Debug, Deserialize)]
    struct SpawnAgentResult {
        agent_id: String,
        nickname: Option<String>,
    }

    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();

    let mut config = (*turn.config).clone();
    config.agent_max_depth = DEFAULT_AGENT_MAX_DEPTH + 1;
    turn.config = Arc::new(config);
    turn.session_source = SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
        parent_thread_id: session.conversation_id,
        depth: DEFAULT_AGENT_MAX_DEPTH,
        agent_path: None,
        agent_nickname: None,
        agent_role: None,
    });

    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "spawn_agent",
        function_payload(json!({"message": "hello"})),
    );
    let output = SpawnAgentHandler
        .handle(invocation)
        .await
        .expect("spawn should succeed within configured depth");
    let (content, success) = expect_text_output(output);
    let result: SpawnAgentResult =
        serde_json::from_str(&content).expect("spawn_agent result should be json");
    assert!(!result.agent_id.is_empty());
    assert!(
        result
            .nickname
            .as_deref()
            .is_some_and(|nickname| !nickname.is_empty())
    );
    assert_eq!(success, Some(true));
}

#[tokio::test]
async fn send_input_rejects_empty_message() {
    let (session, turn) = make_session_and_context().await;
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "send_input",
        function_payload(json!({"target": ThreadId::new().to_string(), "message": ""})),
    );
    let Err(err) = SendInputHandler.handle(invocation).await else {
        panic!("empty message should be rejected");
    };
    assert_eq!(
        err,
        FunctionCallError::RespondToModel("Empty message can't be sent to an agent".to_string())
    );
}

#[tokio::test]
async fn send_input_rejects_when_message_and_items_are_both_set() {
    let (session, turn) = make_session_and_context().await;
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "send_input",
        function_payload(json!({
            "target": ThreadId::new().to_string(),
            "message": "hello",
            "items": [{"type": "mention", "name": "drive", "path": "app://drive"}]
        })),
    );
    let Err(err) = SendInputHandler.handle(invocation).await else {
        panic!("message+items should be rejected");
    };
    assert_eq!(
        err,
        FunctionCallError::RespondToModel(
            "Provide either message or items, but not both".to_string()
        )
    );
}

#[tokio::test]
async fn send_input_rejects_invalid_id() {
    let (session, turn) = make_session_and_context().await;
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "send_input",
        function_payload(json!({"target": "not-a-uuid", "message": "hi"})),
    );
    let Err(err) = SendInputHandler.handle(invocation).await else {
        panic!("invalid id should be rejected");
    };
    let FunctionCallError::RespondToModel(msg) = err else {
        panic!("expected respond-to-model error");
    };
    assert!(msg.starts_with("invalid agent id not-a-uuid:"));
}

#[tokio::test]
async fn send_input_reports_missing_agent() {
    let (mut session, turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();
    let agent_id = ThreadId::new();
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "send_input",
        function_payload(json!({"target": agent_id.to_string(), "message": "hi"})),
    );
    let Err(err) = SendInputHandler.handle(invocation).await else {
        panic!("missing agent should be reported");
    };
    assert_eq!(
        err,
        FunctionCallError::RespondToModel(format!("agent with id {agent_id} not found"))
    );
}

#[tokio::test]
async fn send_input_interrupts_before_prompt() {
    let (mut session, turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();
    let config = turn.config.as_ref().clone();
    let thread = manager.start_thread(config).await.expect("start thread");
    let agent_id = thread.thread_id;
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "send_input",
        function_payload(json!({
            "target": agent_id.to_string(),
            "message": "hi",
            "interrupt": true
        })),
    );
    SendInputHandler
        .handle(invocation)
        .await
        .expect("send_input should succeed");

    let ops = manager.captured_ops();
    let ops_for_agent: Vec<&Op> = ops
        .iter()
        .filter_map(|(id, op)| (*id == agent_id).then_some(op))
        .collect();
    assert_eq!(ops_for_agent.len(), 2);
    assert!(matches!(ops_for_agent[0], Op::Interrupt));
    assert!(matches!(ops_for_agent[1], Op::UserInput { .. }));

    let _ = thread
        .thread
        .submit(Op::Shutdown {})
        .await
        .expect("shutdown should submit");
}

#[tokio::test]
async fn send_input_accepts_structured_items() {
    let (mut session, turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();
    let config = turn.config.as_ref().clone();
    let thread = manager.start_thread(config).await.expect("start thread");
    let agent_id = thread.thread_id;
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "send_input",
        function_payload(json!({
            "target": agent_id.to_string(),
            "items": [
                {"type": "mention", "name": "drive", "path": "app://google_drive"},
                {"type": "text", "text": "read the folder"}
            ]
        })),
    );
    SendInputHandler
        .handle(invocation)
        .await
        .expect("send_input should succeed");

    let expected = Op::UserInput {
        items: vec![
            UserInput::Mention {
                name: "drive".to_string(),
                path: "app://google_drive".to_string(),
            },
            UserInput::Text {
                text: "read the folder".to_string(),
                text_elements: Vec::new(),
            },
        ],
        final_output_json_schema: None,
    };
    let captured = manager
        .captured_ops()
        .into_iter()
        .find(|(id, op)| *id == agent_id && *op == expected);
    assert_eq!(captured, Some((agent_id, expected)));

    let _ = thread
        .thread
        .submit(Op::Shutdown {})
        .await
        .expect("shutdown should submit");
}

#[tokio::test]
async fn resume_agent_rejects_invalid_id() {
    let (session, turn) = make_session_and_context().await;
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "resume_agent",
        function_payload(json!({"id": "not-a-uuid"})),
    );
    let Err(err) = ResumeAgentHandler.handle(invocation).await else {
        panic!("invalid id should be rejected");
    };
    let FunctionCallError::RespondToModel(msg) = err else {
        panic!("expected respond-to-model error");
    };
    assert!(msg.starts_with("invalid agent id not-a-uuid:"));
}

#[tokio::test]
async fn resume_agent_reports_missing_agent() {
    let (mut session, turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();
    let agent_id = ThreadId::new();
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "resume_agent",
        function_payload(json!({"id": agent_id.to_string()})),
    );
    let Err(err) = ResumeAgentHandler.handle(invocation).await else {
        panic!("missing agent should be reported");
    };
    assert_eq!(
        err,
        FunctionCallError::RespondToModel(format!("agent with id {agent_id} not found"))
    );
}

#[tokio::test]
async fn resume_agent_noops_for_active_agent() {
    let (mut session, turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();
    let config = turn.config.as_ref().clone();
    let thread = manager.start_thread(config).await.expect("start thread");
    let agent_id = thread.thread_id;
    let status_before = manager.agent_control().get_status(agent_id).await;
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "resume_agent",
        function_payload(json!({"id": agent_id.to_string()})),
    );

    let output = ResumeAgentHandler
        .handle(invocation)
        .await
        .expect("resume_agent should succeed");
    let (content, success) = expect_text_output(output);
    let result: resume_agent::ResumeAgentResult =
        serde_json::from_str(&content).expect("resume_agent result should be json");
    assert_eq!(result.status, status_before);
    assert_eq!(success, Some(true));

    let thread_ids = manager.list_thread_ids().await;
    assert_eq!(thread_ids, vec![agent_id]);

    let _ = thread
        .thread
        .submit(Op::Shutdown {})
        .await
        .expect("shutdown should submit");
}

#[tokio::test]
async fn resume_agent_restores_closed_agent_and_accepts_send_input() {
    let (mut session, turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();
    let config = turn.config.as_ref().clone();
    let thread = manager
        .resume_thread_with_history(
            config,
            InitialHistory::Forked(vec![RolloutItem::ResponseItem(ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "materialized".to_string(),
                }],
                end_turn: None,
                phase: None,
            })]),
            AuthManager::from_auth_for_testing(CodexAuth::from_api_key("dummy")),
            /*persist_extended_history*/ false,
            /*parent_trace*/ None,
        )
        .await
        .expect("start thread");
    let agent_id = thread.thread_id;
    let _ = manager
        .agent_control()
        .shutdown_live_agent(agent_id)
        .await
        .expect("shutdown agent");
    assert_eq!(
        manager.agent_control().get_status(agent_id).await,
        AgentStatus::NotFound
    );
    let session = Arc::new(session);
    let turn = Arc::new(turn);

    let resume_invocation = invocation(
        session.clone(),
        turn.clone(),
        "resume_agent",
        function_payload(json!({"id": agent_id.to_string()})),
    );
    let output = ResumeAgentHandler
        .handle(resume_invocation)
        .await
        .expect("resume_agent should succeed");
    let (content, success) = expect_text_output(output);
    let result: resume_agent::ResumeAgentResult =
        serde_json::from_str(&content).expect("resume_agent result should be json");
    assert_ne!(result.status, AgentStatus::NotFound);
    assert_eq!(success, Some(true));

    let send_invocation = invocation(
        session,
        turn,
        "send_input",
        function_payload(json!({"target": agent_id.to_string(), "message": "hello"})),
    );
    let output = SendInputHandler
        .handle(send_invocation)
        .await
        .expect("send_input should succeed after resume");
    let (content, success) = expect_text_output(output);
    let result: serde_json::Value =
        serde_json::from_str(&content).expect("send_input result should be json");
    let submission_id = result
        .get("submission_id")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    assert!(!submission_id.is_empty());
    assert_eq!(success, Some(true));

    let _ = manager
        .agent_control()
        .shutdown_live_agent(agent_id)
        .await
        .expect("shutdown resumed agent");
}

#[tokio::test]
async fn resume_agent_rejects_when_depth_limit_exceeded() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();

    let max_depth = turn.config.agent_max_depth;
    turn.session_source = SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
        parent_thread_id: session.conversation_id,
        depth: max_depth,
        agent_path: None,
        agent_nickname: None,
        agent_role: None,
    });

    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "resume_agent",
        function_payload(json!({"id": ThreadId::new().to_string()})),
    );
    let Err(err) = ResumeAgentHandler.handle(invocation).await else {
        panic!("resume should fail when depth limit exceeded");
    };
    assert_eq!(
        err,
        FunctionCallError::RespondToModel(
            "Agent depth limit reached. Solve the task yourself.".to_string()
        )
    );
}

#[tokio::test]
async fn wait_rejects_non_positive_timeout() {
    let (session, turn) = make_session_and_context().await;
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "wait_agent",
        function_payload(json!({
            "ids": [ThreadId::new().to_string()],
            "timeout_ms": 0
        })),
    );
    let Err(err) = WaitAgentHandler.handle(invocation).await else {
        panic!("non-positive timeout should be rejected");
    };
    assert_eq!(
        err,
        FunctionCallError::RespondToModel("timeout_ms must be greater than zero".to_string())
    );
}

#[tokio::test]
async fn wait_rejects_unknown_agent_reference() {
    let (session, turn) = make_session_and_context().await;
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "wait_agent",
        function_payload(json!({"ids": ["invalid"]})),
    );
    let Err(err) = WaitAgentHandler.handle(invocation).await else {
        panic!("invalid id should be rejected");
    };
    let FunctionCallError::RespondToModel(msg) = err else {
        panic!("expected respond-to-model error");
    };
    assert!(msg.starts_with("invalid agent id invalid:"));
}

#[tokio::test]
async fn wait_rejects_empty_ids() {
    let (session, turn) = make_session_and_context().await;
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "wait_agent",
        function_payload(json!({"ids": []})),
    );
    let Err(err) = WaitAgentHandler.handle(invocation).await else {
        panic!("empty ids should be rejected");
    };
    assert_eq!(
        err,
        FunctionCallError::RespondToModel("ids must be non-empty".to_string())
    );
}

#[tokio::test]
async fn wait_rejects_return_when_without_disabled_timeout() {
    let (session, turn) = make_session_and_context().await;
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "wait_agent",
        function_payload(json!({
            "ids": [ThreadId::new().to_string()],
            "return_when": "any_final",
            "timeout_ms": 1000
        })),
    );
    let Err(err) = WaitAgentHandler.handle(invocation).await else {
        panic!("return_when without disable_timeout should be rejected");
    };
    assert_eq!(
        err,
        FunctionCallError::RespondToModel("return_when requires disable_timeout=true".to_string())
    );
}

#[tokio::test]
async fn multi_agent_v2_wait_agent_accepts_timeout_only_argument() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.conversation_id = root.thread_id;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    turn.config = Arc::new(config);
    let session = Arc::new(session);
    let turn = Arc::new(turn);

    SpawnAgentHandlerV2
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "spawn_agent",
            function_payload(json!({
                "message": "boot worker",
                "task_name": "worker"
            })),
        ))
        .await
        .expect("spawn worker");
    let agent_id = session
        .services
        .agent_control
        .resolve_agent_reference(session.conversation_id, &turn.session_source, "worker")
        .await
        .expect("worker should resolve");
    let worker_path = session
        .services
        .agent_control
        .get_agent_metadata(agent_id)
        .expect("worker metadata")
        .agent_path
        .expect("worker path");

    let wait_task = tokio::spawn({
        let session = session.clone();
        let turn = turn.clone();
        async move {
            WaitAgentHandlerV2
                .handle(invocation(
                    session,
                    turn,
                    "wait_agent",
                    function_payload(json!({"timeout_ms": 1000})),
                ))
                .await
        }
    });
    tokio::task::yield_now().await;

    session.enqueue_mailbox_communication(InterAgentCommunication::new(
        worker_path,
        AgentPath::root(),
        Vec::new(),
        "hello from worker".to_string(),
        /*trigger_turn*/ false,
    ));

    let output = wait_task
        .await
        .expect("wait task should join")
        .expect("timeout-only args should be accepted in v2 mode");
    let (content, success) = expect_text_output(output);
    let result: crate::tools::handlers::multi_agents_v2::wait::WaitAgentResult =
        serde_json::from_str(&content).expect("wait_agent result should be json");
    assert_eq!(
        result,
        crate::tools::handlers::multi_agents_v2::wait::WaitAgentResult {
            message: "Wait completed.".to_string(),
            timed_out: false,
        }
    );
    assert_eq!(success, None);
}

#[tokio::test]
async fn wait_rejects_disable_timeout_without_return_when() {
    let (session, turn) = make_session_and_context().await;
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "wait_agent",
        function_payload(json!({
            "ids": [ThreadId::new().to_string()],
            "disable_timeout": true
        })),
    );
    let Err(err) = WaitAgentHandler.handle(invocation).await else {
        panic!("disable_timeout without return_when should be rejected");
    };
    assert_eq!(
        err,
        FunctionCallError::RespondToModel("disable_timeout requires return_when".to_string())
    );
}

#[tokio::test]
async fn wait_returns_not_found_for_missing_agents() {
    let (mut session, turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();
    let id_a = ThreadId::new();
    let id_b = ThreadId::new();
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "wait_agent",
        function_payload(json!({
            "ids": [id_a.to_string(), id_b.to_string()],
            "timeout_ms": 1000
        })),
    );
    let output = WaitAgentHandler
        .handle(invocation)
        .await
        .expect("wait should succeed");
    let (content, success) = expect_text_output(output);
    let result: wait::WaitResult =
        serde_json::from_str(&content).expect("wait result should be json");
    assert!(!result.timed_out);
    assert_eq!(result.agents.len(), 2);
    assert_eq!(
        assert_agent_status(&result.agents, id_a, AgentStatus::NotFound).last_activity,
        None
    );
    assert_eq!(
        assert_agent_status(&result.agents, id_b, AgentStatus::NotFound).last_activity,
        None
    );
    assert_eq!(success, None);
}

#[tokio::test]
async fn wait_times_out_when_status_is_not_final() {
    let (mut session, turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();
    let config = turn.config.as_ref().clone();
    let thread = manager.start_thread(config).await.expect("start thread");
    let agent_id = thread.thread_id;
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "wait_agent",
        function_payload(json!({
            "ids": [agent_id.to_string()],
            "timeout_ms": MIN_WAIT_TIMEOUT_MS
        })),
    );
    let output = WaitAgentHandler
        .handle(invocation)
        .await
        .expect("wait should succeed");
    let (content, success) = expect_text_output(output);
    let result: wait::WaitResult =
        serde_json::from_str(&content).expect("wait result should be json");
    assert!(result.timed_out);
    assert_eq!(result.agents.len(), 1);
    assert_eq!(
        assert_agent_status(&result.agents, agent_id, AgentStatus::PendingInit).last_activity,
        None
    );
    assert_eq!(success, None);

    let _ = thread
        .thread
        .submit(Op::Shutdown {})
        .await
        .expect("shutdown should submit");
}

#[tokio::test]
async fn wait_clamps_short_timeouts_to_minimum() {
    let (mut session, turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();
    let config = turn.config.as_ref().clone();
    let thread = manager.start_thread(config).await.expect("start thread");
    let agent_id = thread.thread_id;
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "wait_agent",
        function_payload(json!({
            "ids": [agent_id.to_string()],
            "timeout_ms": 10
        })),
    );

    let early = timeout(
        Duration::from_millis(50),
        WaitAgentHandler.handle(invocation),
    )
    .await;
    assert!(
        early.is_err(),
        "wait should not return before the minimum timeout clamp"
    );

    let _ = thread
        .thread
        .submit(Op::Shutdown {})
        .await
        .expect("shutdown should submit");
}

#[tokio::test]
async fn wait_without_explicit_condition_returns_final_status_before_timeout() {
    let (mut session, turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();
    let config = turn.config.as_ref().clone();
    let thread = manager.start_thread(config).await.expect("start thread");
    let agent_id = thread.thread_id;
    let mut status_rx = manager
        .agent_control()
        .subscribe_status(agent_id)
        .await
        .expect("subscribe should succeed");

    let _ = thread
        .thread
        .submit(Op::Shutdown {})
        .await
        .expect("shutdown should submit");
    let _ = timeout(Duration::from_secs(1), status_rx.changed())
        .await
        .expect("shutdown status should arrive");

    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "wait_agent",
        function_payload(json!({
            "ids": [agent_id.to_string()],
            "timeout_ms": 1000
        })),
    );
    let output = WaitAgentHandler
        .handle(invocation)
        .await
        .expect("wait should succeed");
    let (content, success) = expect_text_output(output);
    let result: wait::WaitResult =
        serde_json::from_str(&content).expect("wait result should be json");
    assert!(!result.timed_out);
    assert_eq!(result.agents.len(), 1);
    assert_shutdown_activity(assert_agent_status(
        &result.agents,
        agent_id,
        AgentStatus::Shutdown,
    ));
    assert_eq!(success, None);
}

#[tokio::test]
async fn wait_without_explicit_condition_returns_immediately_when_any_agent_is_already_final() {
    let (mut session, turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();
    let config = turn.config.as_ref().clone();
    let final_thread = manager
        .start_thread(config.clone())
        .await
        .expect("start final thread");
    let running_thread = manager
        .start_thread(config)
        .await
        .expect("start running thread");
    let final_agent_id = final_thread.thread_id;
    let running_agent_id = running_thread.thread_id;
    let mut final_status_rx = manager
        .agent_control()
        .subscribe_status(final_agent_id)
        .await
        .expect("subscribe should succeed");

    let _ = final_thread
        .thread
        .submit(Op::Shutdown {})
        .await
        .expect("shutdown should submit");
    wait_for_shutdown_status(&mut final_status_rx).await;

    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "wait_agent",
        function_payload(json!({
            "ids": [final_agent_id.to_string(), running_agent_id.to_string()],
            "timeout_ms": MIN_WAIT_TIMEOUT_MS
        })),
    );
    let output = WaitAgentHandler
        .handle(invocation)
        .await
        .expect("wait should succeed");
    let (content, success) = expect_text_output(output);
    let result: wait::WaitResult =
        serde_json::from_str(&content).expect("wait result should be json");
    assert!(!result.timed_out);
    assert_eq!(result.agents.len(), 2);
    assert_shutdown_activity(assert_agent_status(
        &result.agents,
        final_agent_id,
        AgentStatus::Shutdown,
    ));
    let running_state = result
        .agents
        .get(&running_agent_id)
        .expect("running agent should be reported");
    assert!(matches!(
        running_state.status,
        AgentStatus::PendingInit | AgentStatus::Running
    ));
    assert_eq!(success, None);

    let _ = running_thread
        .thread
        .submit(Op::Shutdown {})
        .await
        .expect("shutdown should submit");
}

#[tokio::test]
async fn wait_without_explicit_condition_returns_immediately_when_any_agent_is_not_found() {
    let (mut session, turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();
    let config = turn.config.as_ref().clone();
    let running_thread = manager
        .start_thread(config)
        .await
        .expect("start running thread");
    let running_agent_id = running_thread.thread_id;
    let missing_agent_id = ThreadId::new();

    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "wait_agent",
        function_payload(json!({
            "ids": [missing_agent_id.to_string(), running_agent_id.to_string()],
            "timeout_ms": MIN_WAIT_TIMEOUT_MS
        })),
    );
    let output = WaitAgentHandler
        .handle(invocation)
        .await
        .expect("wait should succeed");
    let (content, success) = expect_text_output(output);
    let result: wait::WaitResult =
        serde_json::from_str(&content).expect("wait result should be json");
    assert!(!result.timed_out);
    assert_eq!(result.agents.len(), 2);
    assert_eq!(
        assert_agent_status(&result.agents, missing_agent_id, AgentStatus::NotFound).last_activity,
        None
    );
    let running_state = result
        .agents
        .get(&running_agent_id)
        .expect("running agent should be reported");
    assert!(matches!(
        running_state.status,
        AgentStatus::PendingInit | AgentStatus::Running
    ));
    assert_eq!(success, None);

    let _ = running_thread
        .thread
        .submit(Op::Shutdown {})
        .await
        .expect("shutdown should submit");
}

#[tokio::test]
async fn wait_any_final_ignores_agents_already_final_at_start() {
    let (mut session, turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();
    let config = turn.config.as_ref().clone();
    let final_thread = manager
        .start_thread(config.clone())
        .await
        .expect("start final thread");
    let running_thread = manager
        .start_thread(config)
        .await
        .expect("start running thread");
    let final_agent_id = final_thread.thread_id;
    let running_agent_id = running_thread.thread_id;
    let mut final_status_rx = manager
        .agent_control()
        .subscribe_status(final_agent_id)
        .await
        .expect("subscribe should succeed");

    let _ = final_thread
        .thread
        .submit(Op::Shutdown {})
        .await
        .expect("shutdown should submit");
    wait_for_shutdown_status(&mut final_status_rx).await;

    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "wait_agent",
        function_payload(json!({
            "ids": [final_agent_id.to_string(), running_agent_id.to_string()],
            "return_when": "any_final",
            "disable_timeout": true
        })),
    );
    let mut wait_task = tokio::spawn(async move { WaitAgentHandler.handle(invocation).await });
    let early = timeout(Duration::from_millis(50), &mut wait_task).await;
    assert!(
        early.is_err(),
        "any_final should wait for a non-final agent to newly finish"
    );

    let _ = running_thread
        .thread
        .submit(Op::Shutdown {})
        .await
        .expect("shutdown should submit");

    let output = timeout(Duration::from_secs(1), &mut wait_task)
        .await
        .expect("wait task should finish once the running agent finishes")
        .expect("wait task should join")
        .expect("wait should succeed");
    assert!(!output.timed_out);
    assert_eq!(output.agents.len(), 2);
    assert_shutdown_activity(assert_agent_status(
        &output.agents,
        final_agent_id,
        AgentStatus::Shutdown,
    ));
    assert_shutdown_activity(assert_agent_status(
        &output.agents,
        running_agent_id,
        AgentStatus::Shutdown,
    ));
}

#[tokio::test]
async fn wait_any_final_ignores_not_found_agents_at_start() {
    let (mut session, turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();
    let config = turn.config.as_ref().clone();
    let running_thread = manager
        .start_thread(config)
        .await
        .expect("start running thread");
    let running_agent_id = running_thread.thread_id;
    let missing_agent_id = ThreadId::new();

    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "wait_agent",
        function_payload(json!({
            "ids": [missing_agent_id.to_string(), running_agent_id.to_string()],
            "return_when": "any_final",
            "disable_timeout": true
        })),
    );
    let mut wait_task = tokio::spawn(async move { WaitAgentHandler.handle(invocation).await });
    let early = timeout(Duration::from_millis(50), &mut wait_task).await;
    assert!(
        early.is_err(),
        "any_final should not complete immediately on a not-found agent when another agent is still non-final"
    );

    let _ = running_thread
        .thread
        .submit(Op::Shutdown {})
        .await
        .expect("shutdown should submit");

    let output = timeout(Duration::from_secs(1), &mut wait_task)
        .await
        .expect("wait task should finish once the running agent finishes")
        .expect("wait task should join")
        .expect("wait should succeed");
    assert!(!output.timed_out);
    assert_eq!(output.agents.len(), 2);
    assert_eq!(
        assert_agent_status(&output.agents, missing_agent_id, AgentStatus::NotFound).last_activity,
        None
    );
    assert_shutdown_activity(assert_agent_status(
        &output.agents,
        running_agent_id,
        AgentStatus::Shutdown,
    ));
}

#[tokio::test]
async fn wait_all_final_waits_for_remaining_non_final_agents() {
    let (mut session, turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();
    let config = turn.config.as_ref().clone();
    let final_thread = manager
        .start_thread(config.clone())
        .await
        .expect("start final thread");
    let running_thread = manager
        .start_thread(config)
        .await
        .expect("start running thread");
    let final_agent_id = final_thread.thread_id;
    let running_agent_id = running_thread.thread_id;
    let mut final_status_rx = manager
        .agent_control()
        .subscribe_status(final_agent_id)
        .await
        .expect("subscribe should succeed");

    let _ = final_thread
        .thread
        .submit(Op::Shutdown {})
        .await
        .expect("shutdown should submit");
    wait_for_shutdown_status(&mut final_status_rx).await;

    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "wait_agent",
        function_payload(json!({
            "ids": [final_agent_id.to_string(), running_agent_id.to_string()],
            "return_when": "all_final",
            "disable_timeout": true
        })),
    );
    let mut wait_task = tokio::spawn(async move { WaitAgentHandler.handle(invocation).await });
    let early = timeout(Duration::from_millis(50), &mut wait_task).await;
    assert!(
        early.is_err(),
        "all_final should keep waiting while any requested agent remains non-final"
    );

    let _ = running_thread
        .thread
        .submit(Op::Shutdown {})
        .await
        .expect("shutdown should submit");

    let output = timeout(Duration::from_secs(1), &mut wait_task)
        .await
        .expect("wait task should finish once all requested agents finish")
        .expect("wait task should join")
        .expect("wait should succeed");
    assert!(!output.timed_out);
    assert_eq!(output.agents.len(), 2);
    assert_shutdown_activity(assert_agent_status(
        &output.agents,
        final_agent_id,
        AgentStatus::Shutdown,
    ));
    assert_shutdown_activity(assert_agent_status(
        &output.agents,
        running_agent_id,
        AgentStatus::Shutdown,
    ));
}

// Merge-safety anchor: legacy wait_agent explicit waits must short-circuit when a requested
// agent is already or becomes errored so the lead cannot hang forever on unreachable any/all
// final conditions.
#[tokio::test]
async fn wait_any_final_returns_immediately_when_agent_is_already_errored() {
    let (mut session, turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();
    let config = turn.config.as_ref().clone();
    let errored_thread = manager
        .start_thread(config.clone())
        .await
        .expect("start errored thread");
    let running_thread = manager
        .start_thread(config)
        .await
        .expect("start running thread");
    let errored_agent_id = errored_thread.thread_id;
    let running_agent_id = running_thread.thread_id;
    let mut errored_status_rx = manager
        .agent_control()
        .subscribe_status(errored_agent_id)
        .await
        .expect("subscribe should succeed");

    mark_thread_errored(&errored_thread.thread, "usage limit").await;
    wait_for_errored_status(&mut errored_status_rx, "usage limit").await;

    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "wait_agent",
        function_payload(json!({
            "ids": [errored_agent_id.to_string(), running_agent_id.to_string()],
            "return_when": "any_final",
            "disable_timeout": true
        })),
    );
    let output = timeout(Duration::from_secs(1), WaitAgentHandler.handle(invocation))
        .await
        .expect("wait should finish immediately")
        .expect("wait should succeed");
    assert!(!output.timed_out);
    assert_eq!(output.agents.len(), 2);
    assert_agent_status(
        &output.agents,
        errored_agent_id,
        AgentStatus::Errored("usage limit".to_string()),
    );
    let running_state = output
        .agents
        .get(&running_agent_id)
        .expect("running agent should be reported");
    assert!(matches!(
        running_state.status,
        AgentStatus::PendingInit | AgentStatus::Running
    ));

    let _ = errored_thread
        .thread
        .submit(Op::Shutdown {})
        .await
        .expect("shutdown should submit");
    let _ = running_thread
        .thread
        .submit(Op::Shutdown {})
        .await
        .expect("shutdown should submit");
}

#[tokio::test]
async fn wait_all_final_returns_immediately_when_agent_is_already_errored() {
    let (mut session, turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();
    let config = turn.config.as_ref().clone();
    let errored_thread = manager
        .start_thread(config.clone())
        .await
        .expect("start errored thread");
    let running_thread = manager
        .start_thread(config)
        .await
        .expect("start running thread");
    let errored_agent_id = errored_thread.thread_id;
    let running_agent_id = running_thread.thread_id;
    let mut errored_status_rx = manager
        .agent_control()
        .subscribe_status(errored_agent_id)
        .await
        .expect("subscribe should succeed");

    mark_thread_errored(&errored_thread.thread, "usage limit").await;
    wait_for_errored_status(&mut errored_status_rx, "usage limit").await;

    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "wait_agent",
        function_payload(json!({
            "ids": [errored_agent_id.to_string(), running_agent_id.to_string()],
            "return_when": "all_final",
            "disable_timeout": true
        })),
    );
    let output = timeout(Duration::from_secs(1), WaitAgentHandler.handle(invocation))
        .await
        .expect("wait should finish immediately")
        .expect("wait should succeed");
    assert!(!output.timed_out);
    assert_eq!(output.agents.len(), 2);
    assert_agent_status(
        &output.agents,
        errored_agent_id,
        AgentStatus::Errored("usage limit".to_string()),
    );
    let running_state = output
        .agents
        .get(&running_agent_id)
        .expect("running agent should be reported");
    assert!(matches!(
        running_state.status,
        AgentStatus::PendingInit | AgentStatus::Running
    ));

    let _ = errored_thread
        .thread
        .submit(Op::Shutdown {})
        .await
        .expect("shutdown should submit");
    let _ = running_thread
        .thread
        .submit(Op::Shutdown {})
        .await
        .expect("shutdown should submit");
}

#[tokio::test]
async fn wait_any_final_returns_when_agent_errors_during_wait() {
    let (mut session, turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();
    let config = turn.config.as_ref().clone();
    let errored_thread = manager
        .start_thread(config.clone())
        .await
        .expect("start errored thread");
    let running_thread = manager
        .start_thread(config)
        .await
        .expect("start running thread");
    let errored_agent_id = errored_thread.thread_id;
    let running_agent_id = running_thread.thread_id;
    let mut errored_status_rx = manager
        .agent_control()
        .subscribe_status(errored_agent_id)
        .await
        .expect("subscribe should succeed");

    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "wait_agent",
        function_payload(json!({
            "ids": [errored_agent_id.to_string(), running_agent_id.to_string()],
            "return_when": "any_final",
            "disable_timeout": true
        })),
    );
    let mut wait_task = tokio::spawn(async move { WaitAgentHandler.handle(invocation).await });
    let early = timeout(Duration::from_millis(50), &mut wait_task).await;
    assert!(
        early.is_err(),
        "any_final should still be waiting before an agent errors"
    );

    mark_thread_errored(&errored_thread.thread, "usage limit").await;
    wait_for_errored_status(&mut errored_status_rx, "usage limit").await;

    let output = timeout(Duration::from_secs(1), &mut wait_task)
        .await
        .expect("wait task should finish once the agent errors")
        .expect("wait task should join")
        .expect("wait should succeed");
    assert!(!output.timed_out);
    assert_eq!(output.agents.len(), 2);
    assert_agent_status(
        &output.agents,
        errored_agent_id,
        AgentStatus::Errored("usage limit".to_string()),
    );
    let running_state = output
        .agents
        .get(&running_agent_id)
        .expect("running agent should be reported");
    assert!(matches!(
        running_state.status,
        AgentStatus::PendingInit | AgentStatus::Running
    ));

    let _ = errored_thread
        .thread
        .submit(Op::Shutdown {})
        .await
        .expect("shutdown should submit");
    let _ = running_thread
        .thread
        .submit(Op::Shutdown {})
        .await
        .expect("shutdown should submit");
}

#[tokio::test]
async fn wait_all_final_returns_when_agent_errors_during_wait() {
    let (mut session, turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();
    let config = turn.config.as_ref().clone();
    let errored_thread = manager
        .start_thread(config.clone())
        .await
        .expect("start errored thread");
    let running_thread = manager
        .start_thread(config)
        .await
        .expect("start running thread");
    let errored_agent_id = errored_thread.thread_id;
    let running_agent_id = running_thread.thread_id;
    let mut errored_status_rx = manager
        .agent_control()
        .subscribe_status(errored_agent_id)
        .await
        .expect("subscribe should succeed");

    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "wait_agent",
        function_payload(json!({
            "ids": [errored_agent_id.to_string(), running_agent_id.to_string()],
            "return_when": "all_final",
            "disable_timeout": true
        })),
    );
    let mut wait_task = tokio::spawn(async move { WaitAgentHandler.handle(invocation).await });
    let early = timeout(Duration::from_millis(50), &mut wait_task).await;
    assert!(
        early.is_err(),
        "all_final should still be waiting before an agent errors"
    );

    mark_thread_errored(&errored_thread.thread, "usage limit").await;
    wait_for_errored_status(&mut errored_status_rx, "usage limit").await;

    let output = timeout(Duration::from_secs(1), &mut wait_task)
        .await
        .expect("wait task should finish once the agent errors")
        .expect("wait task should join")
        .expect("wait should succeed");
    assert!(!output.timed_out);
    assert_eq!(output.agents.len(), 2);
    assert_agent_status(
        &output.agents,
        errored_agent_id,
        AgentStatus::Errored("usage limit".to_string()),
    );
    let running_state = output
        .agents
        .get(&running_agent_id)
        .expect("running agent should be reported");
    assert!(matches!(
        running_state.status,
        AgentStatus::PendingInit | AgentStatus::Running
    ));

    let _ = errored_thread
        .thread
        .submit(Op::Shutdown {})
        .await
        .expect("shutdown should submit");
    let _ = running_thread
        .thread
        .submit(Op::Shutdown {})
        .await
        .expect("shutdown should submit");
}

#[tokio::test]
async fn multi_agent_v2_wait_agent_returns_summary_for_mailbox_activity() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.conversation_id = root.thread_id;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    turn.config = Arc::new(config);

    let session = Arc::new(session);
    let turn = Arc::new(turn);
    let spawn_output = SpawnAgentHandlerV2
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "spawn_agent",
            function_payload(json!({
                "message": "inspect this repo",
                "task_name": "test_process"
            })),
        ))
        .await
        .expect("spawn_agent should succeed");
    let _ = expect_text_output(spawn_output);

    let agent_id = session
        .services
        .agent_control
        .resolve_agent_reference(
            session.conversation_id,
            &turn.session_source,
            "test_process",
        )
        .await
        .expect("relative path should resolve");
    let worker_path = session
        .services
        .agent_control
        .get_agent_metadata(agent_id)
        .expect("worker metadata")
        .agent_path
        .expect("worker path");
    let wait_task = tokio::spawn({
        let session = session.clone();
        let turn = turn.clone();
        async move {
            WaitAgentHandlerV2
                .handle(invocation(
                    session,
                    turn,
                    "wait_agent",
                    function_payload(json!({"timeout_ms": 1000})),
                ))
                .await
        }
    });
    tokio::task::yield_now().await;

    session.enqueue_mailbox_communication(InterAgentCommunication::new(
        worker_path,
        AgentPath::root(),
        Vec::new(),
        "completed".to_string(),
        /*trigger_turn*/ false,
    ));

    let wait_output = wait_task
        .await
        .expect("wait task should join")
        .expect("wait_agent should succeed");
    let (content, success) = expect_text_output(wait_output);
    let result: crate::tools::handlers::multi_agents_v2::wait::WaitAgentResult =
        serde_json::from_str(&content).expect("wait_agent result should be json");
    assert_eq!(
        result,
        crate::tools::handlers::multi_agents_v2::wait::WaitAgentResult {
            message: "Wait completed.".to_string(),
            timed_out: false,
        }
    );
    assert_eq!(success, None);
}

#[tokio::test]
async fn multi_agent_v2_wait_agent_waits_for_new_mail_after_start() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.conversation_id = root.thread_id;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    turn.config = Arc::new(config);
    let session = Arc::new(session);
    let turn = Arc::new(turn);

    SpawnAgentHandlerV2
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "spawn_agent",
            function_payload(json!({
                "message": "boot worker",
                "task_name": "worker"
            })),
        ))
        .await
        .expect("spawn worker");
    let agent_id = session
        .services
        .agent_control
        .resolve_agent_reference(session.conversation_id, &turn.session_source, "worker")
        .await
        .expect("worker should resolve");
    let worker_path = session
        .services
        .agent_control
        .get_agent_metadata(agent_id)
        .expect("worker metadata")
        .agent_path
        .expect("worker path");

    session.enqueue_mailbox_communication(InterAgentCommunication::new(
        worker_path.clone(),
        AgentPath::root(),
        Vec::new(),
        "already queued".to_string(),
        /*trigger_turn*/ false,
    ));

    let wait_task = tokio::spawn({
        let session = session.clone();
        let turn = turn.clone();
        async move {
            WaitAgentHandlerV2
                .handle(invocation(
                    session,
                    turn,
                    "wait_agent",
                    function_payload(json!({"timeout_ms": 1000})),
                ))
                .await
        }
    });
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !wait_task.is_finished(),
        "mail already queued before wait should not wake wait_agent"
    );

    session.enqueue_mailbox_communication(InterAgentCommunication::new(
        worker_path,
        AgentPath::root(),
        Vec::new(),
        "new mail".to_string(),
        /*trigger_turn*/ false,
    ));

    let output = wait_task
        .await
        .expect("wait task should join")
        .expect("wait_agent should succeed");
    let (content, success) = expect_text_output(output);
    let result: crate::tools::handlers::multi_agents_v2::wait::WaitAgentResult =
        serde_json::from_str(&content).expect("wait_agent result should be json");
    assert_eq!(
        result,
        crate::tools::handlers::multi_agents_v2::wait::WaitAgentResult {
            message: "Wait completed.".to_string(),
            timed_out: false,
        }
    );
    assert_eq!(success, None);
}

#[tokio::test]
async fn multi_agent_v2_wait_agent_wakes_on_any_mailbox_notification() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.conversation_id = root.thread_id;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    turn.config = Arc::new(config);
    let session = Arc::new(session);
    let turn = Arc::new(turn);

    for task_name in ["worker_a", "worker_b"] {
        SpawnAgentHandlerV2
            .handle(invocation(
                session.clone(),
                turn.clone(),
                "spawn_agent",
                function_payload(json!({
                    "message": format!("boot {task_name}"),
                    "task_name": task_name
                })),
            ))
            .await
            .expect("spawn worker");
    }
    let worker_b_id = session
        .services
        .agent_control
        .resolve_agent_reference(session.conversation_id, &turn.session_source, "worker_b")
        .await
        .expect("worker_b should resolve");
    let worker_b_path = session
        .services
        .agent_control
        .get_agent_metadata(worker_b_id)
        .expect("worker_b metadata")
        .agent_path
        .expect("worker_b path");

    let wait_task = tokio::spawn({
        let session = session.clone();
        let turn = turn.clone();
        async move {
            WaitAgentHandlerV2
                .handle(invocation(
                    session,
                    turn,
                    "wait_agent",
                    function_payload(json!({"timeout_ms": 1000})),
                ))
                .await
        }
    });
    tokio::task::yield_now().await;

    session.enqueue_mailbox_communication(InterAgentCommunication::new(
        worker_b_path,
        AgentPath::root(),
        Vec::new(),
        "from worker b".to_string(),
        /*trigger_turn*/ false,
    ));

    let output = wait_task
        .await
        .expect("wait task should join")
        .expect("wait_agent should succeed");
    let (content, success) = expect_text_output(output);
    let result: crate::tools::handlers::multi_agents_v2::wait::WaitAgentResult =
        serde_json::from_str(&content).expect("wait_agent result should be json");
    assert_eq!(
        result,
        crate::tools::handlers::multi_agents_v2::wait::WaitAgentResult {
            message: "Wait completed.".to_string(),
            timed_out: false,
        }
    );
    assert_eq!(success, None);
}

#[tokio::test]
async fn multi_agent_v2_wait_agent_does_not_return_completed_content() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.conversation_id = root.thread_id;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    turn.config = Arc::new(config);
    let session = Arc::new(session);
    let turn = Arc::new(turn);

    SpawnAgentHandlerV2
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "spawn_agent",
            function_payload(json!({
                "message": "boot worker",
                "task_name": "worker"
            })),
        ))
        .await
        .expect("spawn worker");
    let agent_id = session
        .services
        .agent_control
        .resolve_agent_reference(session.conversation_id, &turn.session_source, "worker")
        .await
        .expect("worker should resolve");
    let worker_path = session
        .services
        .agent_control
        .get_agent_metadata(agent_id)
        .expect("worker metadata")
        .agent_path
        .expect("worker path");
    let wait_task = tokio::spawn({
        let session = session.clone();
        let turn = turn.clone();
        async move {
            WaitAgentHandlerV2
                .handle(invocation(
                    session,
                    turn,
                    "wait_agent",
                    function_payload(json!({"timeout_ms": 1000})),
                ))
                .await
        }
    });
    tokio::task::yield_now().await;

    session.enqueue_mailbox_communication(InterAgentCommunication::new(
        worker_path,
        AgentPath::root(),
        Vec::new(),
        "sensitive child output".to_string(),
        /*trigger_turn*/ false,
    ));

    let output = wait_task
        .await
        .expect("wait task should join")
        .expect("wait_agent should succeed");
    let (content, success) = expect_text_output(output);
    let result: crate::tools::handlers::multi_agents_v2::wait::WaitAgentResult =
        serde_json::from_str(&content).expect("wait_agent result should be json");
    assert_eq!(
        result,
        crate::tools::handlers::multi_agents_v2::wait::WaitAgentResult {
            message: "Wait completed.".to_string(),
            timed_out: false,
        }
    );
    assert!(!content.contains("sensitive child output"));
    assert_eq!(success, None);
}

#[tokio::test]
async fn multi_agent_v2_close_agent_accepts_task_name_target() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.conversation_id = root.thread_id;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    turn.config = Arc::new(config);

    let session = Arc::new(session);
    let turn = Arc::new(turn);
    SpawnAgentHandlerV2
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "spawn_agent",
            function_payload(json!({
                "message": "inspect this repo",
                "task_name": "worker"
            })),
        ))
        .await
        .expect("spawn_agent should succeed");

    let agent_id = session
        .services
        .agent_control
        .resolve_agent_reference(session.conversation_id, &turn.session_source, "worker")
        .await
        .expect("worker path should resolve");

    let output = CloseAgentHandlerV2
        .handle(invocation(
            session,
            turn,
            "close_agent",
            function_payload(json!({"target": "worker"})),
        ))
        .await
        .expect("close_agent should succeed for v2 task names");
    let (content, success) = expect_text_output(output);
    let result: crate::tools::handlers::multi_agents_v2::CloseAgentResult =
        serde_json::from_str(&content).expect("close_agent result should be json");
    assert_ne!(result.previous_status, AgentStatus::NotFound);
    assert_eq!(success, Some(true));
    assert_eq!(
        manager.agent_control().get_status(agent_id).await,
        AgentStatus::NotFound
    );
}

#[tokio::test]
async fn multi_agent_v2_close_agent_rejects_root_target_and_id() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.conversation_id = root.thread_id;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    turn.config = Arc::new(config);

    let session = Arc::new(session);
    let turn = Arc::new(turn);
    let root_path_error = CloseAgentHandlerV2
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "close_agent",
            function_payload(json!({"target": "/root"})),
        ))
        .await
        .expect_err("close_agent should reject the root path");
    assert_eq!(
        root_path_error,
        FunctionCallError::RespondToModel("root is not a spawned agent".to_string())
    );

    let root_id_error = CloseAgentHandlerV2
        .handle(invocation(
            session,
            turn,
            "close_agent",
            function_payload(json!({"target": root.thread_id.to_string()})),
        ))
        .await
        .expect_err("close_agent should reject the root thread id");
    assert_eq!(
        root_id_error,
        FunctionCallError::RespondToModel("root is not a spawned agent".to_string())
    );
}

#[tokio::test]
async fn close_agent_rejects_invalid_id_target() {
    let (mut session, turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.conversation_id = root.thread_id;

    let error = CloseAgentHandler
        .handle(invocation(
            Arc::new(session),
            Arc::new(turn),
            "close_agent",
            function_payload(json!({"target": "worker"})),
        ))
        .await
        .expect_err("close_agent should reject non-id legacy targets");
    let FunctionCallError::RespondToModel(message) = error else {
        panic!("expected respond-to-model error");
    };
    assert!(message.starts_with("invalid agent id worker:"));
}

#[tokio::test]
async fn close_agent_rejects_root_path_target() {
    let (mut session, turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.conversation_id = root.thread_id;

    let error = CloseAgentHandler
        .handle(invocation(
            Arc::new(session),
            Arc::new(turn),
            "close_agent",
            function_payload(json!({"target": "/root"})),
        ))
        .await
        .expect_err("close_agent should reject the root path");
    assert_eq!(
        error,
        FunctionCallError::RespondToModel("root is not a spawned agent".to_string())
    );
}

#[tokio::test]
async fn close_agent_rejects_root_thread_target() {
    let (mut session, turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.conversation_id = root.thread_id;

    let error = CloseAgentHandler
        .handle(invocation(
            Arc::new(session),
            Arc::new(turn),
            "close_agent",
            function_payload(json!({"target": root.thread_id.to_string()})),
        ))
        .await
        .expect_err("close_agent should reject the root thread id");
    assert_eq!(
        error,
        FunctionCallError::RespondToModel("root is not a spawned agent".to_string())
    );
}

#[tokio::test]
async fn close_agent_submits_shutdown_and_returns_previous_status() {
    let (mut session, turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();
    let config = turn.config.as_ref().clone();
    let thread = manager.start_thread(config).await.expect("start thread");
    let agent_id = thread.thread_id;
    let status_before = manager.agent_control().get_status(agent_id).await;

    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "close_agent",
        function_payload(json!({"target": agent_id.to_string()})),
    );
    let output = CloseAgentHandler
        .handle(invocation)
        .await
        .expect("close_agent should succeed");
    let (content, success) = expect_text_output(output);
    let result: close_agent::CloseAgentResult =
        serde_json::from_str(&content).expect("close_agent result should be json");
    assert_eq!(result.previous_status, status_before);
    assert_eq!(success, Some(true));

    let ops = manager.captured_ops();
    let submitted_shutdown = ops
        .iter()
        .any(|(id, op)| *id == agent_id && matches!(op, Op::Shutdown));
    assert_eq!(submitted_shutdown, true);

    let status_after = manager.agent_control().get_status(agent_id).await;
    assert_eq!(status_after, AgentStatus::NotFound);
}

#[tokio::test]
async fn tool_handlers_cascade_close_and_resume_and_keep_explicitly_closed_subtrees_closed() {
    let (_session, turn) = make_session_and_context().await;
    let manager = thread_manager();
    let mut config = turn.config.as_ref().clone();
    config.agent_max_depth = 3;
    config
        .features
        .enable(Feature::Sqlite)
        .expect("test config should allow sqlite");

    let parent = manager
        .start_thread(config.clone())
        .await
        .expect("parent thread should start");
    let parent_thread_id = parent.thread_id;
    let parent_session = parent.thread.codex.session.clone();

    let child_spawn_output = SpawnAgentHandler
        .handle(invocation(
            parent_session.clone(),
            parent_session.new_default_turn().await,
            "spawn_agent",
            function_payload(json!({"message": "hello child"})),
        ))
        .await
        .expect("child spawn should succeed");
    let (child_content, child_success) = expect_text_output(child_spawn_output);
    let child_result: serde_json::Value =
        serde_json::from_str(&child_content).expect("child spawn result should be json");
    let child_thread_id = parse_agent_id(
        child_result
            .get("agent_id")
            .and_then(serde_json::Value::as_str)
            .expect("child spawn result should include agent_id"),
    );
    assert_eq!(child_success, Some(true));

    let child_thread = manager
        .get_thread(child_thread_id)
        .await
        .expect("child thread should exist");
    let child_session = child_thread.codex.session.clone();
    let grandchild_spawn_output = SpawnAgentHandler
        .handle(invocation(
            child_session.clone(),
            child_session.new_default_turn().await,
            "spawn_agent",
            function_payload(json!({"message": "hello grandchild"})),
        ))
        .await
        .expect("grandchild spawn should succeed");
    let (grandchild_content, grandchild_success) = expect_text_output(grandchild_spawn_output);
    let grandchild_result: serde_json::Value =
        serde_json::from_str(&grandchild_content).expect("grandchild spawn result should be json");
    let grandchild_thread_id = parse_agent_id(
        grandchild_result
            .get("agent_id")
            .and_then(serde_json::Value::as_str)
            .expect("grandchild spawn result should include agent_id"),
    );
    assert_eq!(grandchild_success, Some(true));

    let close_output = CloseAgentHandler
        .handle(invocation(
            parent_session.clone(),
            parent_session.new_default_turn().await,
            "close_agent",
            function_payload(json!({"target": child_thread_id.to_string()})),
        ))
        .await
        .expect("close_agent should close the child subtree");
    let (close_content, close_success) = expect_text_output(close_output);
    let close_result: close_agent::CloseAgentResult =
        serde_json::from_str(&close_content).expect("close_agent result should be json");
    assert_ne!(close_result.previous_status, AgentStatus::NotFound);
    assert_eq!(close_success, Some(true));
    assert_eq!(
        manager.agent_control().get_status(child_thread_id).await,
        AgentStatus::NotFound
    );
    assert_eq!(
        manager
            .agent_control()
            .get_status(grandchild_thread_id)
            .await,
        AgentStatus::NotFound
    );

    let child_resume_output = ResumeAgentHandler
        .handle(invocation(
            parent_session.clone(),
            parent_session.new_default_turn().await,
            "resume_agent",
            function_payload(json!({"id": child_thread_id.to_string()})),
        ))
        .await
        .expect("resume_agent should reopen the child subtree");
    let (child_resume_content, child_resume_success) = expect_text_output(child_resume_output);
    let child_resume_result: resume_agent::ResumeAgentResult =
        serde_json::from_str(&child_resume_content).expect("resume result should be json");
    assert_ne!(child_resume_result.status, AgentStatus::NotFound);
    assert_eq!(child_resume_success, Some(true));
    assert_ne!(
        manager.agent_control().get_status(child_thread_id).await,
        AgentStatus::NotFound
    );
    assert_ne!(
        manager
            .agent_control()
            .get_status(grandchild_thread_id)
            .await,
        AgentStatus::NotFound
    );

    let close_again_output = CloseAgentHandler
        .handle(invocation(
            parent_session.clone(),
            parent_session.new_default_turn().await,
            "close_agent",
            function_payload(json!({"target": child_thread_id.to_string()})),
        ))
        .await
        .expect("close_agent should be repeatable for the child subtree");
    let (close_again_content, close_again_success) = expect_text_output(close_again_output);
    let close_again_result: close_agent::CloseAgentResult =
        serde_json::from_str(&close_again_content)
            .expect("second close_agent result should be json");
    assert_ne!(close_again_result.previous_status, AgentStatus::NotFound);
    assert_eq!(close_again_success, Some(true));
    assert_eq!(
        manager.agent_control().get_status(child_thread_id).await,
        AgentStatus::NotFound
    );
    assert_eq!(
        manager
            .agent_control()
            .get_status(grandchild_thread_id)
            .await,
        AgentStatus::NotFound
    );

    let operator = manager
        .start_thread(config)
        .await
        .expect("operator thread should start");
    let operator_session = operator.thread.codex.session.clone();
    let _ = manager
        .agent_control()
        .shutdown_live_agent(parent_thread_id)
        .await
        .expect("parent shutdown should succeed");
    assert_eq!(
        manager.agent_control().get_status(parent_thread_id).await,
        AgentStatus::NotFound
    );

    let parent_resume_output = ResumeAgentHandler
        .handle(invocation(
            operator_session,
            operator.thread.codex.session.new_default_turn().await,
            "resume_agent",
            function_payload(json!({"id": parent_thread_id.to_string()})),
        ))
        .await
        .expect("resume_agent should reopen the parent thread");
    let (parent_resume_content, parent_resume_success) = expect_text_output(parent_resume_output);
    let parent_resume_result: resume_agent::ResumeAgentResult =
        serde_json::from_str(&parent_resume_content).expect("parent resume result should be json");
    assert_ne!(parent_resume_result.status, AgentStatus::NotFound);
    assert_eq!(parent_resume_success, Some(true));
    assert_ne!(
        manager.agent_control().get_status(parent_thread_id).await,
        AgentStatus::NotFound
    );
    assert_eq!(
        manager.agent_control().get_status(child_thread_id).await,
        AgentStatus::NotFound
    );
    assert_eq!(
        manager
            .agent_control()
            .get_status(grandchild_thread_id)
            .await,
        AgentStatus::NotFound
    );

    let shutdown_report = manager
        .shutdown_all_threads_bounded(Duration::from_secs(5))
        .await;
    assert_eq!(shutdown_report.submit_failed, Vec::<ThreadId>::new());
    assert_eq!(shutdown_report.timed_out, Vec::<ThreadId>::new());
}

#[tokio::test]
async fn build_agent_spawn_config_uses_turn_context_values() {
    fn pick_allowed_sandbox_policy(
        constraint: &crate::config::Constrained<SandboxPolicy>,
        base: SandboxPolicy,
    ) -> SandboxPolicy {
        let candidates = [
            SandboxPolicy::new_read_only_policy(),
            SandboxPolicy::new_workspace_write_policy(),
            SandboxPolicy::DangerFullAccess,
        ];
        candidates
            .into_iter()
            .find(|candidate| *candidate != base && constraint.can_set(candidate).is_ok())
            .unwrap_or(base)
    }

    let (_session, mut turn) = make_session_and_context().await;
    let mut base_config = (*turn.config).clone();
    base_config.subagent_base_instructions = Some("base".to_string());
    base_config.developer_instructions = Some("base-dev".to_string());
    base_config.user_instructions = Some("base-user".to_string());
    base_config.project_doc_max_bytes = 4_321;
    base_config
        .features
        .enable(Feature::ChildAgentsMd)
        .expect("child agents md should enable");
    turn.config = Arc::new(base_config.clone());
    turn.developer_instructions = Some("resolved-dev".to_string());
    turn.compact_prompt = Some("compact".to_string());
    turn.shell_environment_policy = ShellEnvironmentPolicy {
        use_profile: true,
        ..ShellEnvironmentPolicy::default()
    };
    let temp_dir = tempfile::tempdir().expect("temp dir");
    turn.cwd = temp_dir.abs();
    turn.codex_linux_sandbox_exe = Some(PathBuf::from("/bin/echo"));
    let sandbox_policy = pick_allowed_sandbox_policy(
        &turn.config.permissions.sandbox_policy,
        turn.config.permissions.sandbox_policy.get().clone(),
    );
    let file_system_sandbox_policy =
        FileSystemSandboxPolicy::from_legacy_sandbox_policy(&sandbox_policy, &turn.cwd);
    let network_sandbox_policy = NetworkSandboxPolicy::from(&sandbox_policy);
    turn.sandbox_policy
        .set(sandbox_policy)
        .expect("sandbox policy set");
    turn.file_system_sandbox_policy = file_system_sandbox_policy.clone();
    turn.network_sandbox_policy = network_sandbox_policy;
    turn.approval_policy
        .set(AskForApproval::OnRequest)
        .expect("approval policy set");

    let config = build_agent_spawn_config(&turn).expect("spawn config");
    let mut expected = base_config;
    expected.base_instructions = Some("base".to_string());
    expected.model = Some(turn.model_info.slug.clone());
    expected.model_provider = turn.provider.clone();
    expected.model_reasoning_effort = turn.reasoning_effort;
    expected.model_reasoning_summary = Some(turn.reasoning_summary);
    expected.developer_instructions = Some("base-dev".to_string());
    expected.compact_prompt = turn.compact_prompt.clone();
    expected.permissions.shell_environment_policy = turn.shell_environment_policy.clone();
    expected.codex_linux_sandbox_exe = turn.codex_linux_sandbox_exe.clone();
    expected.cwd = turn.cwd.clone();
    expected
        .permissions
        .approval_policy
        .set(AskForApproval::OnRequest)
        .expect("approval policy set");
    expected
        .permissions
        .sandbox_policy
        .set(turn.sandbox_policy.get().clone())
        .expect("sandbox policy set");
    expected.permissions.file_system_sandbox_policy = file_system_sandbox_policy;
    expected.permissions.network_sandbox_policy = network_sandbox_policy;
    expected.pos_compact_instructions =
        Some(crate::codex::SUBAGENT_AUTO_COMPACT_RECALL_WARNING_BODY.to_string());
    assert_eq!(config, expected);
}

#[tokio::test]
async fn build_agent_spawn_config_preserves_user_instructions() {
    let (_session, mut turn) = make_session_and_context().await;
    let mut base_config = (*turn.config).clone();
    base_config.user_instructions = Some("base-user".to_string());
    turn.user_instructions = Some("resolved-user".to_string());
    turn.config = Arc::new(base_config);

    let config = build_agent_spawn_config(&turn).expect("spawn config");

    assert_eq!(config.user_instructions.as_deref(), Some("base-user"));
}

#[tokio::test]
async fn build_agent_resume_config_clears_base_instructions() {
    let (_session, mut turn) = make_session_and_context().await;
    let mut base_config = (*turn.config).clone();
    base_config.base_instructions = Some("caller-base".to_string());
    base_config.developer_instructions = Some("resume-dev".to_string());
    base_config.user_instructions = Some("resume-user".to_string());
    base_config.project_doc_max_bytes = 2_468;
    base_config
        .features
        .enable(Feature::ChildAgentsMd)
        .expect("child agents md should enable");
    turn.config = Arc::new(base_config);
    turn.developer_instructions = Some("resolved-resume-dev".to_string());
    turn.approval_policy
        .set(AskForApproval::OnRequest)
        .expect("approval policy set");

    let config = build_agent_resume_config(&turn, /*child_depth*/ 0).expect("resume config");

    let mut expected = (*turn.config).clone();
    expected.base_instructions = None;
    expected.model = Some(turn.model_info.slug.clone());
    expected.model_provider = turn.provider.clone();
    expected.model_reasoning_effort = turn.reasoning_effort;
    expected.model_reasoning_summary = Some(turn.reasoning_summary);
    expected.developer_instructions = Some("resume-dev".to_string());
    expected.compact_prompt = turn.compact_prompt.clone();
    expected.permissions.shell_environment_policy = turn.shell_environment_policy.clone();
    expected.codex_linux_sandbox_exe = turn.codex_linux_sandbox_exe.clone();
    expected.cwd = turn.cwd.clone();
    expected
        .permissions
        .approval_policy
        .set(AskForApproval::OnRequest)
        .expect("approval policy set");
    expected
        .permissions
        .sandbox_policy
        .set(turn.sandbox_policy.get().clone())
        .expect("sandbox policy set");
    expected.pos_compact_instructions =
        Some(crate::codex::SUBAGENT_AUTO_COMPACT_RECALL_WARNING_BODY.to_string());
    assert_eq!(config, expected);
}
