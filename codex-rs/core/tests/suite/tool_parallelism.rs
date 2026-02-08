#![cfg(not(target_os = "windows"))]
#![allow(clippy::unwrap_used)]

use std::fs;
use std::time::Duration;
use std::time::Instant;

use codex_core::protocol::AskForApproval;
use codex_core::protocol::EventMsg;
use codex_core::protocol::Op;
use codex_core::protocol::SandboxPolicy;
use codex_protocol::config_types::ReasoningSummary;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ResponsesRequest;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::ev_shell_command_call_with_args;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::streaming_sse::StreamingSseChunk;
use core_test_support::streaming_sse::start_streaming_sse_server;
use core_test_support::test_codex::TestCodex;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use tokio::sync::oneshot;

async fn run_turn(test: &TestCodex, prompt: &str) -> anyhow::Result<()> {
    let session_model = test.session_configured.model.clone();

    test.codex
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: prompt.into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: test.cwd.path().to_path_buf(),
            approval_policy: AskForApproval::Never,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            model: session_model,
            effort: None,
            summary: ReasoningSummary::Auto,
            collaboration_mode: None,
            personality: None,
        })
        .await?;

    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    Ok(())
}

async fn run_turn_and_measure(test: &TestCodex, prompt: &str) -> anyhow::Result<Duration> {
    let start = Instant::now();
    run_turn(test, prompt).await?;
    Ok(start.elapsed())
}

#[allow(clippy::expect_used)]
async fn build_codex_with_test_tool(server: &wiremock::MockServer) -> anyhow::Result<TestCodex> {
    let mut builder = test_codex().with_model("test-gpt-5.1-codex");
    builder.build(server).await
}

const PARALLEL_SLEEP_AFTER_MS: u64 = 2_000;
const PARALLEL_DURATION_GUARD_MS: u64 = 4_000;

fn has_function_call_output(request: &ResponsesRequest, call_id: &str) -> bool {
    request.input().iter().any(|item| {
        item.get("type").and_then(Value::as_str) == Some("function_call_output")
            && item.get("call_id").and_then(Value::as_str) == Some(call_id)
    })
}

fn assert_function_call_succeeded(request: &ResponsesRequest, call_id: &str) {
    let output = request
        .function_call_output(call_id)
        .get("output")
        .cloned()
        .unwrap_or(Value::Null);

    match output {
        Value::String(content) => {
            let expected = format!("call_id: {call_id}\nok");
            assert!(
                content == "ok" || content == expected,
                "unexpected text output for {call_id}: {content:?}"
            );
        }
        Value::Object(content_obj) => {
            let success = content_obj.get("success").and_then(Value::as_bool);
            let content = content_obj.get("content").and_then(Value::as_str);
            assert_eq!(success, Some(true), "unexpected success flag for {call_id}");
            assert_eq!(
                content,
                Some("ok"),
                "unexpected object content for {call_id}"
            );
        }
        other => panic!("unexpected output type for {call_id}: {other:?}"),
    }
}

fn assert_parallel_duration(actual: Duration) {
    assert!(
        actual < Duration::from_millis(PARALLEL_DURATION_GUARD_MS),
        "expected parallel execution to finish under {PARALLEL_DURATION_GUARD_MS}ms, got {actual:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_file_tools_run_in_parallel() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let test = build_codex_with_test_tool(&server).await?;

    let warmup_args = json!({
        "sleep_after_ms": 10,
        "barrier": {
            "id": "parallel-test-sync-warmup",
            "participants": 2,
            "timeout_ms": 1_000,
        }
    })
    .to_string();

    let parallel_args = json!({
        "sleep_after_ms": PARALLEL_SLEEP_AFTER_MS,
        "barrier": {
            "id": "parallel-test-sync",
            "participants": 2,
            "timeout_ms": 1_000,
        }
    })
    .to_string();

    let warmup_first = sse(vec![
        json!({"type": "response.created", "response": {"id": "resp-warm-1"}}),
        ev_function_call("warm-call-1", "test_sync_tool", &warmup_args),
        ev_function_call("warm-call-2", "test_sync_tool", &warmup_args),
        ev_completed("resp-warm-1"),
    ]);
    let warmup_second = sse(vec![
        ev_assistant_message("warm-msg-1", "warmup complete"),
        ev_completed("resp-warm-2"),
    ]);

    let first_response = sse(vec![
        json!({"type": "response.created", "response": {"id": "resp-1"}}),
        ev_function_call("call-1", "test_sync_tool", &parallel_args),
        ev_function_call("call-2", "test_sync_tool", &parallel_args),
        ev_completed("resp-1"),
    ]);
    let second_response = sse(vec![
        ev_assistant_message("msg-1", "done"),
        ev_completed("resp-2"),
    ]);
    let response_mock = mount_sse_sequence(
        &server,
        vec![warmup_first, warmup_second, first_response, second_response],
    )
    .await;

    run_turn(&test, "warm up parallel tool").await?;

    let duration = run_turn_and_measure(&test, "exercise sync tool").await?;
    let measured_completion_request = response_mock
        .requests()
        .into_iter()
        .find(|request| {
            has_function_call_output(request, "call-1")
                && has_function_call_output(request, "call-2")
        })
        .expect("completion request with call-1/call-2 outputs must be present");
    assert_function_call_succeeded(&measured_completion_request, "call-1");
    assert_function_call_succeeded(&measured_completion_request, "call-2");
    assert_parallel_duration(duration);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shell_tools_finish_quickly_under_load_guard() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mut builder = test_codex().with_model("gpt-5.1");
    let test = builder.build(&server).await?;

    let shell_args = json!({
        "command": "sleep 0.3",
        // Avoid user-specific shell startup cost (e.g. zsh profile scripts) in timing assertions.
        "login": false,
        "timeout_ms": 1_000,
    });
    let args_one = serde_json::to_string(&shell_args)?;
    let args_two = serde_json::to_string(&shell_args)?;

    let first_response = sse(vec![
        json!({"type": "response.created", "response": {"id": "resp-1"}}),
        ev_function_call("call-1", "shell_command", &args_one),
        ev_function_call("call-2", "shell_command", &args_two),
        ev_completed("resp-1"),
    ]);
    let second_response = sse(vec![
        ev_assistant_message("msg-1", "done"),
        ev_completed("resp-2"),
    ]);
    mount_sse_sequence(&server, vec![first_response, second_response]).await;

    let duration = run_turn_and_measure(&test, "run shell_command twice").await?;
    assert!(
        duration < Duration::from_millis(4_000),
        "expected shell tool turn to finish under load guard, got {duration:?}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mixed_tool_turn_finishes_under_load_guard() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let test = build_codex_with_test_tool(&server).await?;

    let sync_args = json!({
        "sleep_after_ms": 300
    })
    .to_string();
    let shell_args = serde_json::to_string(&json!({
        "command": "sleep 0.3",
        "login": false,
        "timeout_ms": 1_000,
    }))?;

    let first_response = sse(vec![
        json!({"type": "response.created", "response": {"id": "resp-1"}}),
        ev_function_call("call-1", "test_sync_tool", &sync_args),
        ev_function_call("call-2", "shell_command", &shell_args),
        ev_completed("resp-1"),
    ]);
    let second_response = sse(vec![
        ev_assistant_message("msg-1", "done"),
        ev_completed("resp-2"),
    ]);
    mount_sse_sequence(&server, vec![first_response, second_response]).await;

    let duration = run_turn_and_measure(&test, "mix tools").await?;
    assert!(
        duration < Duration::from_millis(4_000),
        "expected mixed tool turn to finish under load guard, got {duration:?}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_results_grouped() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let test = build_codex_with_test_tool(&server).await?;

    let shell_args = serde_json::to_string(&json!({
        "command": "echo 'shell output'",
        "timeout_ms": 1_000,
    }))?;

    mount_sse_once(
        &server,
        sse(vec![
            json!({"type": "response.created", "response": {"id": "resp-1"}}),
            ev_function_call("call-1", "shell_command", &shell_args),
            ev_function_call("call-2", "shell_command", &shell_args),
            ev_function_call("call-3", "shell_command", &shell_args),
            ev_completed("resp-1"),
        ]),
    )
    .await;
    let tool_output_request = mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("msg-1", "done"),
            ev_completed("resp-2"),
        ]),
    )
    .await;

    run_turn(&test, "run shell three times").await?;

    let input = tool_output_request.single_request().input();

    // find all function_call inputs with indexes
    let function_calls = input
        .iter()
        .enumerate()
        .filter(|(_, item)| item.get("type").and_then(Value::as_str) == Some("function_call"))
        .collect::<Vec<_>>();

    let function_call_outputs = input
        .iter()
        .enumerate()
        .filter(|(_, item)| {
            item.get("type").and_then(Value::as_str) == Some("function_call_output")
        })
        .collect::<Vec<_>>();

    assert_eq!(function_calls.len(), 3);
    assert_eq!(function_call_outputs.len(), 3);

    for (index, _) in &function_calls {
        for (output_index, _) in &function_call_outputs {
            assert!(
                *index < *output_index,
                "all function calls must come before outputs"
            );
        }
    }

    // output should come in the order of the function calls
    let zipped = function_calls
        .iter()
        .zip(function_call_outputs.iter())
        .collect::<Vec<_>>();
    for (call, output) in zipped {
        assert_eq!(
            call.1.get("call_id").and_then(Value::as_str),
            output.1.get("call_id").and_then(Value::as_str)
        );
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shell_tools_start_before_response_completed_when_stream_delayed() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let output_file = tempfile::NamedTempFile::new()?;
    let output_path = output_file.path();
    let first_response_id = "resp-1";
    let second_response_id = "resp-2";

    let command = format!(
        "perl -MTime::HiRes -e 'print int(Time::HiRes::time()*1000), \"\\n\"' >> \"{}\"",
        output_path.display()
    );
    // Use a non-login shell to avoid slow, user-specific shell init (e.g. zsh profiles)
    // from making this timing-based test flaky.
    let args = json!({
        "command": command,
        "login": false,
        "timeout_ms": 5_000,
    });

    let first_chunk = sse(vec![
        ev_response_created(first_response_id),
        ev_shell_command_call_with_args("call-1", &args),
        ev_shell_command_call_with_args("call-2", &args),
        ev_shell_command_call_with_args("call-3", &args),
        ev_shell_command_call_with_args("call-4", &args),
    ]);
    let second_chunk = sse(vec![ev_completed(first_response_id)]);
    let follow_up = sse(vec![
        ev_assistant_message("msg-1", "done"),
        ev_completed(second_response_id),
    ]);

    let (first_gate_tx, first_gate_rx) = oneshot::channel();
    let (completion_gate_tx, completion_gate_rx) = oneshot::channel();
    let (follow_up_gate_tx, follow_up_gate_rx) = oneshot::channel();
    let (streaming_server, completion_receivers) = start_streaming_sse_server(vec![
        vec![
            StreamingSseChunk {
                gate: Some(first_gate_rx),
                body: first_chunk,
            },
            StreamingSseChunk {
                gate: Some(completion_gate_rx),
                body: second_chunk,
            },
        ],
        vec![StreamingSseChunk {
            gate: Some(follow_up_gate_rx),
            body: follow_up,
        }],
    ])
    .await;

    let mut builder = test_codex().with_model("gpt-5.1");
    let test = builder
        .build_with_streaming_server(&streaming_server)
        .await?;

    let session_model = test.session_configured.model.clone();
    test.codex
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "stream delayed completion".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: test.cwd.path().to_path_buf(),
            approval_policy: AskForApproval::Never,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            model: session_model,
            effort: None,
            summary: ReasoningSummary::Auto,
            collaboration_mode: None,
            personality: None,
        })
        .await?;

    let _ = first_gate_tx.send(());
    let _ = follow_up_gate_tx.send(());

    let timestamps = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let contents = fs::read_to_string(output_path)?;
            let timestamps = contents
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(|line| {
                    line.trim()
                        .parse::<i64>()
                        .map_err(|err| anyhow::anyhow!("invalid timestamp {line:?}: {err}"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            if timestamps.len() == 4 {
                return Ok::<_, anyhow::Error>(timestamps);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;

    let _ = completion_gate_tx.send(());
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let mut completion_iter = completion_receivers.into_iter();
    let completed_at = completion_iter
        .next()
        .expect("completion receiver missing")
        .await
        .expect("completion timestamp missing");
    let count = i64::try_from(timestamps.len()).expect("timestamp count fits in i64");
    assert_eq!(count, 4);

    for timestamp in timestamps {
        assert!(
            timestamp <= completed_at,
            "timestamp {timestamp} should be before or equal to completed {completed_at}"
        );
    }

    streaming_server.shutdown().await;

    Ok(())
}
