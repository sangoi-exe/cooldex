#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "support/fake_peer.rs"]
mod fake_peer;
#[path = "support/credentials.rs"]
mod credentials;

use codex_cursor_agent_service::AgentServiceTransport;
use codex_cursor_agent_service::AgentServiceTransportError;
use codex_cursor_agent_service::CursorSamplingRequest;
use codex_cursor_agent_service::CursorSamplingSession;
use codex_cursor_agent_service::map_sampling_request;
use codex_cursor_agent_service::proto::AGENT_SERVICE_TYPE_NAME;
use codex_cursor_agent_service::proto::AgentClientMessage;
use codex_cursor_agent_service::proto::AgentRunRequest;
use codex_cursor_agent_service::proto::AgentServerMessage;
use codex_cursor_agent_service::proto::ClientHeartbeat;
use codex_cursor_agent_service::proto::ExecServerMessage;
use codex_cursor_agent_service::proto::HEARTBEAT_INTERVAL_SECONDS;
use codex_cursor_agent_service::proto::InteractionUpdate;
use codex_cursor_agent_service::proto::McpArgs;
use codex_cursor_agent_service::proto::PINNED_CURSOR_CLI_SHA256;
use codex_cursor_agent_service::proto::PINNED_CURSOR_CLI_VERSION;
use codex_cursor_agent_service::proto::PINNED_SCHEMA_SHA256;
use codex_cursor_agent_service::proto::TextDeltaUpdate;
use codex_cursor_agent_service::proto::TurnEndedUpdate;
use codex_cursor_agent_service::proto::UnsupportedAgentServerMessage;
use codex_cursor_agent_service::proto::UnsupportedExecServerMessage;
use codex_cursor_agent_service::proto::UnsupportedInteractionUpdate;
use codex_cursor_agent_service::proto::agent_client_message;
use codex_cursor_agent_service::proto::agent_server_message;
use codex_cursor_agent_service::proto::exec_server_message;
use codex_cursor_agent_service::proto::interaction_update;
use codex_api::ResponseEvent;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::models::ResponseItem;
use codex_tools::AdditionalProperties;
use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolSpec;
use fake_peer::FakePeer;
use credentials::ACCESS_TOKEN;
use credentials::start_run;
use pretty_assertions::assert_eq;
use prost::Message as _;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const EXPECTED_CURSOR_CLI_SHA256: &str =
    "eed61c5224668c9236334c4c68936a16aecc37374b592f59e31eb50433817831";
const EXPECTED_SCHEMA_SHA256: &str =
    "eac12505afd2b0b15fa8d416c4ae4d65a4534ee31a465ab37e1e3f105a943413";

#[test]
fn generated_protocol_records_the_pinned_snapshot() {
    assert_eq!(PINNED_CURSOR_CLI_VERSION, "2026.07.23-e383d2b");
    assert_eq!(PINNED_CURSOR_CLI_SHA256, EXPECTED_CURSOR_CLI_SHA256);
    assert_eq!(PINNED_SCHEMA_SHA256, EXPECTED_SCHEMA_SHA256);
    assert_eq!(AGENT_SERVICE_TYPE_NAME, "agent.v1.AgentService");
    assert_eq!(HEARTBEAT_INTERVAL_SECONDS, 5);
}

#[test]
fn generated_protocol_preserves_the_wp3_security_and_history_field_numbers() {
    use codex_cursor_agent_service::proto::AgentConversationTurnStructure;
    use codex_cursor_agent_service::proto::AssistantMessage;
    use codex_cursor_agent_service::proto::ConversationStep;
    use codex_cursor_agent_service::proto::ConversationTurnStructure;
    use codex_cursor_agent_service::proto::McpArgs;
    use codex_cursor_agent_service::proto::McpSuccess;
    use codex_cursor_agent_service::proto::McpToolCall;
    use codex_cursor_agent_service::proto::McpToolResult;
    use codex_cursor_agent_service::proto::SmartModeApproval;
    use codex_cursor_agent_service::proto::ToolCall;
    use codex_cursor_agent_service::proto::conversation_step;
    use codex_cursor_agent_service::proto::conversation_turn_structure;
    use codex_cursor_agent_service::proto::mcp_tool_result;
    use codex_cursor_agent_service::proto::tool_call;

    let approval = SmartModeApproval {
        request_id: "r".to_string(),
        reason: "x".to_string(),
    };
    assert_eq!(approval.encode_to_vec(), b"\x0a\x01r\x12\x01x");
    assert_eq!(
        McpArgs {
            smart_mode_approval: Some(approval),
            ..Default::default()
        }
        .encode_to_vec(),
        b"\x32\x06\x0a\x01r\x12\x01x"
    );

    let agent_turn = AgentConversationTurnStructure {
        user_message: b"u".to_vec(),
        steps: vec![b"s".to_vec()],
        request_id: Some("r".to_string()),
        encrypted_model: Some("e".to_string()),
        dynamic_tool_count: Some(1),
    };
    assert_eq!(
        agent_turn.encode_to_vec(),
        b"\x0a\x01u\x12\x01s\x1a\x01r\x22\x01e\x28\x01"
    );
    assert_eq!(
        ConversationTurnStructure {
            turn: Some(conversation_turn_structure::Turn::AgentConversationTurn(
                AgentConversationTurnStructure::default(),
            )),
        }
        .encode_to_vec(),
        b"\x0a\x00"
    );
    assert_eq!(
        ConversationStep {
            message: Some(conversation_step::Message::AssistantMessage(
                AssistantMessage {
                    text: "a".to_string(),
                },
            )),
        }
        .encode_to_vec(),
        b"\x0a\x03\x0a\x01a"
    );
    assert_eq!(
        ToolCall {
            tool: Some(tool_call::Tool::McpToolCall(McpToolCall::default())),
            tool_call_id: None,
        }
        .encode_to_vec(),
        b"\x7a\x00"
    );
    assert_eq!(
        ToolCall {
            tool: None,
            tool_call_id: Some("c".to_string()),
        }
        .encode_to_vec(),
        b"\xca\x03\x01c"
    );
    assert_eq!(
        McpToolResult {
            result: Some(mcp_tool_result::Result::Success(McpSuccess::default())),
        }
        .encode_to_vec(),
        b"\x0a\x00"
    );
}

#[tokio::test]
async fn opens_one_bidirectional_run_and_observes_the_explicit_terminal() {
    let mut peer = FakePeer::spawn().await;
    let mut transport = AgentServiceTransport::connect(peer.endpoint())
        .await
        .unwrap();
    let request = minimal_run_request();
    let mut run = start_run(&mut transport, request.clone()).await.unwrap();
    let mut server_run = peer.next_run().await;
    assert_eq!(
        server_run
            .metadata()
            .get("authorization")
            .unwrap()
            .to_str()
            .unwrap(),
        format!("Bearer {ACCESS_TOKEN}")
    );
    assert_eq!(
        server_run
            .metadata()
            .get("x-cursor-client-version")
            .unwrap(),
        "cli-2026.07.23-e383d2b"
    );
    assert_eq!(
        server_run
            .metadata()
            .get("x-cursor-client-type")
            .unwrap(),
        "cli"
    );
    assert_eq!(
        server_run.metadata().get("x-cursor-streaming").unwrap(),
        "true"
    );
    assert_eq!(server_run.metadata().get("x-ghost-mode").unwrap(), "true");
    assert!(server_run.metadata().get("x-request-id").is_some());

    assert_eq!(
        server_run.next_client_message().await,
        AgentClientMessage {
            message: Some(agent_client_message::Message::RunRequest(request)),
        }
    );

    let text_delta = text_delta("hello from Cursor");
    server_run.send(text_delta.clone()).await;
    assert_eq!(run.next_server_message().await.unwrap(), text_delta);

    let terminal = turn_ended();
    server_run.send(terminal.clone()).await;
    assert_eq!(run.next_server_message().await.unwrap(), terminal);

    drop(run);
    server_run.close();
    peer.shutdown().await;
}

#[tokio::test]
async fn session_returns_a_tool_result_on_the_same_run_before_terminal() {
    let input = vec![user_message("Use echo")];
    let tools = vec![function_spec("echo")];
    let mapped = map_sampling_request(CursorSamplingRequest {
        conversation_id: "cooldex-run-1",
        model_id: "composer-2.5",
        model_display_name: "Composer 2.5",
        base_instructions: "exact Cooldex instructions",
        input: &input,
        tools: &tools,
        current_message_id: "current-message",
        synthesized_user_message: None,
    })
    .expect("sampling request should map");
    let expected_request = mapped.request.clone();
    let definition = expected_request
        .mcp_tools
        .as_ref()
        .expect("mapped request should advertise tools")
        .mcp_tools[0]
        .clone();

    let mut peer = FakePeer::spawn().await;
    let mut transport = AgentServiceTransport::connect(peer.endpoint())
        .await
        .unwrap();
    let run = start_run(&mut transport, mapped.request).await.unwrap();
    let mut server_run = peer.next_run().await;
    assert_eq!(
        server_run.next_client_message().await,
        AgentClientMessage {
            message: Some(agent_client_message::Message::RunRequest(expected_request)),
        }
    );

    let mut session = CursorSamplingSession::start(
        run,
        mapped.tool_snapshot,
        "exact Cooldex instructions".to_string(),
        "cursor-response-1".to_string(),
        8,
        CancellationToken::new(),
    );
    assert!(matches!(
        session.next_event().await,
        Some(Ok(ResponseEvent::Created))
    ));

    server_run
        .send(mcp_server_message(41, "exec-41", McpArgs {
            name: definition.name,
            args: HashMap::new(),
            tool_call_id: "cursor-action-1".to_string(),
            provider_identifier: definition.provider_identifier,
            tool_name: definition.tool_name,
            smart_mode_approval: None,
            smart_mode_approval_only: false,
            skip_approval: false,
            server_identifier: "cooldex".to_string(),
        }))
        .await;

    let call_id = match session.next_event().await {
        Some(Ok(ResponseEvent::OutputItemAdded(ResponseItem::FunctionCall {
            call_id,
            ..
        }))) => call_id,
        other => panic!("expected added function call, got {other:?}"),
    };
    assert!(matches!(
        session.next_event().await,
        Some(Ok(ResponseEvent::OutputItemDone(ResponseItem::FunctionCall { call_id: ref done_call_id, .. })))
            if done_call_id == &call_id
    ));

    session
        .send_tool_result(ResponseInputItem::FunctionCallOutput {
            call_id: call_id.clone(),
            output: FunctionCallOutputPayload {
                body: FunctionCallOutputBody::Text("done".to_string()),
                success: Some(true),
            },
        })
        .await
        .expect("same-stream result should be accepted");
    let result = server_run.next_client_message().await;
    let Some(agent_client_message::Message::ExecClientMessage(result)) = result.message else {
        panic!("expected same-Run exec result");
    };
    assert_eq!(result.id, 41);
    assert_eq!(result.exec_id, "exec-41");

    server_run.send(turn_ended()).await;
    assert!(matches!(
        session.next_event().await,
        Some(Ok(ResponseEvent::Completed { ref response_id, .. }))
            if response_id == "cursor-response-1"
    ));
    assert!(session.next_event().await.is_none());
    assert!(
        tokio::time::timeout(Duration::from_millis(100), peer.next_run())
            .await
            .is_err(),
        "same-stream tool completion must not open a second Run"
    );

    server_run.close();
    peer.shutdown().await;
}

#[tokio::test]
async fn sends_a_client_heartbeat_while_the_run_is_open() {
    let mut peer = FakePeer::spawn().await;
    let mut transport = AgentServiceTransport::connect(peer.endpoint())
        .await
        .unwrap();
    let run = start_run(&mut transport, minimal_run_request()).await.unwrap();
    let mut server_run = peer.next_run().await;
    let _initial_request = server_run.next_client_message().await;

    let heartbeat = tokio::time::timeout(
        Duration::from_secs(HEARTBEAT_INTERVAL_SECONDS + 1),
        server_run.next_client_message(),
    )
    .await
    .expect("client did not send the pinned heartbeat");
    assert_eq!(
        heartbeat,
        AgentClientMessage {
            message: Some(agent_client_message::Message::ClientHeartbeat(
                ClientHeartbeat {}
            )),
        }
    );

    drop(run);
    server_run.close();
    peer.shutdown().await;
}

#[tokio::test]
async fn rejects_eof_before_turn_ended() {
    let mut peer = FakePeer::spawn().await;
    let mut transport = AgentServiceTransport::connect(peer.endpoint())
        .await
        .unwrap();
    let mut run = start_run(&mut transport, minimal_run_request()).await.unwrap();
    let mut server_run = peer.next_run().await;
    let _initial_request = server_run.next_client_message().await;
    server_run.close();

    assert_eq!(
        run.next_server_message().await.unwrap_err(),
        AgentServiceTransportError::UnexpectedEof
    );

    drop(run);
    peer.shutdown().await;
}

#[tokio::test]
async fn rejects_an_empty_server_envelope() {
    let mut peer = FakePeer::spawn().await;
    let mut transport = AgentServiceTransport::connect(peer.endpoint())
        .await
        .unwrap();
    let mut run = start_run(&mut transport, minimal_run_request()).await.unwrap();
    let mut server_run = peer.next_run().await;
    let _initial_request = server_run.next_client_message().await;
    server_run.send(AgentServerMessage { message: None }).await;

    assert_eq!(
        run.next_server_message().await.unwrap_err(),
        AgentServiceTransportError::EmptyServerMessage
    );

    drop(run);
    server_run.close();
    peer.shutdown().await;
}

#[tokio::test]
async fn rejects_every_pinned_unsupported_server_envelope() {
    let messages = unsupported_server_envelopes();
    assert_eq!(messages.len(), 4);
    assert_rejected_messages(
        messages,
        AgentServiceTransportError::UnsupportedServerMessage,
    )
    .await;
}

#[tokio::test]
async fn rejects_every_pinned_unsupported_interaction_update() {
    let messages = unsupported_interaction_updates();
    assert_eq!(messages.len(), 19);
    assert_rejected_messages(
        messages,
        AgentServiceTransportError::UnsupportedInteractionUpdate,
    )
    .await;
}

#[tokio::test]
async fn rejects_every_pinned_unsupported_internal_action() {
    let messages = unsupported_exec_server_messages();
    assert_eq!(messages.len(), 38);
    assert_rejected_messages(
        messages,
        AgentServiceTransportError::UnsupportedExecServerMessage,
    )
    .await;
}

async fn assert_rejected_messages(
    messages: Vec<AgentServerMessage>,
    expected_error: AgentServiceTransportError,
) {
    let mut peer = FakePeer::spawn().await;
    let mut transport = AgentServiceTransport::connect(peer.endpoint())
        .await
        .unwrap();

    for message in messages {
        let mut run = start_run(&mut transport, minimal_run_request()).await.unwrap();
        let mut server_run = peer.next_run().await;
        let _initial_request = server_run.next_client_message().await;
        server_run.send(message).await;

        assert_eq!(
            run.next_server_message().await.unwrap_err(),
            expected_error.clone()
        );
        assert_eq!(
            run.next_server_message().await.unwrap_err(),
            AgentServiceTransportError::RunClosed
        );

        drop(run);
        server_run.close();
    }

    peer.shutdown().await;
}

fn minimal_run_request() -> AgentRunRequest {
    AgentRunRequest {
        conversation_id: Some("cooldex-run-1".to_string()),
        ..Default::default()
    }
}

fn text_delta(text: &str) -> AgentServerMessage {
    AgentServerMessage {
        message: Some(agent_server_message::Message::InteractionUpdate(
            InteractionUpdate {
                message: Some(interaction_update::Message::TextDelta(TextDeltaUpdate {
                    text: text.to_string(),
                })),
            },
        )),
    }
}

fn turn_ended() -> AgentServerMessage {
    AgentServerMessage {
        message: Some(agent_server_message::Message::InteractionUpdate(
            InteractionUpdate {
                message: Some(interaction_update::Message::TurnEnded(TurnEndedUpdate {
                    input_tokens: Some(11),
                    output_tokens: Some(7),
                    cache_read_tokens: Some(3),
                    cache_write_tokens: Some(0),
                    reasoning_tokens: Some(2),
                })),
            },
        )),
    }
}

fn unsupported_server_envelopes() -> Vec<AgentServerMessage> {
    use agent_server_message::Message;

    vec![
        Message::ConversationCheckpointUpdate(UnsupportedAgentServerMessage {}),
        Message::KvServerMessage(UnsupportedAgentServerMessage {}),
        Message::ExecServerControlMessage(UnsupportedAgentServerMessage {}),
        Message::InteractionQuery(UnsupportedAgentServerMessage {}),
    ]
    .into_iter()
    .map(server_message)
    .collect()
}

fn unsupported_interaction_updates() -> Vec<AgentServerMessage> {
    use interaction_update::Message;

    vec![
        Message::ToolCallStarted(UnsupportedInteractionUpdate {}),
        Message::ToolCallCompleted(UnsupportedInteractionUpdate {}),
        Message::ThinkingDelta(UnsupportedInteractionUpdate {}),
        Message::ThinkingCompleted(UnsupportedInteractionUpdate {}),
        Message::UserMessageAppended(UnsupportedInteractionUpdate {}),
        Message::PartialToolCall(UnsupportedInteractionUpdate {}),
        Message::TokenDelta(UnsupportedInteractionUpdate {}),
        Message::Summary(UnsupportedInteractionUpdate {}),
        Message::SummaryStarted(UnsupportedInteractionUpdate {}),
        Message::SummaryCompleted(UnsupportedInteractionUpdate {}),
        Message::ShellOutputDelta(UnsupportedInteractionUpdate {}),
        Message::ToolCallDelta(UnsupportedInteractionUpdate {}),
        Message::StepStarted(UnsupportedInteractionUpdate {}),
        Message::StepCompleted(UnsupportedInteractionUpdate {}),
        Message::PromptSuggestion(UnsupportedInteractionUpdate {}),
        Message::PostRequestPrompt(UnsupportedInteractionUpdate {}),
        Message::ActiveBranchChange(UnsupportedInteractionUpdate {}),
        Message::FeedbackRequest(UnsupportedInteractionUpdate {}),
        Message::ResponseComparison(UnsupportedInteractionUpdate {}),
    ]
    .into_iter()
    .map(interaction_message)
    .collect()
}

fn unsupported_exec_server_messages() -> Vec<AgentServerMessage> {
    use exec_server_message::Message;

    vec![
        Message::ShellArgs(UnsupportedExecServerMessage {}),
        Message::WriteArgs(UnsupportedExecServerMessage {}),
        Message::DeleteArgs(UnsupportedExecServerMessage {}),
        Message::GrepArgs(UnsupportedExecServerMessage {}),
        Message::ReadArgs(UnsupportedExecServerMessage {}),
        Message::LsArgs(UnsupportedExecServerMessage {}),
        Message::DiagnosticsArgs(UnsupportedExecServerMessage {}),
        Message::ShellStreamArgs(UnsupportedExecServerMessage {}),
        Message::BackgroundShellSpawnArgs(UnsupportedExecServerMessage {}),
        Message::ListMcpResourcesExecArgs(UnsupportedExecServerMessage {}),
        Message::ReadMcpResourceExecArgs(UnsupportedExecServerMessage {}),
        Message::FetchArgs(UnsupportedExecServerMessage {}),
        Message::RecordScreenArgs(UnsupportedExecServerMessage {}),
        Message::ComputerUseArgs(UnsupportedExecServerMessage {}),
        Message::WriteShellStdinArgs(UnsupportedExecServerMessage {}),
        Message::ExecuteHookArgs(UnsupportedExecServerMessage {}),
        Message::SubagentArgs(UnsupportedExecServerMessage {}),
        Message::RedactedReadArgs(UnsupportedExecServerMessage {}),
        Message::ForceBackgroundShellArgs(UnsupportedExecServerMessage {}),
        Message::ForceBackgroundSubagentArgs(UnsupportedExecServerMessage {}),
        Message::McpStateExecArgs(UnsupportedExecServerMessage {}),
        Message::SubagentAwaitArgs(UnsupportedExecServerMessage {}),
        Message::SmartModeClassifierArgs(UnsupportedExecServerMessage {}),
        Message::CanvasDiagnosticsArgs(UnsupportedExecServerMessage {}),
        Message::ShellAllowlistPrecheckArgs(UnsupportedExecServerMessage {}),
        Message::McpAllowlistPrecheckArgs(UnsupportedExecServerMessage {}),
        Message::WebFetchAllowlistPrecheckArgs(UnsupportedExecServerMessage {}),
        Message::GitDiffRequest(UnsupportedExecServerMessage {}),
        Message::PiReadArgs(UnsupportedExecServerMessage {}),
        Message::PiBashArgs(UnsupportedExecServerMessage {}),
        Message::PiEditArgs(UnsupportedExecServerMessage {}),
        Message::PiWriteArgs(UnsupportedExecServerMessage {}),
        Message::PiGrepArgs(UnsupportedExecServerMessage {}),
        Message::PiFindArgs(UnsupportedExecServerMessage {}),
        Message::PiLsArgs(UnsupportedExecServerMessage {}),
        Message::MiniSweAgentBashArgs(UnsupportedExecServerMessage {}),
        Message::ConversationSearchArgs(UnsupportedExecServerMessage {}),
        Message::AgentStoreConflictArgs(UnsupportedExecServerMessage {}),
    ]
    .into_iter()
    .map(exec_server_message)
    .collect()
}

fn server_message(message: agent_server_message::Message) -> AgentServerMessage {
    AgentServerMessage {
        message: Some(message),
    }
}

fn interaction_message(message: interaction_update::Message) -> AgentServerMessage {
    server_message(agent_server_message::Message::InteractionUpdate(
        InteractionUpdate {
            message: Some(message),
        },
    ))
}

fn exec_server_message(message: exec_server_message::Message) -> AgentServerMessage {
    server_message(agent_server_message::Message::ExecServerMessage(
        ExecServerMessage {
            id: 1,
            exec_id: "cursor-exec-1".to_string(),
            message: Some(message),
        },
    ))
}

fn mcp_server_message(id: u32, exec_id: &str, args: McpArgs) -> AgentServerMessage {
    server_message(agent_server_message::Message::ExecServerMessage(
        ExecServerMessage {
            id,
            exec_id: exec_id.to_string(),
            message: Some(exec_server_message::Message::McpArgs(args)),
        },
    ))
}

fn user_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

fn function_spec(name: &str) -> ToolSpec {
    ToolSpec::Function(ResponsesApiTool {
        name: name.to_string(),
        description: "A test function".to_string(),
        strict: true,
        defer_loading: None,
        parameters: JsonSchema::object(
            BTreeMap::new(),
            Some(Vec::new()),
            Some(AdditionalProperties::Boolean(false)),
        ),
        output_schema: None,
    })
}
