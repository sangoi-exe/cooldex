use super::compact::COMPACT_WARNING_MESSAGE;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use codex_core::compact::SUMMARIZATION_PROMPT;
use codex_features::Feature;
use codex_history::HandoffPreparation;
use codex_history::PostCompactRecoveryPayloadKind;
use codex_history::RolloutItem;
use codex_protocol::AgentPath;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::protocol::Op;
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
use core_test_support::test_codex::TestCodex;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::json;
use tokio::sync::oneshot;

// Merge-safety anchor: persisted rollout tests use codex_rollout's canonical JSONL decoder.

const FIRST_USER: &str = "historical user request";
const FIRST_REPLY: &str = "historical assistant response";
const SUMMARY: &str = "bounded compact summary";
const LIVE_USER: &str = "continue only the live work";
const PRE_STOP_REPLY: &str = "draft before stop hook";
const STOP_CONTINUATION_PROMPT: &str = "continue after the blocking stop hook";
const AFTER_RECOVERY_USER: &str = "start a genuinely new turn";
const STEER_DURING_STOP: &str = "steer while the stop hook is waiting";
const CUSTOM_RECOVERY_INSTRUCTIONS: &str = "Use the configured post-compact recovery instructions.";

type RecoveryFragment = (usize, (String, String));

fn user_turn(text: &str) -> codex_protocol::turn_input::TurnInputRequest {
    codex_protocol::turn_input::TurnInputRequest::user_input(vec![UserInput::Text {
        text: text.to_string(),
        text_elements: Vec::new(),
    }])
}

fn test_rollout_path(test: &TestCodex) -> PathBuf {
    test.session_configured
        .rollout_path
        .clone()
        .expect("rollout path")
}

fn read_rollout_items(path: &Path) -> Vec<RolloutItem> {
    fs::read_to_string(path)
        .expect("read rollout")
        .lines()
        .map(|line| codex_rollout::parse_rollout_line(line).expect("parse rollout line"))
        .map(|line| line.item)
        .collect()
}

fn matching_recovery_fragments(
    request: &ResponsesRequest,
    expected_role: &str,
    expected_content_type: &str,
    marker: &str,
) -> Vec<RecoveryFragment> {
    request
        .input()
        .into_iter()
        .enumerate()
        .filter_map(|(index, item)| {
            let id = item.get("id").and_then(serde_json::Value::as_str)?;
            let content = item
                .get("content")
                .and_then(serde_json::Value::as_array)
                .and_then(|content| content.first())?;
            let text = content.get("text").and_then(serde_json::Value::as_str)?;
            (item.get("role").and_then(serde_json::Value::as_str) == Some(expected_role)
                && content.get("type").and_then(serde_json::Value::as_str)
                    == Some(expected_content_type)
                && text.starts_with(marker))
            .then(|| (index, (id.to_string(), text.to_string())))
        })
        .collect()
}

fn recovery_fragment(
    request: &ResponsesRequest,
    expected_role: &str,
    expected_content_type: &str,
    marker: &str,
) -> RecoveryFragment {
    let matches =
        matching_recovery_fragments(request, expected_role, expected_content_type, marker);
    assert_eq!(
        matches.len(),
        1,
        "request should contain exactly one {expected_role}/{expected_content_type} item starting with {marker}"
    );
    matches.into_iter().next().expect("one recovery fragment")
}

fn recovery_fragments(request: &ResponsesRequest) -> (RecoveryFragment, Option<RecoveryFragment>) {
    let mut handoff = matching_recovery_fragments(
        request,
        "assistant",
        "output_text",
        "<post_compact_handoff>",
    );
    let handoff_carrier_count = request
        .input()
        .iter()
        .filter(|item| {
            item.get("content")
                .and_then(serde_json::Value::as_array)
                .and_then(|content| content.first())
                .and_then(|content| content.get("text"))
                .and_then(serde_json::Value::as_str)
                .is_some_and(|text| text.starts_with("<post_compact_handoff>"))
        })
        .count();
    assert_eq!(
        handoff.len(),
        handoff_carrier_count,
        "every post-compact handoff carrier must use assistant/output_text"
    );
    assert!(
        handoff.len() <= 1,
        "request should contain at most one post-compact handoff item"
    );
    (
        recovery_fragment(
            request,
            "developer",
            "input_text",
            "<post_compact_recovery>",
        ),
        handoff.pop(),
    )
}

fn assert_no_recovery_fragments(request: &ResponsesRequest) {
    let serialized = serde_json::to_string(&request.input()).expect("serialize request input");
    assert!(!serialized.contains("<post_compact_recovery>"));
    assert!(!serialized.contains("<post_compact_handoff>"));
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
    codex.start_or_steer_turn(user_turn(FIRST_USER)).await?;
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
        // Merge-safety anchor: streaming recovery fixtures reserve the operation-local handoff
        // request before local provider compaction so the later failed sampling stream remains
        // the user turn under test.
        vec![StreamingSseChunk {
            gate: None,
            body: sse(vec![
                ev_assistant_message("pre-compact-handoff", "continue after compaction"),
                ev_completed("pre-compact-handoff"),
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
    let rollout_path = test_rollout_path(&test);

    seed_and_compact(&test.codex).await?;
    test.codex.start_or_steer_turn(user_turn(LIVE_USER)).await?;
    wait_for_failed_turn_complete(&test.codex).await;

    assert_pending_marker_without_application(&read_rollout_items(&rollout_path));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
// Merge-safety anchor: mailbox preemption before response.completed leaves the exact pending
// recovery packet intact, so the mailbox continuation—not an unaccepted stream—consumes it.
async fn post_compact_recovery_mailbox_preemption_before_completed_keeps_packet_pending()
-> Result<()> {
    skip_if_no_network!(Ok(()));
    let (release_preemption_tx, release_preemption_rx) = oneshot::channel();
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
                ev_assistant_message("pre-compact-handoff", "continue after compaction"),
                ev_completed("pre-compact-handoff"),
            ]),
        }],
        vec![StreamingSseChunk {
            gate: None,
            body: sse(vec![
                ev_assistant_message("compact-message", SUMMARY),
                ev_completed("compact"),
            ]),
        }],
        vec![
            StreamingSseChunk {
                gate: None,
                body: sse(vec![ev_response_created("preempted-response")]),
            },
            StreamingSseChunk {
                gate: Some(release_preemption_rx),
                body: sse(vec![
                    json!({
                        "type": "response.output_item.done",
                        "item": {
                            "type": "message",
                            "role": "assistant",
                            "id": "preempted-commentary",
                            "content": [{"type": "output_text", "text": "working"}],
                            "phase": "commentary",
                        }
                    }),
                    ev_completed("preempted-response"),
                ]),
            },
        ],
        vec![StreamingSseChunk {
            gate: None,
            body: sse(vec![ev_response_created("continuation-closed")]),
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
    let rollout_path = test_rollout_path(&test);

    seed_and_compact(&test.codex).await?;
    test.codex.start_or_steer_turn(user_turn(LIVE_USER)).await?;
    server.wait_for_request_count(4).await;

    test.codex
        .submit(Op::InterAgentCommunication {
            communication: InterAgentCommunication::new(
                AgentPath::try_from("/root/worker").expect("worker path should parse"),
                AgentPath::root(),
                Vec::new(),
                "queued mailbox input".to_string(),
                /*trigger_turn*/ false,
            ),
            start_options: Default::default(),
        })
        .await?;
    test.codex
        .submit(Op::RealtimeConversationListVoices)
        .await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::RealtimeConversationListVoicesResponse(_))
    })
    .await;

    release_preemption_tx
        .send(())
        .expect("release the preempted response");
    wait_for_failed_turn_complete(&test.codex).await;
    server.wait_for_request_count(5).await;

    let requests = server.requests().await;
    assert_eq!(requests.len(), 5);
    for request in [&requests[3], &requests[4]] {
        assert!(
            String::from_utf8_lossy(request).contains("<post_compact_recovery>"),
            "pre-completion mailbox preemption must preserve recovery for the next sample"
        );
    }
    assert_pending_marker_without_application(&read_rollout_items(&rollout_path));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
// Merge-safety anchor: an accepted response persists recovery before draining a blocking tool,
// so an interrupt cannot restore the packet or inject it into a later request.
async fn post_compact_recovery_completion_before_blocking_tool_interrupt_remains_consumed()
-> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = start_mock_server().await;
    let blocking_tool_args = serde_json::json!({
        "cmd": "sleep 60",
        "yield_time_ms": 60_000,
    })
    .to_string();
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
                ev_function_call("blocking-tool", "exec_command", &blocking_tool_args),
                ev_completed("recovery-tool-response"),
            ]),
            sse(vec![
                ev_assistant_message("after-interrupt", "continued after interruption"),
                ev_completed("after-interrupt"),
            ]),
        ],
    )
    .await;
    let mut builder = test_codex().with_config(|config| {
        config.model_provider.name = "OpenAI-compatible test provider".to_string();
        config.compact_prompt = Some(SUMMARIZATION_PROMPT.to_string());
        config.model_provider.request_max_retries = Some(0);
        config.model_provider.stream_max_retries = Some(0);
    });
    let test = builder.build(&server).await?;
    let rollout_path = test_rollout_path(&test);

    seed_and_compact(&test.codex).await?;
    test.codex.start_or_steer_turn(user_turn(LIVE_USER)).await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::ExecCommandBegin(_))
    })
    .await;

    let application_items_before_interrupt = read_rollout_items(&rollout_path)
        .into_iter()
        .filter(|item| matches!(item, RolloutItem::PostCompactRecoveryApplied(_)))
        .count();
    assert_eq!(requests.requests().len(), 3);

    test.codex.submit(Op::Interrupt).await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnAborted(_))
    })
    .await;
    test.codex.submit(Op::CleanBackgroundTerminals).await?;

    assert_eq!(
        application_items_before_interrupt, 1,
        "the accepted response must write one proof before the blocking tool is interrupted"
    );
    test.codex
        .start_or_steer_turn(user_turn(AFTER_RECOVERY_USER))
        .await?;
    wait_for_successful_turn_complete(&test.codex).await;

    let requests = requests.requests();
    assert_eq!(requests.len(), 4);
    let _ = recovery_fragments(&requests[2]);
    assert_no_recovery_fragments(&requests[3]);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
// Merge-safety anchor: a fatal recovery-proof write remains the terminal error after the
// already-started blocking tool is cleaned up and a later interrupt cancels the turn.
async fn post_compact_recovery_application_failure_survives_blocking_tool_interrupt() -> Result<()>
{
    skip_if_no_network!(Ok(()));
    let (release_completed_tx, release_completed_rx) = oneshot::channel();
    let blocking_tool_args = json!({
        "questions": [{
            "id": "confirm",
            "header": "Confirm",
            "question": "Keep the turn blocked until cancellation?",
            "options": [{
                "label": "Continue",
                "description": "Keep waiting."
            }]
        }]
    })
    .to_string();
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
                ev_assistant_message("pre-compact-handoff", "continue after compaction"),
                ev_completed("pre-compact-handoff"),
            ]),
        }],
        vec![StreamingSseChunk {
            gate: None,
            body: sse(vec![
                ev_assistant_message("compact-message", SUMMARY),
                ev_completed("compact"),
            ]),
        }],
        vec![
            StreamingSseChunk {
                gate: None,
                body: sse(vec![ev_function_call(
                    "blocking-tool",
                    "request_user_input",
                    &blocking_tool_args,
                )]),
            },
            StreamingSseChunk {
                gate: Some(release_completed_rx),
                body: sse(vec![ev_completed("recovery-tool-response")]),
            },
        ],
    ])
    .await;
    let mut builder = test_codex().with_config(|config| {
        config.model_provider.name = "OpenAI-compatible test provider".to_string();
        config.compact_prompt = Some(SUMMARIZATION_PROMPT.to_string());
        config.model_provider.request_max_retries = Some(0);
        config.model_provider.stream_max_retries = Some(0);
        config
            .features
            .enable(Feature::DefaultModeRequestUserInput)
            .expect("enable request_user_input in Default mode");
    });
    let test = builder.build_with_streaming_server(&server).await?;

    seed_and_compact(&test.codex).await?;
    test.codex.start_or_steer_turn(user_turn(LIVE_USER)).await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::RequestUserInput(_))
    })
    .await;
    test.thread_store
        .shutdown_thread(test.session_configured.thread_id)
        .await?;

    release_completed_tx
        .send(())
        .expect("release the completed recovery response");
    wait_for_event(&test.codex, |event| {
        matches!(
            event,
            EventMsg::RawResponseCompleted(completed)
                if completed.response_id == "recovery-tool-response"
        )
    })
    .await;
    // RawResponseCompleted is emitted immediately before recovery acknowledgement. Yield once so
    // the sampling task enters its already-started tool drain after the injected write failure.
    tokio::task::yield_now().await;
    test.codex.submit(Op::Interrupt).await?;

    let terminal = wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::Error(_) | EventMsg::TurnAborted(_))
    })
    .await;
    let EventMsg::Error(error) = terminal else {
        panic!(
            "the recovery persistence failure must win over a later interrupt, got {terminal:?}"
        );
    };
    assert!(
        error
            .message
            .starts_with("Fatal error: failed to persist post-compact recovery application proof:"),
        "the recovery persistence failure, not TurnAborted, must reach the terminal error path: {error:?}"
    );
    let EventMsg::TurnComplete(completed) = wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await
    else {
        unreachable!("predicate guarantees a turn complete event");
    };
    assert_eq!(completed.error.as_ref(), Some(&error));
    assert!(
        tokio::time::timeout(Duration::from_millis(200), async {
            loop {
                let event = test
                    .codex
                    .next_event()
                    .await
                    .expect("event stream should remain open after the terminal error");
                if matches!(event.msg, EventMsg::TurnAborted(_)) {
                    return event;
                }
            }
        })
        .await
        .is_err(),
        "a recovery persistence failure must not be followed by TurnAborted"
    );
    test.codex.submit(Op::CleanBackgroundTerminals).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
// Merge-safety anchor: the first accepted normal sampling response consumes recovery before
// tool and stop-hook continuations can form another request.
async fn post_compact_recovery_retry_reuses_fragments_and_sampling_success_consumes_before_continuations()
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
            config.post_compact_recovery_instructions =
                Some(CUSTOM_RECOVERY_INSTRUCTIONS.to_string());
            config.model_provider.request_max_retries = Some(0);
            config.model_provider.stream_max_retries = Some(1);
        });
    let test = builder.build(&server).await?;
    let rollout_path = test_rollout_path(&test);

    seed_and_compact(&test.codex).await?;
    test.codex.start_or_steer_turn(user_turn(LIVE_USER)).await?;
    wait_for_successful_turn_complete(&test.codex).await;
    test.codex
        .start_or_steer_turn(user_turn(AFTER_RECOVERY_USER))
        .await?;
    wait_for_successful_turn_complete(&test.codex).await;

    let requests = requests.requests();
    assert_eq!(requests.len(), 7);
    let ((failed_recovery_index, failed_recovery), failed_handoff) =
        recovery_fragments(&requests[2]);
    let ((retry_recovery_index, retry_recovery), retry_handoff) = recovery_fragments(&requests[3]);
    assert_eq!(failed_recovery, retry_recovery);
    assert!(failed_recovery.1.contains(CUSTOM_RECOVERY_INSTRUCTIONS));
    assert_eq!(failed_handoff, retry_handoff);
    for request in [&requests[4], &requests[5], &requests[6]] {
        assert_no_recovery_fragments(request);
    }

    let items = read_rollout_items(&rollout_path);
    let persisted_items = serde_json::to_string(&items)?;
    assert!(
        !persisted_items.contains("<post_compact_recovery>"),
        "the transient recovery packet must never enter persisted rollout items"
    );
    assert!(
        !persisted_items.contains("<post_compact_handoff>"),
        "the transient handoff packet must never enter persisted rollout items"
    );
    assert!(
        !persisted_items.contains("continue after compaction"),
        "generated handoff content must never enter persisted rollout items"
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
    assert_eq!(
        &marker.handoff_preparation,
        &HandoffPreparation::Available,
        "the marker records successful synthesis without generated handoff content"
    );
    for (request, recovery_index, handoff_index) in [
        (
            &requests[2],
            failed_recovery_index,
            failed_handoff.as_ref().map(|(index, _)| *index),
        ),
        (
            &requests[3],
            retry_recovery_index,
            retry_handoff.as_ref().map(|(index, _)| *index),
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
        if let Some(handoff_index) = handoff_index {
            assert_eq!(handoff_index, boundary_index + 1);
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
    assert!(
        requests[5].input().iter().any(|item| {
            item.get("role").and_then(serde_json::Value::as_str) == Some("user")
                && item
                    .get("content")
                    .and_then(serde_json::Value::as_array)
                    .and_then(|content| content.first())
                    .and_then(|content| content.get("text"))
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|text| text.contains(STOP_CONTINUATION_PROMPT))
        }),
        "stop hook continuation should be sampled after recovery consumption"
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
        &application_items[0].payload_kind,
        &PostCompactRecoveryPayloadKind::HandoffAndRecovery,
        "the proof describes the exact recovery packet sampled by the accepted response"
    );
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
async fn post_compact_recovery_steer_during_stop_hook_uses_consumed_recovery_state() -> Result<()> {
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
    let rollout_path = test_rollout_path(&test);
    let started_path = test
        .codex_home_path()
        .join("recovery_waiting_stop_hook_started");
    let release_path = test
        .codex_home_path()
        .join("recovery_waiting_stop_hook_release");

    seed_and_compact(&test.codex).await?;
    test.codex.start_or_steer_turn(user_turn(LIVE_USER)).await?;
    fs_wait::wait_for_path_exists(&started_path, Duration::from_secs(5)).await?;

    test.codex
        .start_or_steer_turn(user_turn(STEER_DURING_STOP))
        .await?;
    submit_thread_settings(
        &test.codex,
        ThreadSettingsOverrides {
            service_tier: Some(None),
            ..Default::default()
        },
    )
    .await?;
    // Merge-safety anchor: recovery is applied after the accepted response and before a
    // blocked stop hook permits a steered continuation.
    let application_count = read_rollout_items(&rollout_path)
        .into_iter()
        .filter(|item| matches!(item, RolloutItem::PostCompactRecoveryApplied(_)))
        .count();
    assert_eq!(application_count, 1);

    fs::write(&release_path, "release").expect("release waiting stop hook");
    wait_for_successful_turn_complete(&test.codex).await;

    let requests = requests.requests();
    assert_eq!(requests.len(), 4);
    let _ = recovery_fragments(&requests[2]);
    assert_no_recovery_fragments(&requests[3]);

    let steer_input = requests[3].input();
    assert!(
        steer_input.iter().any(|item| {
            item.get("role").and_then(serde_json::Value::as_str) == Some("user")
                && item
                    .get("content")
                    .and_then(serde_json::Value::as_array)
                    .and_then(|content| content.first())
                    .and_then(|content| content.get("text"))
                    .and_then(serde_json::Value::as_str)
                    == Some(STEER_DURING_STOP)
        }),
        "steered user input should be sampled before task completion"
    );

    let application_count = read_rollout_items(&rollout_path)
        .into_iter()
        .filter(|item| matches!(item, RolloutItem::PostCompactRecoveryApplied(_)))
        .count();
    assert_eq!(application_count, 1);
    Ok(())
}
