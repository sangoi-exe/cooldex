#![cfg(not(target_os = "windows"))]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use anyhow::Result;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;

fn contains_manage_context_chatter(input: &[Value], call_id: &str) -> bool {
    input
        .iter()
        .any(|item| match item.get("type").and_then(Value::as_str) {
            Some("function_call") => {
                item.get("name").and_then(Value::as_str) == Some("manage_context")
            }
            Some("function_call_output") => {
                item.get("call_id").and_then(Value::as_str) == Some(call_id)
            }
            Some("custom_tool_call") => {
                item.get("name").and_then(Value::as_str) == Some("manage_context")
            }
            Some("custom_tool_call_output") => {
                item.get("call_id").and_then(Value::as_str) == Some(call_id)
            }
            _ => false,
        })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cleanup_only_sanitize_reports_no_changes_and_keeps_followup_request_free_of_sanitize_chatter()
-> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let retrieve_call_id = "sanitize-retrieve";
    let request_log = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-1"),
                ev_function_call(
                    retrieve_call_id,
                    "manage_context",
                    &json!({
                        "mode": "retrieve",
                        "policy_id": "sanitize_prompt"
                    })
                    .to_string(),
                ),
                ev_completed("resp-1"),
            ]),
            sse(vec![
                ev_response_created("resp-2"),
                ev_assistant_message("msg-1", "/sanitize completed and applied context updates."),
                ev_completed("resp-2"),
            ]),
            sse(vec![
                ev_response_created("resp-3"),
                ev_assistant_message("msg-2", "follow-up complete"),
                ev_completed("resp-3"),
            ]),
        ],
    )
    .await;
    let test = test_codex().build(&server).await?;

    test.codex.submit(Op::Sanitize).await?;
    let sanitize_turn_complete = wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    let EventMsg::TurnComplete(turn_complete) = sanitize_turn_complete else {
        panic!("expected TurnComplete event");
    };
    assert_eq!(
        turn_complete.last_agent_message.as_deref(),
        Some("/sanitize completed with no context changes.")
    );

    test.codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "after sanitize".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    let requests = request_log.requests();
    assert_eq!(requests.len(), 3);

    let sanitize_follow_up_input = requests[1].input();
    assert!(
        sanitize_follow_up_input.iter().any(|item| {
            item.get("type").and_then(Value::as_str) == Some("function_call_output")
                && item.get("call_id").and_then(Value::as_str) == Some(retrieve_call_id)
        }),
        "sanitize follow-up request should include the retrieve tool output"
    );

    let follow_up_input = requests[2].input();
    assert!(
        !contains_manage_context_chatter(&follow_up_input, retrieve_call_id),
        "post-sanitize follow-up request should not contain completed manage_context chatter: {follow_up_input:#?}"
    );
    assert!(
        !requests[2].body_contains_text("/sanitize completed and applied context updates."),
        "post-sanitize follow-up request should not contain misleading sanitize assistant text: {follow_up_input:#?}"
    );
    assert!(
        requests[2]
            .message_input_texts("user")
            .iter()
            .any(|text| text == "after sanitize"),
        "follow-up user text missing from post-sanitize request"
    );

    Ok(())
}
