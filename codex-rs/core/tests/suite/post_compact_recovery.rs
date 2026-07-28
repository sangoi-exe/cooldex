use super::compact::COMPACT_WARNING_MESSAGE;
use std::fs;
use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use codex_core::compact::SUMMARIZATION_PROMPT;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::RolloutLine;
use codex_protocol::protocol::ThreadSettingsOverrides;
use codex_protocol::protocol::WarningEvent;
use codex_protocol::user_input::UserInput;
use core_test_support::fs_wait;
use core_test_support::hooks::trust_discovered_hooks;
use core_test_support::responses::ResponsesRequest;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::streaming_sse::StreamingSseChunk;
use core_test_support::streaming_sse::start_streaming_sse_server;
use core_test_support::submit_thread_settings;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;

const FIRST_USER: &str = "historical user request";
const FIRST_REPLY: &str = "historical assistant response";
const SUMMARY: &str = "bounded compact summary";
const LIVE_USER: &str = "continue only the live work";
const PRE_STOP_REPLY: &str = "draft before stop hook";
const STOP_CONTINUATION_PROMPT: &str = "continue after the blocking stop hook";
const AFTER_RECOVERY_USER: &str = "start a genuinely new turn";
const STEER_DURING_STOP: &str = "steer while the stop hook is waiting";

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

fn matching_recovery_fragments(
    request: &ResponsesRequest,
    expected_role: &str,
    marker: &str,
) -> Vec<(usize, String)> {
    request
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
            (item.get("role").and_then(serde_json::Value::as_str) == Some(expected_role)
                && text.starts_with(marker))
            .then(|| (index, text.to_string()))
        })
        .collect()
}

fn recovery_fragment(
    request: &ResponsesRequest,
    expected_role: &str,
    marker: &str,
) -> (usize, String) {
    let matches = matching_recovery_fragments(request, expected_role, marker);
    assert_eq!(
        matches.len(),
        1,
        "request should contain exactly one {expected_role} item starting with {marker}"
    );
    matches.into_iter().next().expect("one recovery fragment")
}

fn recovery_fragments(request: &ResponsesRequest) -> ((usize, String), Option<(usize, String)>) {
    let mut recall = matching_recovery_fragments(request, "user", "<post_compact_recall>");
    assert!(
        recall.len() <= 1,
        "request should contain at most one post-compact recall item"
    );
    (
        recovery_fragment(request, "developer", "<post_compact_recovery>"),
        recall.pop(),
    )
}

fn assert_no_recovery_fragments(request: &ResponsesRequest) {
    let serialized = serde_json::to_string(&request.input()).expect("serialize request input");
    assert!(!serialized.contains("<post_compact_recovery>"));
    assert!(!serialized.contains("<post_compact_recall>"));
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

async fn wait_for_successful_turn_complete(codex: &codex_core::CodexThread) {
    let EventMsg::TurnComplete(completed) =
        wait_for_event(codex, |event| matches!(event, EventMsg::TurnComplete(_))).await
    else {
        unreachable!("predicate guarantees a turn complete event");
    };
    assert_eq!(completed.error, None);
}

async fn wait_for_failed_turn_complete(codex: &codex_core::CodexThread) {
    let EventMsg::Error(error) =
        wait_for_event(codex, |event| matches!(event, EventMsg::Error(_))).await
    else {
        unreachable!("predicate guarantees an error event");
    };
    let EventMsg::TurnComplete(completed) =
        wait_for_event(codex, |event| matches!(event, EventMsg::TurnComplete(_))).await
    else {
        unreachable!("predicate guarantees a turn complete event");
    };
    assert_eq!(completed.error.as_ref(), Some(&error));
}

async fn seed_and_compact(codex: &codex_core::CodexThread) -> Result<()> {
    codex.submit(user_turn(FIRST_USER)).await?;
    wait_for_successful_turn_complete(codex).await;
    codex.submit(Op::Compact).await?;
    let EventMsg::Warning(WarningEvent { message }) = wait_for_event(codex, |event| {
        matches!(
            event,
            EventMsg::Warning(WarningEvent { message }) if message == COMPACT_WARNING_MESSAGE
        )
    })
    .await
    else {
        unreachable!("predicate guarantees a compact warning event");
    };
    assert_eq!(message, COMPACT_WARNING_MESSAGE);
    wait_for_successful_turn_complete(codex).await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn post_compact_recovery_stream_closes_after_created_without_sampling_success() -> Result<()>
{
    skip_if_no_network!(Ok(()));
    let (server, _) = start_streaming_sse_server(vec![
        vec![StreamingSseChunk {
            gate: None,
            body: sse(vec![
                ev_assistant_message("first-message", FIRST_REPLY),
                ev_completed("first"),
            ]),
        }],
        vec![StreamingSseChunk {
            gate: None,
            body: sse(vec![
                ev_assistant_message("compact-message", SUMMARY),
                ev_completed("compact"),
            ]),
        }],
        vec![StreamingSseChunk {
            gate: None,
            body: sse(vec![ev_response_created("created-without-completed")]),
        }],
    ])
    .await;
    let mut builder = test_codex().with_config(|config| {
        config.model_provider.name = "OpenAI-compatible test provider".to_string();
        config.compact_prompt = Some(SUMMARIZATION_PROMPT.to_string());
        config.model_provider.request_max_retries = Some(0);
        config.model_provider.stream_max_retries = Some(0);
    });
    let test = builder.build_with_streaming_server(&server).await?;
    let rollout_path = test
        .session_configured
        .rollout_path
        .clone()
        .expect("rollout path");

    seed_and_compact(&test.codex).await?;
    test.codex.submit(user_turn(LIVE_USER)).await?;
    wait_for_failed_turn_complete(&test.codex).await;

    assert_pending_marker_without_application(&read_rollout_items(&rollout_path));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn post_compact_recovery_retry_reuses_fragments_and_sampling_success_consumes_once()
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
                ev_assistant_message("pre-stop-message", PRE_STOP_REPLY),
                ev_completed("pre-stop"),
            ]),
            sse(vec![
                ev_assistant_message("final-message", "continued after stop hook"),
                ev_completed("final"),
            ]),
            sse(vec![
                ev_assistant_message("after-recovery", "handled new turn"),
                ev_completed("after-recovery"),
            ]),
        ],
    )
    .await;
    let mut builder = test_codex()
        .with_pre_build_hook(|home| {
            let script_path = home.join("recovery_stop_hook.py");
            let marker_path = home.join("recovery_stop_hook_blocked");
            let pre_stop_reply =
                serde_json::to_string(PRE_STOP_REPLY).expect("serialize pre-stop reply");
            let continuation_prompt = serde_json::to_string(STOP_CONTINUATION_PROMPT)
                .expect("serialize stop continuation prompt");
            let script = format!(
                r#"import json
from pathlib import Path
import sys

payload = json.load(sys.stdin)
marker_path = Path(r"{marker_path}")
if payload.get("last_assistant_message") == {pre_stop_reply} and not marker_path.exists():
    marker_path.write_text("blocked", encoding="utf-8")
    print(json.dumps({{"decision": "block", "reason": {continuation_prompt}}}))
else:
    print(json.dumps({{"systemMessage": "stop hook passed"}}))
"#,
                marker_path = marker_path.display(),
            );
            let hooks = serde_json::json!({
                "hooks": {
                    "Stop": [{
                        "hooks": [{
                            "type": "command",
                            "command": format!("python3 {}", script_path.display()),
                        }]
                    }]
                }
            });
            fs::write(&script_path, script).expect("write targeted stop hook fixture");
            fs::write(home.join("hooks.json"), hooks.to_string())
                .expect("write targeted hooks.json");
        })
        .with_config(|config| {
            trust_discovered_hooks(config);
            config.model_provider.name = "OpenAI-compatible test provider".to_string();
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
    wait_for_successful_turn_complete(&test.codex).await;
    test.codex.submit(user_turn(AFTER_RECOVERY_USER)).await?;
    wait_for_successful_turn_complete(&test.codex).await;

    let requests = requests.requests();
    assert_eq!(requests.len(), 7);
    let ((failed_recovery_index, failed_recovery), failed_recall) =
        recovery_fragments(&requests[2]);
    let ((retry_recovery_index, retry_recovery), retry_recall) = recovery_fragments(&requests[3]);
    let ((follow_up_recovery_index, follow_up_recovery), follow_up_recall) =
        recovery_fragments(&requests[4]);
    let ((stop_hook_recovery_index, stop_hook_recovery), stop_hook_recall) =
        recovery_fragments(&requests[5]);
    assert_eq!(failed_recovery, retry_recovery);
    assert_eq!(failed_recovery, follow_up_recovery);
    assert_eq!(failed_recovery, stop_hook_recovery);
    assert_eq!(failed_recall, retry_recall);
    assert_eq!(failed_recall, follow_up_recall);
    assert_eq!(failed_recall, stop_hook_recall);
    assert_no_recovery_fragments(&requests[6]);

    let items = read_rollout_items(&rollout_path);
    assert!(
        !serde_json::to_string(&items)?.contains("<post_compact_recovery>"),
        "the transient recovery packet must never enter persisted rollout items"
    );
    assert!(
        !serde_json::to_string(&items)?.contains("<post_compact_recall>"),
        "the transient recall packet must never enter persisted rollout items"
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
    for (request, recovery_index, recall_index) in [
        (
            &requests[2],
            failed_recovery_index,
            failed_recall.as_ref().map(|(index, _)| *index),
        ),
        (
            &requests[3],
            retry_recovery_index,
            retry_recall.as_ref().map(|(index, _)| *index),
        ),
        (
            &requests[4],
            follow_up_recovery_index,
            follow_up_recall.as_ref().map(|(index, _)| *index),
        ),
        (
            &requests[5],
            stop_hook_recovery_index,
            stop_hook_recall.as_ref().map(|(index, _)| *index),
        ),
    ] {
        let input = request.input();
        let boundary_index = input
            .iter()
            .position(|item| {
                item.get("id").and_then(serde_json::Value::as_str)
                    == Some(marker.boundary_item_id.as_str())
            })
            .expect("prompt should retain exact compaction boundary item");
        if let Some(recall_index) = recall_index {
            assert_eq!(recall_index, boundary_index + 1);
            assert_eq!(recovery_index, boundary_index + 2);
        } else {
            assert_eq!(recovery_index, boundary_index + 1);
        }
        let live_user_index = input
            .iter()
            .position(|item| {
                item.get("role").and_then(serde_json::Value::as_str) == Some("user")
                    && item
                        .get("content")
                        .and_then(serde_json::Value::as_array)
                        .and_then(|content| content.first())
                        .and_then(|content| content.get("text"))
                        .and_then(serde_json::Value::as_str)
                        == Some(LIVE_USER)
            })
            .expect("genuinely new user input after standalone compact");
        assert!(
            recovery_index < live_user_index,
            "the recovery directive must precede genuinely new user input"
        );
    }
    let stop_hook_prompt_index = requests[5]
        .input()
        .iter()
        .position(|item| {
            item.get("role").and_then(serde_json::Value::as_str) == Some("user")
                && item
                    .get("content")
                    .and_then(serde_json::Value::as_array)
                    .and_then(|content| content.first())
                    .and_then(|content| content.get("text"))
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|text| text.contains(STOP_CONTINUATION_PROMPT))
        })
        .expect("stop hook continuation prompt");
    assert!(
        stop_hook_recovery_index < stop_hook_prompt_index,
        "the recovery directive must remain before stop-hook continuation input"
    );

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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn post_compact_recovery_steer_during_stop_hook_waits_for_task_terminal_boundary()
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
            sse(vec![
                ev_assistant_message("pre-stop-message", PRE_STOP_REPLY),
                ev_completed("pre-stop"),
            ]),
            sse(vec![
                ev_assistant_message("after-steer", "handled steer after stop hook"),
                ev_completed("after-steer"),
            ]),
        ],
    )
    .await;
    let mut builder = test_codex()
        .with_pre_build_hook(|home| {
            let script_path = home.join("recovery_waiting_stop_hook.py");
            let started_path = home.join("recovery_waiting_stop_hook_started");
            let release_path = home.join("recovery_waiting_stop_hook_release");
            let pre_stop_reply =
                serde_json::to_string(PRE_STOP_REPLY).expect("serialize pre-stop reply");
            let script = format!(
                r#"import json
from pathlib import Path
import sys
import time

payload = json.load(sys.stdin)
started_path = Path(r"{started_path}")
release_path = Path(r"{release_path}")
if payload.get("last_assistant_message") == {pre_stop_reply}:
    started_path.write_text("waiting", encoding="utf-8")
    deadline = time.monotonic() + 10
    while not release_path.exists():
        if time.monotonic() >= deadline:
            raise TimeoutError("timed out waiting to release stop hook")
        time.sleep(0.01)
print(json.dumps({{"systemMessage": "stop hook passed"}}))
"#,
                release_path = release_path.display(),
                started_path = started_path.display(),
            );
            let hooks = serde_json::json!({
                "hooks": {
                    "Stop": [{
                        "hooks": [{
                            "type": "command",
                            "command": format!("python3 {}", script_path.display()),
                        }]
                    }]
                }
            });
            fs::write(&script_path, script).expect("write waiting stop hook fixture");
            fs::write(home.join("hooks.json"), hooks.to_string())
                .expect("write waiting hooks.json");
        })
        .with_config(|config| {
            trust_discovered_hooks(config);
            config.model_provider.name = "OpenAI-compatible test provider".to_string();
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
    let started_path = test
        .codex_home_path()
        .join("recovery_waiting_stop_hook_started");
    let release_path = test
        .codex_home_path()
        .join("recovery_waiting_stop_hook_release");

    seed_and_compact(&test.codex).await?;
    test.codex.submit(user_turn(LIVE_USER)).await?;
    fs_wait::wait_for_path_exists(&started_path, Duration::from_secs(5)).await?;

    test.codex.submit(user_turn(STEER_DURING_STOP)).await?;
    submit_thread_settings(
        &test.codex,
        ThreadSettingsOverrides {
            service_tier: Some(None),
            ..Default::default()
        },
    )
    .await?;
    assert_pending_marker_without_application(&read_rollout_items(&rollout_path));

    fs::write(&release_path, "release").expect("release waiting stop hook");
    wait_for_successful_turn_complete(&test.codex).await;

    let requests = requests.requests();
    assert_eq!(requests.len(), 4);
    let ((_first_recovery_index, first_recovery), first_recall) = recovery_fragments(&requests[2]);
    let ((steer_recovery_index, steer_recovery), steer_recall) = recovery_fragments(&requests[3]);
    assert_eq!(first_recovery, steer_recovery);
    assert_eq!(first_recall, steer_recall);

    let steer_input = requests[3].input();
    let steer_user_index = steer_input
        .iter()
        .position(|item| {
            item.get("role").and_then(serde_json::Value::as_str) == Some("user")
                && item
                    .get("content")
                    .and_then(serde_json::Value::as_array)
                    .and_then(|content| content.first())
                    .and_then(|content| content.get("text"))
                    .and_then(serde_json::Value::as_str)
                    == Some(STEER_DURING_STOP)
        })
        .expect("steered user input should be sampled before task completion");
    assert!(
        steer_recovery_index < steer_user_index,
        "the recovery directive must survive until the steered continuation request"
    );

    let application_count = read_rollout_items(&rollout_path)
        .into_iter()
        .filter(|item| matches!(item, RolloutItem::PostCompactRecoveryApplied(_)))
        .count();
    assert_eq!(application_count, 1);
    Ok(())
}
