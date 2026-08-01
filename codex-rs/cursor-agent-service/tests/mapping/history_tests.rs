use super::support::apply_patch_call;
use super::support::apply_patch_output;
use super::support::apply_patch_spec;
use super::support::assistant_message;
use super::support::function_call;
use super::support::function_output;
use super::support::function_spec;
use super::support::user_message;
use codex_cursor_agent_service::CursorMappingError;
use codex_cursor_agent_service::CursorSamplingRequest;
use codex_cursor_agent_service::map_sampling_request;
use codex_cursor_agent_service::proto::ConversationStep;
use codex_cursor_agent_service::proto::ConversationTurnStructure;
use codex_cursor_agent_service::proto::UserMessage;
use codex_cursor_agent_service::proto::conversation_action;
use codex_cursor_agent_service::proto::conversation_step;
use codex_cursor_agent_service::proto::conversation_turn_structure;
use codex_cursor_agent_service::proto::mcp_tool_result;
use codex_cursor_agent_service::proto::tool_call;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use pretty_assertions::assert_eq;
use prost::Message as _;

const DECLARED_HISTORY_REJECTIONS: &[&str] = &[
    "assistant_before_user",
    "custom_namespace",
    "duplicate_call_id",
    "duplicate_output",
    "empty_synthesized_message_id",
    "incomplete_pair",
    "missing_current_user",
    "non_object_arguments",
    "orphan_call",
    "orphan_output",
    "output_before_call",
    "output_kind_mismatch",
    "output_name",
    "unsupported_item",
    "unsupported_role",
    "unsupported_user_content",
    "wrong_custom_name",
];

#[test]
fn maps_typed_function_and_apply_patch_history_without_rewriting_call_ids() {
    let patch = "*** Begin Patch\n*** Add File: demo.txt\n+hello\n*** End Patch";
    let input = vec![
        user_message("first request"),
        assistant_message("working"),
        function_call("echo", None, "cool-call-1"),
        function_output("cool-call-1", r#"{"value":"done"}"#, Some(false)),
        apply_patch_call("cool-call-2", patch),
        apply_patch_output("cool-call-2", None, "Done!"),
        user_message("current request"),
    ];
    let tools = vec![function_spec("echo"), apply_patch_spec()];
    let mapped = map_sampling_request(CursorSamplingRequest {
        conversation_id: "conversation-local",
        model_id: "composer-2.5",
        model_display_name: "Composer 2.5",
        base_instructions: "instructions",
        input: &input,
        tools: &tools,
        current_message_id: "current-message",
        synthesized_user_message: None,
    })
    .expect("representable history should map");

    let state = mapped
        .request
        .conversation_state
        .expect("conversation state should be present");
    assert_eq!(state.turns.len(), 1);
    let turn = ConversationTurnStructure::decode(state.turns[0].as_slice())
        .expect("typed historical turn should decode");
    let Some(conversation_turn_structure::Turn::AgentConversationTurn(turn)) = turn.turn else {
        panic!("expected agent conversation turn");
    };
    assert_eq!(
        UserMessage::decode(turn.user_message.as_slice())
            .expect("historical user message should decode")
            .text,
        "first request"
    );
    assert_eq!(turn.request_id, None);
    assert_eq!(turn.encrypted_model, None);
    assert_eq!(turn.dynamic_tool_count, None);
    assert_eq!(turn.steps.len(), 3);

    let assistant = decode_step(&turn.steps[0]);
    let Some(conversation_step::Message::AssistantMessage(assistant)) = assistant.message else {
        panic!("expected assistant step");
    };
    assert_eq!(assistant.text, "working");

    let function = decode_tool_step(&turn.steps[1]);
    assert_eq!(function.tool_call_id.as_deref(), Some("cool-call-1"));
    let Some(tool_call::Tool::McpToolCall(function)) = function.tool else {
        panic!("expected function MCP step");
    };
    let function_args = function.args.expect("function args should be present");
    assert_eq!(function_args.tool_call_id, "cool-call-1");
    assert_eq!(function_args.name, "echo");
    assert_eq!(function_args.provider_identifier, "cooldex");
    assert_eq!(function_args.tool_name, "echo");
    assert_eq!(function_args.server_identifier, "cooldex");
    assert_eq!(function_args.smart_mode_approval, None);
    assert!(!function_args.smart_mode_approval_only);
    assert!(!function_args.skip_approval);
    let function_result = function.result.expect("function result should be present");
    let Some(mcp_tool_result::Result::Success(success)) = function_result.result else {
        panic!("expected successful MCP result envelope");
    };
    assert!(success.is_error);
    assert_eq!(
        success
            .structured_content
            .expect("JSON object output should retain structured content")
            .fields["value"]
            .kind
            .as_ref()
            .and_then(|kind| match kind {
                prost_types::value::Kind::StringValue(value) => Some(value.as_str()),
                _ => None,
            }),
        Some("done")
    );

    let patch_step = decode_tool_step(&turn.steps[2]);
    assert_eq!(patch_step.tool_call_id.as_deref(), Some("cool-call-2"));
    let Some(tool_call::Tool::McpToolCall(patch_call)) = patch_step.tool else {
        panic!("expected apply_patch MCP step");
    };
    let patch_args = patch_call.args.expect("apply_patch args should be present");
    assert_eq!(patch_args.tool_call_id, "cool-call-2");
    assert_eq!(patch_args.name, "apply_patch");
    assert_eq!(patch_args.provider_identifier, "cooldex");
    assert_eq!(patch_args.tool_name, "apply_patch");
    assert_eq!(patch_args.smart_mode_approval, None);
    assert_eq!(
        patch_args.args["patch"]
            .kind
            .as_ref()
            .and_then(|kind| match kind {
                prost_types::value::Kind::StringValue(value) => Some(value.as_str()),
                _ => None,
            }),
        Some(patch)
    );

    let action = mapped
        .request
        .action
        .and_then(|action| action.action)
        .expect("current action should be present");
    let conversation_action::Action::UserMessageAction(action) = action;
    assert_eq!(
        action
            .user_message
            .expect("current user should be present")
            .text,
        "current request"
    );
}

#[test]
fn accepts_a_synthesized_current_user_without_adding_it_to_history() {
    let input = vec![user_message("old"), assistant_message("answer")];
    let mapped = map_sampling_request(CursorSamplingRequest {
        conversation_id: "conversation-local",
        model_id: "composer-2.5",
        model_display_name: "Composer 2.5",
        base_instructions: "instructions",
        input: &input,
        tools: &[],
        current_message_id: "synthesized-id",
        synthesized_user_message: Some("new user input"),
    })
    .expect("synthesized current user should map");

    assert_eq!(
        mapped
            .request
            .conversation_state
            .expect("state should be present")
            .turns
            .len(),
        1
    );
    let action = mapped
        .request
        .action
        .and_then(|action| action.action)
        .expect("current action should be present");
    let conversation_action::Action::UserMessageAction(action) = action;
    let current = action
        .user_message
        .expect("synthesized user should be present");
    assert_eq!(current.text, "new user input");
    assert_eq!(current.message_id, "synthesized-id");
}

#[test]
fn rejects_every_declared_history_cell_in_isolation() {
    let mut tested = Vec::new();
    for case in DECLARED_HISTORY_REJECTIONS {
        let (input, tools, synthesized, current_id) = history_rejection_case(case);
        let error = map_sampling_request(CursorSamplingRequest {
            conversation_id: "conversation-local",
            model_id: "composer-2.5",
            model_display_name: "Composer 2.5",
            base_instructions: "instructions",
            input: &input,
            tools: &tools,
            current_message_id: current_id,
            synthesized_user_message: synthesized,
        })
        .expect_err("declared unsupported history should reject");

        assert_history_error(case, &error);
        tested.push(*case);
    }

    assert_eq!(tested, DECLARED_HISTORY_REJECTIONS);
}

fn history_rejection_case(
    case: &str,
) -> (
    Vec<ResponseItem>,
    Vec<codex_tools::ToolSpec>,
    Option<&'static str>,
    &'static str,
) {
    let tools = vec![function_spec("echo"), apply_patch_spec()];
    let current = user_message("current");
    match case {
        "assistant_before_user" => (
            vec![assistant_message("orphan"), current],
            tools,
            None,
            "current",
        ),
        "custom_namespace" => {
            let mut call = apply_patch_call("patch-1", "patch");
            let ResponseItem::CustomToolCall { namespace, .. } = &mut call else {
                unreachable!();
            };
            *namespace = Some("other".to_string());
            (
                vec![user_message("old"), call, current],
                tools,
                None,
                "current",
            )
        }
        "duplicate_call_id" => (
            vec![
                user_message("old"),
                function_call("echo", None, "dup"),
                function_output("dup", "one", Some(true)),
                apply_patch_call("dup", "patch"),
                current,
            ],
            tools,
            None,
            "current",
        ),
        "duplicate_output" => (
            vec![
                user_message("old"),
                function_call("echo", None, "call-1"),
                function_output("call-1", "one", Some(true)),
                function_output("call-1", "two", Some(true)),
                current,
            ],
            tools,
            None,
            "current",
        ),
        "empty_synthesized_message_id" => (
            vec![user_message("old"), assistant_message("answer")],
            tools,
            Some("new"),
            "",
        ),
        "incomplete_pair" => (
            vec![
                user_message("old"),
                function_call("echo", None, "call-1"),
                current,
            ],
            tools,
            None,
            "current",
        ),
        "missing_current_user" => (
            vec![user_message("old"), assistant_message("answer")],
            tools,
            None,
            "current",
        ),
        "non_object_arguments" => {
            let mut call = function_call("echo", None, "call-1");
            let ResponseItem::FunctionCall { arguments, .. } = &mut call else {
                unreachable!();
            };
            *arguments = "[]".to_string();
            (
                vec![user_message("old"), call, current],
                tools,
                None,
                "current",
            )
        }
        "orphan_call" => (
            vec![
                user_message("old"),
                function_call("missing", None, "call-1"),
                current,
            ],
            tools,
            None,
            "current",
        ),
        "orphan_output" => (
            vec![function_output("missing", "output", Some(true)), current],
            tools,
            None,
            "current",
        ),
        "output_before_call" => (
            vec![
                user_message("old"),
                function_output("later", "output", Some(true)),
                current,
            ],
            tools,
            None,
            "current",
        ),
        "output_kind_mismatch" => (
            vec![
                user_message("old"),
                function_call("echo", None, "call-1"),
                apply_patch_output("call-1", None, "output"),
                current,
            ],
            tools,
            None,
            "current",
        ),
        "output_name" => (
            vec![
                user_message("old"),
                apply_patch_call("patch-1", "patch"),
                apply_patch_output("patch-1", Some("apply_patch"), "done"),
                current,
            ],
            tools,
            None,
            "current",
        ),
        "unsupported_item" => (
            vec![user_message("old"), ResponseItem::Other, current],
            tools,
            None,
            "current",
        ),
        "unsupported_role" => {
            let mut message = user_message("system text");
            let ResponseItem::Message { role, .. } = &mut message else {
                unreachable!();
            };
            *role = "system".to_string();
            (vec![message, current], tools, None, "current")
        }
        "unsupported_user_content" => {
            let message = ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputImage {
                    image_url: "data:image/png;base64,AA==".to_string(),
                    detail: None,
                }],
                phase: None,
                internal_chat_message_metadata_passthrough: None,
            };
            (vec![message, current], tools, None, "current")
        }
        "wrong_custom_name" => {
            let mut call = apply_patch_call("patch-1", "patch");
            let ResponseItem::CustomToolCall { name, .. } = &mut call else {
                unreachable!();
            };
            *name = "exec".to_string();
            (
                vec![user_message("old"), call, current],
                tools,
                None,
                "current",
            )
        }
        unknown => panic!("undeclared history rejection case: {unknown}"),
    }
}

fn assert_history_error(case: &str, error: &CursorMappingError) {
    match case {
        "assistant_before_user" => assert!(matches!(
            error,
            CursorMappingError::HistoryItemBeforeUser(_)
        )),
        "custom_namespace" | "output_name" | "wrong_custom_name" => {
            assert!(matches!(
                error,
                CursorMappingError::NonCanonicalCustomTool(_)
            ))
        }
        "duplicate_call_id" => assert!(matches!(
            error,
            CursorMappingError::DuplicateHistoricalCallId(_)
        )),
        "duplicate_output" => assert!(matches!(
            error,
            CursorMappingError::DuplicateHistoricalOutput(_)
        )),
        "empty_synthesized_message_id" => {
            assert_eq!(error, &CursorMappingError::EmptyCurrentMessageId)
        }
        "incomplete_pair" => assert!(matches!(
            error,
            CursorMappingError::IncompleteHistoricalCall(_)
        )),
        "missing_current_user" => assert_eq!(error, &CursorMappingError::MissingCurrentUserMessage),
        "non_object_arguments" => assert!(matches!(
            error,
            CursorMappingError::InvalidToolArguments { .. }
        )),
        "orphan_call" => assert!(matches!(error, CursorMappingError::UnsupportedTool(_))),
        "orphan_output" | "output_before_call" => {
            assert!(matches!(
                error,
                CursorMappingError::HistoricalOutputBeforeCall(_)
            ))
        }
        "output_kind_mismatch" => assert!(matches!(
            error,
            CursorMappingError::HistoricalOutputKindMismatch(_)
        )),
        "unsupported_item" => assert!(matches!(
            error,
            CursorMappingError::UnsupportedHistoryItem(_)
        )),
        "unsupported_role" => assert!(matches!(
            error,
            CursorMappingError::UnsupportedMessageRole(_)
        )),
        "unsupported_user_content" => assert!(matches!(
            error,
            CursorMappingError::UnsupportedMessageContent(_)
        )),
        unknown => panic!("undeclared history rejection assertion: {unknown}"),
    }
}

fn decode_step(bytes: &[u8]) -> ConversationStep {
    ConversationStep::decode(bytes).expect("historical step should decode")
}

fn decode_tool_step(bytes: &[u8]) -> codex_cursor_agent_service::proto::ToolCall {
    let step = decode_step(bytes);
    let Some(conversation_step::Message::ToolCall(tool_call)) = step.message else {
        panic!("expected tool-call step");
    };
    tool_call
}
