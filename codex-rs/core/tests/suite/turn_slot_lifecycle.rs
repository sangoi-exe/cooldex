use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use codex_core::config::Config;
use codex_extension_api::ExtensionFuture;
use codex_extension_api::ExtensionRegistryBuilder;
use codex_extension_api::TurnLifecycleContributor;
use codex_extension_api::TurnStopInput;
use codex_protocol::protocol::EventMsg;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::sse;
use core_test_support::streaming_sse::StreamingSseChunk;
use core_test_support::streaming_sse::start_streaming_sse_server;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::from_slice;

struct BlockFirstTurnStop {
    blocked: AtomicBool,
    entered_tx: async_channel::Sender<()>,
    release_rx: async_channel::Receiver<()>,
}

impl TurnLifecycleContributor for BlockFirstTurnStop {
    fn on_turn_stop<'a>(&'a self, _input: TurnStopInput<'a>) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            if self.blocked.swap(true, Ordering::SeqCst) {
                return;
            }
            self.entered_tx
                .send(())
                .await
                .expect("turn-stop observer should remain open");
            self.release_rx
                .recv()
                .await
                .expect("turn-stop hook should be released");
        })
    }
}

fn chunk(event: Value) -> StreamingSseChunk {
    StreamingSseChunk {
        gate: None,
        body: sse(vec![event]),
    }
}

fn response_chunks(response_id: &str, message_id: &str, text: &str) -> Vec<StreamingSseChunk> {
    vec![
        chunk(ev_response_created(response_id)),
        chunk(ev_assistant_message(message_id, text)),
        chunk(ev_completed(response_id)),
    ]
}

fn message_input_texts(body: &Value, role: &str) -> Vec<String> {
    body.get("input")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("message"))
        .filter(|item| item.get("role").and_then(Value::as_str) == Some(role))
        .filter_map(|item| item.get("content").and_then(Value::as_array))
        .flatten()
        .filter(|span| span.get("type").and_then(Value::as_str) == Some("input_text"))
        .filter_map(|span| span.get("text").and_then(Value::as_str).map(str::to_owned))
        .collect()
}

async fn submit_user_input(codex: &codex_core::CodexThread, text: &str) {
    codex
        .start_or_steer_turn(codex_protocol::turn_input::TurnInputRequest::user_input(
            vec![UserInput::Text {
                text: text.to_string(),
                text_elements: Vec::new(),
            }],
        ))
        .await
        .expect("submit user input");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fresh_submit_waits_for_prior_turn_terminal_transition() {
    let (stop_entered_tx, stop_entered_rx) = async_channel::bounded(1);
    let (stop_release_tx, stop_release_rx) = async_channel::bounded(1);
    let mut extensions = ExtensionRegistryBuilder::<Config>::new();
    extensions.turn_lifecycle_contributor(Arc::new(BlockFirstTurnStop {
        blocked: AtomicBool::new(false),
        entered_tx: stop_entered_tx,
        release_rx: stop_release_rx,
    }));
    let (server, _completions) = start_streaming_sse_server(vec![
        response_chunks("resp-first", "msg-first", "first answer"),
        response_chunks("resp-second", "msg-second", "second answer"),
    ])
    .await;
    let codex = test_codex()
        .with_model("gpt-5.4")
        .with_extensions(Arc::new(extensions.build()))
        .build_with_streaming_server(&server)
        .await
        .expect("build streaming Codex test session")
        .codex;

    submit_user_input(&codex, "first prompt").await;
    wait_for_event(&codex, |event| {
        matches!(
            event,
            EventMsg::AgentMessage(message) if message.message == "first answer"
        )
    })
    .await;
    tokio::time::timeout(Duration::from_secs(2), stop_entered_rx.recv())
        .await
        .expect("first turn should enter terminal transition")
        .expect("turn-stop observer should remain open");

    submit_user_input(&codex, "second prompt").await;
    assert!(
        tokio::time::timeout(
            Duration::from_millis(100),
            server.wait_for_request_count(/*count*/ 2),
        )
        .await
        .is_err(),
        "fresh submit must not start while the prior turn is transitioning"
    );

    stop_release_tx
        .send(())
        .await
        .expect("turn-stop hook should still be waiting");
    let mut lifecycle = Vec::new();
    while lifecycle.len() < 2 {
        let event = tokio::time::timeout(Duration::from_secs(2), codex.next_event())
            .await
            .expect("lifecycle event should arrive")
            .expect("event channel should remain open");
        match event.msg {
            EventMsg::TurnComplete(event) => {
                lifecycle.push(("complete", event.turn_id));
            }
            EventMsg::TurnStarted(event) => {
                lifecycle.push(("started", event.turn_id));
            }
            _ => {}
        }
    }
    assert_eq!(lifecycle[0].0, "complete");
    assert_eq!(lifecycle[1].0, "started");
    assert_ne!(lifecycle[0].1, lifecycle[1].1);

    wait_for_event(&codex, |event| {
        matches!(
            event,
            EventMsg::AgentMessage(message) if message.message == "second answer"
        )
    })
    .await;
    wait_for_event(&codex, |event| matches!(event, EventMsg::TurnComplete(_))).await;
    server.wait_for_request_count(/*count*/ 2).await;
    let requests = server.requests().await;
    assert_eq!(requests.len(), 2);
    let first_request: Value = from_slice(&requests[0]).expect("parse first request");
    let second_request: Value = from_slice(&requests[1]).expect("parse second request");
    assert!(
        !message_input_texts(&first_request, "user")
            .iter()
            .any(|text| text == "second prompt")
    );
    assert_eq!(
        message_input_texts(&second_request, "user")
            .iter()
            .filter(|text| text.as_str() == "second prompt")
            .count(),
        1
    );

    server.shutdown().await;
}
