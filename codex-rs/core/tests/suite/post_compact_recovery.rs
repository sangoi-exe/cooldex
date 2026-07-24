use std::fs;
use std::path::Path;

use anyhow::Result;
use codex_core::compact::SUMMARIZATION_PROMPT;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::RolloutLine;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ResponsesRequest;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::responses::start_websocket_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;

const FIRST_USER: &str = "historical user request";
const FIRST_REPLY: &str = "historical assistant response";
const SUMMARY: &str = "bounded compact summary";
const LIVE_USER: &str = "continue only the live work";

fn user_turn(text: &str) -> Op {
    Op::UserInput {
        items: vec![UserInput::Text {
            text: text.to_string(),
            text_elements: Vec::new(),
        }],
        final_output_json_schema: None,
        responsesapi_client_metadata: None,
        additional_context: Default::default(),
        thread_settings: Default::default(),
    }
}

fn read_rollout_items(path: &Path) -> Vec<RolloutItem> {
    fs::read_to_string(path)
        .expect("read rollout")
        .lines()
        .map(|line| serde_json::from_str::<RolloutLine>(line).expect("parse rollout line"))
        .map(|line| line.item)
        .collect()
}

fn recovery_fragment(request: &ResponsesRequest) -> (usize, String) {
    let matches = request
        .input()
        .into_iter()
        .enumerate()
        .filter_map(|(index, item)| {
            let text = item
                .get("content")
                .and_then(serde_json::Value::as_array)
                .and_then(|content| content.first())
                .and_then(|content| content.get("text"))
                .and_then(serde_json::Value::as_str)?;
            (item.get("role").and_then(serde_json::Value::as_str) == Some("developer")
                && text.starts_with("<post_compact_recovery>"))
            .then(|| (index, text.to_string()))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        matches.len(),
        1,
        "request should contain exactly one recovery developer item"
    );
    matches.into_iter().next().expect("one recovery fragment")
}

fn assert_pending_marker_without_application(items: &[RolloutItem]) {
    assert!(
        items.iter().any(|item| matches!(
            item,
            RolloutItem::Compacted(compacted)
                if compacted.post_compact_recovery.is_some()
        )),
        "the durable compaction marker should remain pending"
    );
    assert!(
        !items
            .iter()
            .any(|item| matches!(item, RolloutItem::PostCompactRecoveryApplied(_))),
        "a request without a successful normal response must not write application proof"
    );
}

async fn seed_and_compact(
    codex: &codex_core::CodexConversation,
) -> Result<()> {
    codex.submit(user_turn(FIRST_USER)).await?;
    wait_for_event(codex, |event| matches!(event, EventMsg::TurnComplete(_))).await;
    codex.submit(Op::Compact).await?;
    wait_for_event(codex, |event| matches!(event, EventMsg::Warning(_))).await;
    wait_for_event(codex, |event| matches!(event, EventMsg::TurnComplete(_))).await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn post_compact_recovery_websocket_stream_return_without_response_records_no_sampling_success(
) -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = start_websocket_server(vec![vec![
        vec![
            ev_response_created("first"),
            ev_assistant_message("first-message", FIRST_REPLY),
            ev_completed("first"),
        ],
        vec![
            ev_response_created("compact"),
            ev_assistant_message("compact-message", SUMMARY),
            ev_completed("compact"),
        ],
        Vec::new(),
    ]])
    .await;
    let mut builder = test_codex().with_config(|config| {
        config.compact_prompt = Some(SUMMARIZATION_PROMPT.to_string());
        config.model_provider.request_max_retries = Some(0);
        config.model_provider.stream_max_retries = Some(0);
    });
    let test = builder.build_with_websocket_server(&server).await?;
    let rollout_path = test
        .session_configured
        .rollout_path
        .clone()
        .expect("rollout path");

    seed_and_compact(&test.codex).await?;
    test.codex.submit(user_turn(LIVE_USER)).await?;
    wait_for_event(&test.codex, |event| matches!(event, EventMsg::Error(_))).await;
    wait_for_event(&test.codex, |event| matches!(event, EventMsg::TurnComplete(_))).await;

    assert_pending_marker_without_application(&read_rollout_items(&rollout_path));
    server.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn post_compact_recovery_transport_failure_before_response_records_no_sampling_success()
-> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = start_mock_server().await;
    mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_assistant_message("first-message", FIRST_REPLY),
                ev_completed("first"),
            ]),
            sse(vec![
                ev_assistant_message("compact-message", SUMMARY),
                ev_completed("compact"),
            ]),
        ],
    )
    .await;
    let mut builder = test_codex().with_config(|config| {
        config.compact_prompt = Some(SUMMARIZATION_PROMPT.to_string());
        config.model_provider.request_max_retries = Some(0);
        config.model_provider.stream_max_retries = Some(0);
    });
    let test = builder.build(&server).await?;
    let rollout_path = test
        .session_configured
        .rollout_path
        .clone()
        .expect("rollout path");

    seed_and_compact(&test.codex).await?;
    server.shutdown().await;
    test.codex.submit(user_turn(LIVE_USER)).await?;
    wait_for_event(&test.codex, |event| matches!(event, EventMsg::Error(_))).await;
    wait_for_event(&test.codex, |event| matches!(event, EventMsg::TurnComplete(_))).await;

    assert_pending_marker_without_application(&read_rollout_items(&rollout_path));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn post_compact_recovery_retry_and_tool_continuation_reuse_byte_identical_fragment()
-> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = start_mock_server().await;
    let requests = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_assistant_message("first-message", FIRST_REPLY),
                ev_completed("first"),
            ]),
            sse(vec![
                ev_assistant_message("compact-message", SUMMARY),
                ev_completed("compact"),
            ]),
            sse(vec![ev_response_created("retry-before-response")]),
            sse(vec![
                ev_function_call("unsupported-call", "test_tool", "{}"),
                ev_completed("tool-request"),
            ]),
            sse(vec![
                ev_assistant_message("final-message", "continued after tool output"),
                ev_completed("final"),
            ]),
        ],
    )
    .await;
    let mut builder = test_codex().with_config(|config| {
        config.compact_prompt = Some(SUMMARIZATION_PROMPT.to_string());
        config.model_provider.request_max_retries = Some(0);
        config.model_provider.stream_max_retries = Some(1);
    });
    let test = builder.build(&server).await?;
    let rollout_path = test
        .session_configured
        .rollout_path
        .clone()
        .expect("rollout path");

    seed_and_compact(&test.codex).await?;
    test.codex.submit(user_turn(LIVE_USER)).await?;
    wait_for_event(&test.codex, |event| matches!(event, EventMsg::TurnComplete(_))).await;

    let requests = requests.requests();
    assert_eq!(requests.len(), 5);
    let (failed_index, failed_fragment) = recovery_fragment(&requests[2]);
    let (retry_index, retry_fragment) = recovery_fragment(&requests[3]);
    let (continuation_index, continuation_fragment) = recovery_fragment(&requests[4]);
    assert_eq!(failed_fragment, retry_fragment);
    assert_eq!(retry_fragment, continuation_fragment);

    let items = read_rollout_items(&rollout_path);
    assert!(
        !serde_json::to_string(&items)?.contains("<post_compact_recovery>"),
        "the transient recovery packet must never enter persisted rollout items"
    );
    let compacted = items
        .iter()
        .rev()
        .find_map(|item| match item {
            RolloutItem::Compacted(compacted) => Some(compacted),
            _ => None,
        })
        .expect("recovery-aware compacted item");
    let marker = compacted
        .post_compact_recovery
        .as_ref()
        .expect("recovery marker");
    for (request, recovery_index) in [
        (&requests[2], failed_index),
        (&requests[3], retry_index),
        (&requests[4], continuation_index),
    ] {
        let boundary_index = request
            .input()
            .iter()
            .position(|item| {
                item.get("id").and_then(serde_json::Value::as_str)
                    == Some(marker.boundary_item_id.as_str())
            })
            .expect("prompt should retain exact compaction boundary item");
        assert_eq!(recovery_index, boundary_index + 1);
    }

    let application_items = items
        .iter()
        .filter_map(|item| match item {
            RolloutItem::PostCompactRecoveryApplied(applied) => Some(applied),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(application_items.len(), 1);
    assert_eq!(
        application_items[0].compaction_window_id,
        compacted.window_id.clone().expect("compaction window id")
    );
    assert_eq!(
        application_items[0].boundary_item_id,
        marker.boundary_item_id
    );
    Ok(())
}
