#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "support/fake_peer.rs"]
mod fake_peer;

use codex_cursor_agent_service::AgentServiceTransport;
use codex_cursor_agent_service::AgentServiceTransportError;
use codex_cursor_agent_service::proto::AGENT_SERVICE_TYPE_NAME;
use codex_cursor_agent_service::proto::AgentClientMessage;
use codex_cursor_agent_service::proto::AgentRunRequest;
use codex_cursor_agent_service::proto::AgentServerMessage;
use codex_cursor_agent_service::proto::ClientHeartbeat;
use codex_cursor_agent_service::proto::ExecServerMessage;
use codex_cursor_agent_service::proto::HEARTBEAT_INTERVAL_SECONDS;
use codex_cursor_agent_service::proto::InteractionUpdate;
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
use fake_peer::FakePeer;
use pretty_assertions::assert_eq;
use prost::Message as _;
use std::time::Duration;

const EXPECTED_CURSOR_CLI_SHA256: &str =
    "eed61c5224668c9236334c4c68936a16aecc37374b592f59e31eb50433817831";
const EXPECTED_SCHEMA_SHA256: &str =
    "9117b3cfd5c5f903ec42e74c81312d9392dbb09b6fedc4be429f6b18437a931d";

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
    let mut run = transport.start_run(request.clone()).await.unwrap();
    let mut server_run = peer.next_run().await;

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
async fn sends_a_client_heartbeat_while_the_run_is_open() {
    let mut peer = FakePeer::spawn().await;
    let mut transport = AgentServiceTransport::connect(peer.endpoint())
        .await
        .unwrap();
    let run = transport.start_run(minimal_run_request()).await.unwrap();
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
    let mut run = transport.start_run(minimal_run_request()).await.unwrap();
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
    let mut run = transport.start_run(minimal_run_request()).await.unwrap();
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
        let mut run = transport.start_run(minimal_run_request()).await.unwrap();
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
