use super::support::apply_patch_spec;
use super::support::function_spec;
use super::support::output;
use super::support::prost_string;
use super::support::valid_mcp_args;
use codex_cursor_agent_service::CursorMappingError;
use codex_cursor_agent_service::CursorToolCallTracker;
use codex_cursor_agent_service::CursorToolSnapshot;
use codex_cursor_agent_service::proto::SmartModeApproval;
use codex_cursor_agent_service::proto::exec_client_message;
use codex_cursor_agent_service::proto::mcp_result;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseItem;
use pretty_assertions::assert_eq;

const DECLARED_LIVE_REJECTIONS: &[&str] = &[
    "apply_patch_extra_field",
    "apply_patch_missing_field",
    "apply_patch_nonstring_field",
    "duplicate_action_id",
    "duplicate_cooldex_id",
    "duplicate_result",
    "empty_action_id",
    "malformed_value",
    "pending_ninth",
    "skip_approval",
    "smart_mode_approval",
    "smart_mode_approval_only",
    "terminal_with_pending",
    "unknown_result",
    "unsupported_output",
    "wrong_provider",
    "wrong_server",
    "wrong_tool_name",
    "wrong_wire_name",
];

#[test]
fn maps_live_function_and_apply_patch_calls_to_canonical_cooldex_items() {
    let snapshot = live_snapshot();
    let mut tracker = CursorToolCallTracker::new(snapshot.clone(), 8);

    let mut function_args = valid_mcp_args(&snapshot, 0, "cursor-action-1");
    function_args
        .args
        .insert("value".to_string(), prost_string("hello"));
    let function = tracker
        .accept_mcp_call(
            11,
            "exec-11".to_string(),
            function_args,
            "cool-call-1".to_string(),
        )
        .expect("valid function call should map");
    assert_eq!(function.cursor_action_id, "cursor-action-1");
    assert_eq!(function.cooldex_call_id, "cool-call-1");
    assert_eq!(function.tool_name.name, "echo");
    assert_eq!(function.tool_name.namespace, None);
    let ResponseItem::FunctionCall {
        name,
        namespace,
        arguments,
        call_id,
        ..
    } = function.response_item
    else {
        panic!("expected Cooldex function call");
    };
    assert_eq!(name, "echo");
    assert_eq!(namespace, None);
    assert_eq!(arguments, r#"{"value":"hello"}"#);
    assert_eq!(call_id, "cool-call-1");

    let mut patch_args = valid_mcp_args(&snapshot, 1, "cursor-action-2");
    let patch = "*** Begin Patch\n*** End Patch";
    patch_args
        .args
        .insert("patch".to_string(), prost_string(patch));
    let patch_call = tracker
        .accept_mcp_call(
            12,
            "exec-12".to_string(),
            patch_args,
            "cool-call-2".to_string(),
        )
        .expect("valid apply_patch call should map");
    let ResponseItem::CustomToolCall {
        name,
        namespace,
        input,
        call_id,
        ..
    } = patch_call.response_item
    else {
        panic!("expected canonical Cooldex custom tool call");
    };
    assert_eq!(name, "apply_patch");
    assert_eq!(namespace, None);
    assert_eq!(input, patch);
    assert_eq!(call_id, "cool-call-2");
    assert_eq!(tracker.pending_count(), 2);
}

#[test]
fn maps_text_structured_and_error_results_on_the_same_cursor_action_id() {
    let snapshot = live_snapshot();
    let mut tracker = CursorToolCallTracker::new(snapshot.clone(), 8);
    tracker
        .accept_mcp_call(
            21,
            "exec-21".to_string(),
            valid_mcp_args(&snapshot, 0, "cursor-action-1"),
            "cool-call-1".to_string(),
        )
        .expect("valid function call should map");

    let completed = tracker
        .complete_mcp_call("cursor-action-1", &output(r#"{"answer":42}"#, Some(false)))
        .expect("valid result should map");
    assert_eq!(completed.cooldex_call_id, "cool-call-1");
    assert_eq!(completed.exec_client_message.id, 21);
    assert_eq!(completed.exec_client_message.exec_id, "exec-21");
    let Some(exec_client_message::Message::McpResult(result)) =
        completed.exec_client_message.message
    else {
        panic!("expected MCP result");
    };
    let Some(mcp_result::Result::Success(success)) = result.result else {
        panic!("expected MCP success envelope");
    };
    assert!(success.is_error);
    assert_eq!(success.content.len(), 1);
    assert_eq!(
        success
            .structured_content
            .expect("JSON object output should retain structured content")
            .fields["answer"]
            .kind
            .as_ref()
            .and_then(|kind| match kind {
                prost_types::value::Kind::NumberValue(value) => Some(*value),
                _ => None,
            }),
        Some(42.0)
    );
    assert_eq!(tracker.pending_count(), 0);
    tracker
        .require_no_pending()
        .expect("completed call should clear pending state");
}

#[test]
fn rejected_live_call_does_not_poison_action_or_cooldex_ids() {
    let snapshot = live_snapshot();
    let mut tracker = CursorToolCallTracker::new(snapshot.clone(), 8);
    let mut rejected = valid_mcp_args(&snapshot, 0, "cursor-action-1");
    rejected.provider_identifier = "wrong".to_string();
    assert_eq!(
        tracker
            .accept_mcp_call(1, "exec".to_string(), rejected, "cool-call-1".to_string(),)
            .expect_err("identity mismatch should reject"),
        CursorMappingError::InvalidToolIdentity
    );

    tracker
        .accept_mcp_call(
            1,
            "exec".to_string(),
            valid_mcp_args(&snapshot, 0, "cursor-action-1"),
            "cool-call-1".to_string(),
        )
        .expect("rejected action should not reserve either id");
}

#[test]
fn rejects_every_declared_live_call_or_result_cell_in_isolation() {
    let mut tested = Vec::new();
    for case in DECLARED_LIVE_REJECTIONS {
        let error = run_live_rejection_case(case);
        assert_live_error(case, &error);
        tested.push(*case);
    }

    assert_eq!(tested, DECLARED_LIVE_REJECTIONS);
}

fn run_live_rejection_case(case: &str) -> CursorMappingError {
    let snapshot = live_snapshot();
    let mut tracker = CursorToolCallTracker::new(snapshot.clone(), 8);
    let mut args = valid_mcp_args(&snapshot, 0, "cursor-action-1");
    match case {
        "apply_patch_extra_field" => {
            args = valid_mcp_args(&snapshot, 1, "cursor-action-1");
            args.args.insert("patch".to_string(), prost_string("patch"));
            args.args.insert("extra".to_string(), prost_string("no"));
        }
        "apply_patch_missing_field" => {
            args = valid_mcp_args(&snapshot, 1, "cursor-action-1");
        }
        "apply_patch_nonstring_field" => {
            args = valid_mcp_args(&snapshot, 1, "cursor-action-1");
            args.args.insert(
                "patch".to_string(),
                prost_types::Value {
                    kind: Some(prost_types::value::Kind::BoolValue(true)),
                },
            );
        }
        "duplicate_action_id" => {
            tracker
                .accept_mcp_call(
                    1,
                    "exec-1".to_string(),
                    args.clone(),
                    "cool-call-1".to_string(),
                )
                .expect("seed action should map");
            return tracker
                .accept_mcp_call(2, "exec-2".to_string(), args, "cool-call-2".to_string())
                .expect_err("duplicate action id should reject");
        }
        "duplicate_cooldex_id" => {
            tracker
                .accept_mcp_call(1, "exec-1".to_string(), args, "cool-call-1".to_string())
                .expect("seed action should map");
            return tracker
                .accept_mcp_call(
                    2,
                    "exec-2".to_string(),
                    valid_mcp_args(&snapshot, 0, "cursor-action-2"),
                    "cool-call-1".to_string(),
                )
                .expect_err("duplicate Cooldex id should reject");
        }
        "duplicate_result" => {
            tracker
                .accept_mcp_call(1, "exec".to_string(), args, "cool-call-1".to_string())
                .expect("seed action should map");
            tracker
                .complete_mcp_call("cursor-action-1", &output("done", Some(true)))
                .expect("first result should map");
            return tracker
                .complete_mcp_call("cursor-action-1", &output("again", Some(true)))
                .expect_err("second result should reject");
        }
        "empty_action_id" => args.tool_call_id.clear(),
        "malformed_value" => {
            args.args
                .insert("value".to_string(), prost_types::Value { kind: None });
        }
        "pending_ninth" => {
            for index in 0..8 {
                tracker
                    .accept_mcp_call(
                        index,
                        format!("exec-{index}"),
                        valid_mcp_args(&snapshot, 0, &format!("cursor-{index}")),
                        format!("cool-{index}"),
                    )
                    .expect("first eight pending actions should map");
            }
            return tracker
                .accept_mcp_call(
                    9,
                    "exec-9".to_string(),
                    valid_mcp_args(&snapshot, 0, "cursor-9"),
                    "cool-9".to_string(),
                )
                .expect_err("ninth pending action should reject");
        }
        "skip_approval" => args.skip_approval = true,
        "smart_mode_approval" => {
            args.smart_mode_approval = Some(SmartModeApproval {
                request_id: "approval-request".to_string(),
                reason: "bypass Cooldex".to_string(),
            });
        }
        "smart_mode_approval_only" => args.smart_mode_approval_only = true,
        "terminal_with_pending" => {
            tracker
                .accept_mcp_call(1, "exec".to_string(), args, "cool-call-1".to_string())
                .expect("seed action should map");
            return tracker
                .require_no_pending()
                .expect_err("terminal with pending action should reject");
        }
        "unknown_result" => {
            return tracker
                .complete_mcp_call("missing", &output("done", Some(true)))
                .expect_err("unknown result id should reject");
        }
        "unsupported_output" => {
            tracker
                .accept_mcp_call(1, "exec".to_string(), args, "cool-call-1".to_string())
                .expect("seed action should map");
            return tracker
                .complete_mcp_call(
                    "cursor-action-1",
                    &FunctionCallOutputPayload {
                        body: FunctionCallOutputBody::ContentItems(vec![
                            FunctionCallOutputContentItem::InputImage {
                                image_url: "data:image/png;base64,AA==".to_string(),
                                detail: None,
                            },
                        ]),
                        success: Some(true),
                    },
                )
                .expect_err("multimodal tool output should reject");
        }
        "wrong_provider" => args.provider_identifier = "wrong".to_string(),
        "wrong_server" => args.server_identifier = "cursor".to_string(),
        "wrong_tool_name" => args.tool_name = "other".to_string(),
        "wrong_wire_name" => args.name = "other".to_string(),
        unknown => panic!("undeclared live rejection case: {unknown}"),
    }

    tracker
        .accept_mcp_call(1, "exec".to_string(), args, "cool-call-1".to_string())
        .expect_err("declared live action should reject")
}

fn assert_live_error(case: &str, error: &CursorMappingError) {
    match case {
        "apply_patch_extra_field" | "apply_patch_missing_field" | "apply_patch_nonstring_field" => {
            assert_eq!(error, &CursorMappingError::InvalidApplyPatchArguments)
        }
        "duplicate_action_id" => assert!(matches!(error, CursorMappingError::DuplicateActionId(_))),
        "duplicate_cooldex_id" => assert!(matches!(
            error,
            CursorMappingError::DuplicateCooldexCallId(_)
        )),
        "duplicate_result" => assert!(matches!(error, CursorMappingError::DuplicateToolResult(_))),
        "empty_action_id" => assert_eq!(error, &CursorMappingError::EmptyActionId),
        "malformed_value" => assert!(matches!(
            error,
            CursorMappingError::InvalidToolArguments { .. }
        )),
        "pending_ninth" => assert_eq!(error, &CursorMappingError::PendingToolLimit(8)),
        "skip_approval" => assert_eq!(
            error,
            &CursorMappingError::ApprovalBypassRequested("skip_approval")
        ),
        "smart_mode_approval" => assert_eq!(error, &CursorMappingError::SmartModeApprovalRequested),
        "smart_mode_approval_only" => assert_eq!(
            error,
            &CursorMappingError::ApprovalBypassRequested("smart_mode_approval_only")
        ),
        "terminal_with_pending" => {
            assert_eq!(error, &CursorMappingError::PendingToolsAtTerminal(1))
        }
        "unknown_result" => assert!(matches!(error, CursorMappingError::UnknownActionId(_))),
        "unsupported_output" => assert_eq!(error, &CursorMappingError::UnsupportedToolOutput),
        "wrong_provider" | "wrong_tool_name" | "wrong_wire_name" => {
            assert_eq!(error, &CursorMappingError::InvalidToolIdentity)
        }
        "wrong_server" => assert_eq!(error, &CursorMappingError::InvalidServerIdentifier),
        unknown => panic!("undeclared live rejection assertion: {unknown}"),
    }
}

fn live_snapshot() -> CursorToolSnapshot {
    CursorToolSnapshot::from_specs(&[function_spec("echo"), apply_patch_spec()])
        .expect("live test snapshot should map")
}
