use anyhow::Result;
use codex_core::compact::SUMMARIZATION_PROMPT;
use codex_model_provider_info::built_in_model_providers;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::Value;

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
