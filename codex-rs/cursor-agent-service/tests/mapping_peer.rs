#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "support/fake_peer.rs"]
mod fake_peer;
#[path = "support/credentials.rs"]
mod credentials;

use codex_cursor_agent_service::AgentServiceTransport;
use codex_cursor_agent_service::CursorMappingError;
use codex_cursor_agent_service::CursorSamplingRequest;
use codex_cursor_agent_service::CursorToolCallTracker;
use codex_cursor_agent_service::map_request_context_result;
use codex_cursor_agent_service::map_sampling_request;
use codex_cursor_agent_service::proto::AgentClientMessage;
use codex_cursor_agent_service::proto::AgentServerMessage;
use codex_cursor_agent_service::proto::ExecServerMessage;
use codex_cursor_agent_service::proto::McpArgs;
use codex_cursor_agent_service::proto::RequestContextArgs;
use codex_cursor_agent_service::proto::SmartModeApproval;
use codex_cursor_agent_service::proto::agent_client_message;
use codex_cursor_agent_service::proto::agent_server_message;
use codex_cursor_agent_service::proto::exec_server_message;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_tools::AdditionalProperties;
use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolSpec;
use fake_peer::FakePeer;
use credentials::start_run;
use pretty_assertions::assert_eq;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::time::Duration;

#[tokio::test]
async fn fake_peer_observes_exact_mapped_request_and_smart_approval_rejects_without_reply() {
    let input = vec![user_message("Use echo")];
    let tools = vec![function_spec("echo")];
    let mapped = map_sampling_request(CursorSamplingRequest {
        conversation_id: "conversation-local",
        model_id: "composer-2.5",
        model_display_name: "Composer 2.5",
        base_instructions: "exact Cooldex instructions",
        input: &input,
        tools: &tools,
        current_message_id: "current-message",
        synthesized_user_message: None,
    })
    .expect("mapped request should be valid");
    let expected_request = mapped.request.clone();
    let mut tracker = CursorToolCallTracker::new(mapped.tool_snapshot, 8);
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
    let mut run = start_run(&mut transport, expected_request.clone())
        .await
        .unwrap();
    let mut server_run = peer.next_run().await;
    assert_eq!(
        server_run.next_client_message().await,
        AgentClientMessage {
            message: Some(agent_client_message::Message::RunRequest(expected_request)),
        }
    );

    server_run
        .send(exec_server_message(McpArgs {
            name: definition.name,
            args: HashMap::new(),
            tool_call_id: "cursor-action-1".to_string(),
            provider_identifier: definition.provider_identifier,
            tool_name: definition.tool_name,
            smart_mode_approval: Some(SmartModeApproval {
                request_id: "approval-request".to_string(),
                reason: "bypass Cooldex".to_string(),
            }),
            smart_mode_approval_only: false,
            skip_approval: false,
            server_identifier: "cooldex".to_string(),
        }))
        .await;
    let message = run.next_server_message().await.unwrap();
    let Some(agent_server_message::Message::ExecServerMessage(exec)) = message.message else {
        panic!("expected exec server message");
    };
    let Some(exec_server_message::Message::McpArgs(args)) = exec.message else {
        panic!("expected MCP args");
    };
    assert_eq!(
        tracker
            .accept_mcp_call(exec.id, exec.exec_id, args, "cool-call-1".to_string(),)
            .expect_err("SmartModeApproval must reject before dispatch"),
        CursorMappingError::SmartModeApprovalRequested
    );
    assert_eq!(tracker.pending_count(), 0);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), server_run.next_client_message(),)
            .await
            .is_err(),
        "rejected approval request must not produce an exec reply"
    );

    drop(run);
    server_run.close();
    peer.shutdown().await;
}

#[tokio::test]
async fn request_context_query_is_answered_on_the_same_fake_peer_run() {
    let input = vec![user_message("Continue")];
    let mapped = map_sampling_request(CursorSamplingRequest {
        conversation_id: "conversation-local",
        model_id: "composer-2.5",
        model_display_name: "Composer 2.5",
        base_instructions: "exact Cooldex instructions",
        input: &input,
        tools: &[],
        current_message_id: "current-message",
        synthesized_user_message: None,
    })
    .expect("mapped request should be valid");

    let mut peer = FakePeer::spawn().await;
    let mut transport = AgentServiceTransport::connect(peer.endpoint())
        .await
        .unwrap();
    let mut run = start_run(&mut transport, mapped.request).await.unwrap();
    let mut server_run = peer.next_run().await;
    let _initial_request = server_run.next_client_message().await;
    let args = RequestContextArgs {
        notes_session_id: None,
        workspace_id: None,
        read_only_pinned_tree_sha: None,
        read_only_plugin_cache_root: None,
        use_cached: None,
    };
    server_run
        .send(server_message(
            exec_server_message::Message::RequestContextArgs(args.clone()),
        ))
        .await;

    let message = run.next_server_message().await.unwrap();
    let Some(agent_server_message::Message::ExecServerMessage(exec)) = message.message else {
        panic!("expected exec server message");
    };
    let reply =
        map_request_context_result(exec.id, exec.exec_id, &args, "exact Cooldex instructions")
            .expect("empty context query should map");
    run.send_exec_client_message(reply.clone()).await.unwrap();
    assert_eq!(
        server_run.next_client_message().await,
        AgentClientMessage {
            message: Some(agent_client_message::Message::ExecClientMessage(reply)),
        }
    );

    drop(run);
    server_run.close();
    peer.shutdown().await;
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

fn exec_server_message(args: McpArgs) -> AgentServerMessage {
    server_message(exec_server_message::Message::McpArgs(args))
}

fn server_message(message: exec_server_message::Message) -> AgentServerMessage {
    AgentServerMessage {
        message: Some(agent_server_message::Message::ExecServerMessage(
            ExecServerMessage {
                id: 17,
                exec_id: "cursor-exec-17".to_string(),
                message: Some(message),
            },
        )),
    }
}
