use anyhow::Result;
use codex_core::compact::SUMMARIZATION_PROMPT;
use codex_features::Feature;
use codex_model_provider_info::built_in_model_providers;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ResponseMock;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_function_call_with_namespace;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once_match;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::time::Duration;
use test_case::test_case;
use tokio::time::Instant;
use tokio::time::sleep;

const CHILD_PROMPT: &str = "child: inspect only your own rollout with recall";
const CHILD_RECALL_CALL_ID: &str = "child-recall-call";
const SPAWN_CALL_ID: &str = "spawn-recall-child";
const SPAWN_PROMPT: &str = "spawn the recall locality child";

fn decoded_body(request: &wiremock::Request) -> Option<Vec<u8>> {
    let is_zstd = request
        .headers
        .get("content-encoding")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .any(|entry| entry.trim().eq_ignore_ascii_case("zstd"))
        });
    if is_zstd {
        zstd::stream::decode_all(std::io::Cursor::new(&request.body)).ok()
    } else {
        Some(request.body.clone())
    }
}

fn body_contains(request: &wiremock::Request, text: &str) -> bool {
    decoded_body(request)
        .and_then(|body| String::from_utf8(body).ok())
        .is_some_and(|body| body.contains(text))
}

async fn wait_for_recall_output_request(mock: &ResponseMock) -> Result<String> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(output) = mock
            .requests()
            .into_iter()
            .find_map(|request| request.function_call_output_text(CHILD_RECALL_CALL_ID))
        {
            return Ok(output);
        }
        if Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for child recall output");
        }
        sleep(Duration::from_millis(10)).await;
    }
}

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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recall_returns_an_inert_current_thread_tool_result() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let requests = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_assistant_message("old-answer", "answer before compaction"),
                ev_completed("response-1"),
            ]),
            sse(vec![
                ev_assistant_message("summary", "compacted summary"),
                ev_completed("response-2"),
            ]),
            sse(vec![
                ev_function_call("recall-call", "recall", "{}"),
                ev_completed("response-3"),
            ]),
            sse(vec![
                ev_assistant_message("done", "recalled"),
                ev_completed("response-4"),
            ]),
        ],
    )
    .await;
    let mut provider = built_in_model_providers(/*openai_base_url*/ None)["openai"].clone();
    provider.name = "OpenAI (recall test)".to_string();
    provider.base_url = Some(format!("{}/v1", server.uri()));
    provider.supports_websockets = false;
    let mut builder = test_codex().with_config(move |config| {
        config.compact_prompt = Some(SUMMARIZATION_PROMPT.to_string());
        config.model_provider = provider;
    });
    let test = builder.build_with_auto_env(&server).await?;

    test.codex
        .submit(user_turn("question before compaction"))
        .await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    test.codex.submit(Op::Compact).await?;
    wait_for_event(&test.codex, |event| matches!(event, EventMsg::Warning(_))).await;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    test.codex.submit(user_turn("use recall")).await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    let captured = requests.requests();
    assert_eq!(captured.len(), 4);
    let output = captured[3]
        .function_call_output_text("recall-call")
        .expect("recall function output");
    let value: Value = serde_json::from_str(&output)?;
    assert_eq!(value["availability"], "available");
    assert_eq!(
        value["thread_id"],
        test.session_configured.thread_id.to_string()
    );
    assert!(value["source"]["reached_start"].as_bool().unwrap_or(false));
    assert_eq!(value["source"]["segments_read"], 1);
    assert!(
        value["groups"]
            .as_array()
            .is_some_and(|groups| !groups.is_empty())
    );
    assert!(!output.contains("<system>"));
    assert!(!output.contains("<developer>"));

    Ok(())
}

#[test_case("all", "available"; "full history")]
#[test_case("1", "no_compaction"; "partial history")]
#[test_case("none", "no_compaction"; "no history")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn child_recall_reads_only_the_rollout_owned_by_its_history_mode(
    fork_turns: &str,
    expected_availability: &str,
) -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let parent_setup = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_assistant_message("old-answer", "answer before parent compaction"),
                ev_completed("response-1"),
            ]),
            sse(vec![
                ev_assistant_message("summary", "parent compacted summary"),
                ev_completed("response-2"),
            ]),
        ],
    )
    .await;
    let spawn_args = serde_json::to_string(&json!({
        "message": CHILD_PROMPT,
        "task_name": format!("recall_{fork_turns}"),
        "fork_turns": fork_turns,
    }))?;
    let _spawn_request = mount_sse_once_match(
        &server,
        |request: &wiremock::Request| body_contains(request, SPAWN_PROMPT),
        sse(vec![
            ev_response_created("spawn-response"),
            ev_function_call_with_namespace(
                SPAWN_CALL_ID,
                "collaboration",
                "spawn_agent",
                &spawn_args,
            ),
            ev_completed("spawn-response"),
        ]),
    )
    .await;
    let _child_recall_request = mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, CHILD_PROMPT) && !body_contains(request, SPAWN_CALL_ID)
        },
        sse(vec![
            ev_response_created("child-recall-response"),
            ev_function_call(CHILD_RECALL_CALL_ID, "recall", "{}"),
            ev_completed("child-recall-response"),
        ]),
    )
    .await;
    let child_recall_output = mount_sse_once_match(
        &server,
        |request: &wiremock::Request| body_contains(request, CHILD_RECALL_CALL_ID),
        sse(vec![
            ev_response_created("child-done-response"),
            ev_assistant_message("child-done", "child recall complete"),
            ev_completed("child-done-response"),
        ]),
    )
    .await;
    let _parent_followup = mount_sse_once_match(
        &server,
        |request: &wiremock::Request| body_contains(request, SPAWN_CALL_ID),
        sse(vec![
            ev_response_created("parent-done-response"),
            ev_assistant_message("parent-done", "parent spawn complete"),
            ev_completed("parent-done-response"),
        ]),
    )
    .await;

    let mut provider = built_in_model_providers(/*openai_base_url*/ None)["openai"].clone();
    provider.name = "OpenAI (child recall locality test)".to_string();
    provider.base_url = Some(format!("{}/v1", server.uri()));
    provider.supports_websockets = false;
    let mut builder = test_codex().with_config(move |config| {
        config.compact_prompt = Some(SUMMARIZATION_PROMPT.to_string());
        config.model_provider = provider;
        config
            .features
            .enable(Feature::Collab)
            .expect("test config should allow feature update");
        config
            .features
            .enable(Feature::MultiAgentV2)
            .expect("test config should allow feature update");
    });
    let test = builder.build_with_auto_env(&server).await?;
    let parent_thread_id = test.session_configured.thread_id;

    test.codex
        .submit(user_turn("question before parent compaction"))
        .await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    test.codex.submit(Op::Compact).await?;
    wait_for_event(&test.codex, |event| matches!(event, EventMsg::Warning(_))).await;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    test.submit_turn(SPAWN_PROMPT).await?;

    let output = wait_for_recall_output_request(&child_recall_output).await?;
    let value: Value = serde_json::from_str(&output)?;
    assert_eq!(value["availability"], expected_availability);
    let child_thread_id = value["thread_id"]
        .as_str()
        .expect("recall output should name its current thread");
    assert_ne!(child_thread_id, parent_thread_id.to_string());
    assert!(
        test.thread_manager
            .list_thread_ids()
            .await
            .iter()
            .any(|thread_id| thread_id.to_string() == child_thread_id)
    );
    assert_eq!(parent_setup.requests().len(), 2);
    if expected_availability == "available" {
        assert_eq!(value["boundary"]["kind"], "replacement_history");
        assert_eq!(value["source"]["reached_recall_origin"], true);
    } else {
        assert!(value["boundary"].is_null());
        assert_eq!(value["source"]["reached_recall_origin"], false);
    }

    Ok(())
}
