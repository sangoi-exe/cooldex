use anyhow::Result;
use codex_core::TurnInput;
use codex_core::TurnInputRequest;
use codex_core::TurnInputSubmission;
use codex_core::UserMessageAdmission;
use codex_history::RolloutItem;
use codex_history::RolloutLine;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::CodexErrorInfo;
use codex_protocol::protocol::EventMsg;
use codex_protocol::user_input::UserInput;
use core_test_support::responses;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::start_mock_server;
use core_test_support::streaming_sse::StreamingSseChunk;
use core_test_support::streaming_sse::start_streaming_sse_server;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event_match;
use pretty_assertions::assert_eq;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;
use tokio::time::timeout;

fn persisted_user_input_request(text: &str, client_id: &str) -> TurnInputRequest {
    TurnInputRequest::new(TurnInput::UserInput {
        content: vec![UserInput::Text {
            text: text.to_string(),
            text_elements: Vec::new(),
        }],
        client_id: Some(client_id.to_string()),
    })
}

async fn persisted_user_message_texts(codex: &codex_core::CodexThread) -> Result<Vec<String>> {
    let rollout_path = codex.rollout_path().expect("user-message rollout path");
    let rollout = tokio::fs::read_to_string(rollout_path).await?;
    let mut texts = rollout
        .lines()
        .map(serde_json::from_str::<RolloutLine>)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter_map(|line| match line.item {
            RolloutItem::ResponseItem(response_item) => match response_item.item {
                ResponseItem::Message { role, content, .. } if role == "user" => {
                    let text = content
                        .into_iter()
                        .filter_map(|item| match item {
                            ContentItem::InputText { text } => Some(text),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    (!text.starts_with("<environment_context>")).then_some(text)
                }
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();
    texts.sort();
    Ok(texts)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn user_message_admission_reports_started_after_persistence() -> Result<()> {
    let server = start_mock_server().await;
    responses::mount_sse_once(&server, responses::sse_completed("resp-1")).await;
    let test = test_codex().build_with_auto_env(&server).await?;

    let admission = test
        .codex
        .submit_user_input_and_wait_for_persisted_admission(persisted_user_input_request(
            "first message",
            "client-message-1",
        ))
        .await
        .map_err(codex_protocol::error::CodexErr::from)?;
    assert!(matches!(admission, UserMessageAdmission::Started { .. }));

    assert_eq!(
        persisted_user_message_texts(test.codex.as_ref()).await?,
        vec!["first message".to_string()]
    );
    wait_for_event_match(test.codex.as_ref(), |event| {
        matches!(event, EventMsg::TurnComplete(_)).then_some(())
    })
    .await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn user_message_admission_reports_steered_after_persistence() -> Result<()> {
    let (release_response, response_gate) = oneshot::channel();
    let (server, _completions) = start_streaming_sse_server(vec![
        vec![
            StreamingSseChunk {
                gate: None,
                body: responses::sse(vec![ev_response_created("resp-1")]),
            },
            StreamingSseChunk {
                gate: Some(response_gate),
                body: responses::sse(vec![ev_completed("resp-1")]),
            },
        ],
        vec![StreamingSseChunk {
            gate: None,
            body: responses::sse(vec![ev_response_created("resp-2"), ev_completed("resp-2")]),
        }],
    ])
    .await;
    let test = test_codex()
        .with_model("gpt-5.4")
        .build_with_streaming_server(&server)
        .await?;

    let active_turn_id = match test
        .codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "first message".to_string(),
            text_elements: Vec::new(),
        }]))
        .await?
    {
        TurnInputSubmission::Started { turn_id } => turn_id,
        other => panic!("expected initial turn start, got {other:?}"),
    };
    timeout(
        Duration::from_secs(5),
        server.wait_for_request_count(/*count*/ 1),
    )
    .await?;

    let steer_submission = tokio::spawn({
        let codex = Arc::clone(&test.codex);
        async move {
            codex
                .submit_user_input_and_wait_for_persisted_admission(persisted_user_input_request(
                    "follow-up while running",
                    "client-message-2",
                ))
                .await
        }
    });
    tokio::task::yield_now().await;
    assert!(
        !steer_submission.is_finished(),
        "persisted steer should wait for the active turn to persist the message"
    );

    release_response
        .send(())
        .expect("response gate should remain open");
    let admission = timeout(Duration::from_secs(5), steer_submission)
        .await
        .expect("steered admission should resolve once persistence succeeds")?
        .map_err(codex_protocol::error::CodexErr::from)?;
    assert_eq!(
        admission,
        UserMessageAdmission::Steered {
            turn_id: active_turn_id.clone(),
        }
    );
    assert_eq!(
        persisted_user_message_texts(test.codex.as_ref()).await?,
        vec![
            "first message".to_string(),
            "follow-up while running".to_string(),
        ]
    );
    wait_for_event_match(test.codex.as_ref(), |event| {
        matches!(event, EventMsg::TurnComplete(_)).then_some(())
    })
    .await;

    let requests = server.requests().await;
    assert_eq!(requests.len(), 2);
    let second_request = String::from_utf8(requests[1].clone())?;
    assert!(second_request.contains("follow-up while running"));

    server.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn user_message_admission_reports_invalid_settings_and_recovers() -> Result<()> {
    let server = start_mock_server().await;
    responses::mount_sse_once(&server, responses::sse_completed("resp-1")).await;
    let test = test_codex()
        .with_model("gpt-5.4")
        .with_config(|config| {
            config.permissions.approval_policy =
                codex_core::config::Constrained::allow_only(AskForApproval::OnRequest);
        })
        .build_with_auto_env(&server)
        .await?;
    let codex = &test.codex;
    let mut invalid_request =
        persisted_user_input_request("invalid approval policy", "invalid-client-message");
    invalid_request.thread_settings.approval_policy = Some(AskForApproval::Never);

    let error = timeout(
        Duration::from_secs(5),
        codex.submit_user_input_and_wait_for_admission(invalid_request),
    )
    .await
    .expect("handler-side rejection should resolve promptly")
    .expect_err("disallowed approval settings should not be admitted");
    assert!(matches!(
        error.details(),
        CodexErrorDetails::InvalidRequest(_)
    ));

    let error_event = loop {
        let event = timeout(Duration::from_secs(5), codex.next_event())
            .await
            .expect("invalid settings should emit an error event promptly")?;
        match event.msg {
            EventMsg::Error(error) => break error,
            EventMsg::TurnComplete(_) => {
                panic!("invalid settings completed a turn without emitting an error")
            }
            EventMsg::StreamError(error) => {
                panic!("invalid settings unexpectedly started a model stream: {error:?}")
            }
            _ => {}
        }
    };
    assert_eq!(error_event.codex_error_info, Some(CodexErrorInfo::Other));

    let recovered = timeout(
        Duration::from_secs(5),
        codex.submit_user_input_and_wait_for_admission(persisted_user_input_request(
            "valid message after rejected settings",
            "valid-client-message",
        )),
    )
    .await
    .expect("session should accept a valid message after rejecting invalid settings")?;
    assert!(matches!(recovered, UserMessageAdmission::Started { .. }));

    loop {
        let event = timeout(Duration::from_secs(10), codex.next_event())
            .await
            .expect("recovered turn should finish promptly")?;
        match event.msg {
            EventMsg::Error(error) => panic!("settings rejection emitted another error: {error:?}"),
            EventMsg::StreamError(error) => panic!("recovered turn stream failed: {error:?}"),
            EventMsg::TurnComplete(_) => break,
            _ => {}
        }
    }

    Ok(())
}

#[tokio::test]
async fn user_message_admission_fails_promptly_after_session_shutdown() -> Result<()> {
    let server = start_mock_server().await;
    let test = test_codex()
        .with_model("gpt-5.4")
        .build_with_auto_env(&server)
        .await?;
    let codex = &test.codex;
    codex.shutdown_and_wait().await?;

    let error = timeout(
        Duration::from_secs(5),
        codex.submit_user_input_and_wait_for_admission(persisted_user_input_request(
            "after shutdown",
            "client-message-1",
        )),
    )
    .await
    .expect("closed session should reject admission promptly")
    .expect_err("closed session should not accept user messages");
    assert!(matches!(
        error.details(),
        CodexErrorDetails::InternalAgentDied
    ));

    Ok(())
}
