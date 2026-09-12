// Merge-safety anchor: pre-compaction synthesis tests keep the handoff operation
// isolated from live history, normal continuation state, and recovery-state mutation.
use super::*;
use crate::ResponseStream;
use crate::client_common::ResponseEvent;
use crate::context::ContextualUserFragment;
use crate::session::pre_compact_handoff_input_snapshot_from_parts;
use crate::session::tests::make_session_and_context;
use crate::session::tests::make_session_and_context_with_auth_and_config_and_rx;
use crate::state::PostCompactRecoveryIdentity;
use crate::state::PostCompactRecoveryRuntimeState;
use codex_login::CodexAuth;
use codex_protocol::ResponseItemId;
use codex_protocol::error::CodexErr;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use pretty_assertions::assert_eq;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

fn message(id: Option<ResponseItemId>, role: &str, text: &str) -> ResponseItem {
    ResponseItem::Message {
        id,
        role: role.to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

fn output_message(role: &str, text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: role.to_string(),
        content: vec![ContentItem::OutputText {
            text: text.to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

fn assistant_message(text: &str) -> ResponseItem {
    output_message("assistant", text)
}

fn reasoning_item() -> ResponseItem {
    ResponseItem::Reasoning {
        id: None,
        summary: Vec::new(),
        content: None,
        encrypted_content: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

fn response_stream(events: Vec<Result<ResponseEvent, CodexErr>>) -> ResponseStream {
    let (tx, rx) = mpsc::channel(events.len().max(1));
    for event in events {
        tx.try_send(event)
            .expect("response stream test channel should have capacity");
    }
    drop(tx);
    ResponseStream {
        rx_event: rx,
        consumer_dropped: CancellationToken::new(),
    }
}

fn completed() -> ResponseEvent {
    ResponseEvent::Completed {
        response_id: "resp-handoff".to_string(),
        token_usage: None,
        usage_metadata: None,
        end_turn: Some(true),
    }
}

#[tokio::test]
async fn snapshot_preserves_admitted_history_base_instructions_and_source_identity() {
    let (session, turn_context, _events) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| config.base_instructions = Some("current base sentinel".to_string()),
    )
    .await;
    session
        .record_conversation_items(
            turn_context.as_ref(),
            &[message(None, "user", "pre-compaction history sentinel")],
        )
        .await;

    let step_context = crate::session::step_context::StepContext::for_test(Arc::clone(&turn_context));
    let settings = PreCompactHandoffSettings::from_step_context(&step_context);
    let source = PreCompactHandoffSource::from_snapshot(
        session
            .snapshot_pre_compact_handoff_input(&settings.model_info)
            .await
            .expect("snapshot"),
        settings.clone(),
    );

    assert_eq!(source.base_instructions.text, "current base sentinel");
    assert!(source.input.iter().any(|item| {
        matches!(item, ResponseItem::Message { role, content, .. }
            if role == "user" && content.iter().any(|content| {
                matches!(content, ContentItem::InputText { text }
                    if text == "pre-compaction history sentinel")
            }))
    }));
    assert!(!source.input.iter().any(|item| {
        matches!(item, ResponseItem::Message { content, .. }
            if content.iter().any(|content| {
                matches!(content, ContentItem::InputText { text }
                    if text == "unadmitted incoming input")
            }))
    }));
    assert!(source.is_current_for(&session, &settings).await.expect("source match"));

    session
        .record_conversation_items(
            turn_context.as_ref(),
            &[message(None, "user", "newly admitted input")],
        )
        .await;
    assert!(
        !source
            .is_current_for(&session, &settings)
            .await
            .expect("changed history must invalidate source")
    );
}

#[tokio::test]
async fn cached_recovery_packet_is_read_only_overlay_at_the_existing_boundary() {
    let identity = PostCompactRecoveryIdentity {
        compaction_window_id: "window-handoff".to_string(),
        boundary_item_id: "msg-boundary-handoff".to_string(),
    };
    let input = vec![message(
        Some(ResponseItemId::from_server(identity.boundary_item_id.clone())),
        "user",
        "recovery boundary",
    )];
    let packet = crate::context::PostCompactRecoveryContext::new(
        &identity.compaction_window_id,
        &identity.boundary_item_id,
        "fixed recovery boundary",
        None,
    )
    .expect("recovery packet");
    let mut state = PostCompactRecoveryRuntimeState::pending(identity.clone());
    state
        .cache_packet(&identity, packet.clone())
        .expect("cache packet");
    let recovery_packet = state
        .pending_packet_snapshot()
        .expect("cached recovery packet snapshot");
    let snapshot = pre_compact_handoff_input_snapshot_from_parts(
        input,
        BaseInstructions {
            text: "base instructions".to_string(),
            provenance: None,
        },
        recovery_packet,
    )
    .expect("snapshot with cached recovery overlay");
    let boundary_index = snapshot
        .input
        .iter()
        .position(|item| item.id().is_some_and(|id| id.as_str() == identity.boundary_item_id))
        .expect("boundary in snapshot");
    let ResponseItem::Message { role, content, .. } = &snapshot.input[boundary_index + 1] else {
        panic!("recovery boundary must be inserted immediately after a non-tool boundary");
    };
    assert_eq!(role, "developer");
    assert!(content.iter().any(|content| {
        matches!(content, ContentItem::InputText { text }
            if crate::context::PostCompactRecoveryContext::matches_text(text))
    }));

    assert_eq!(state.pending_identity(), Some(&identity));
    assert_eq!(
        state
            .packet(&identity)
            .expect("matching packet read"),
        Some(packet)
    );
}

#[tokio::test]
async fn pending_identity_without_cached_packet_neither_loads_recall_nor_mutates_state() {
    let identity = PostCompactRecoveryIdentity {
        compaction_window_id: "window-missing-packet".to_string(),
        boundary_item_id: "msg-boundary-missing-packet".to_string(),
    };
    let state = PostCompactRecoveryRuntimeState::pending(identity.clone());
    let state_before = state.clone();
    let snapshot = pre_compact_handoff_input_snapshot_from_parts(
        vec![message(
            Some(ResponseItemId::from_server(identity.boundary_item_id.clone())),
            "user",
            "recovery boundary",
        )],
        BaseInstructions {
            text: "base instructions".to_string(),
            provenance: None,
        },
        state
            .pending_packet_snapshot()
            .expect("missing packet must remain a pure read"),
    )
    .expect("missing cached packet must not fail snapshot");
    assert!(!snapshot.input.iter().any(|item| {
        matches!(item, ResponseItem::Message { content, .. }
            if content.iter().any(|content| {
                matches!(content, ContentItem::InputText { text }
                    if crate::context::PostCompactRecoveryContext::matches_text(text))
            }))
    }));

    assert_eq!(state, state_before);
    assert_eq!(state.pending_identity(), Some(&identity));
    assert_eq!(
        state
            .packet(&identity)
            .expect("matching packet read"),
        None
    );
}

#[tokio::test]
async fn synthesis_prompt_is_tool_free_bounded_and_uses_frozen_settings() {
    let (session, turn_context) = make_session_and_context().await;
    let turn_context = Arc::new(turn_context);
    let step_context = crate::session::step_context::StepContext::for_test(Arc::clone(&turn_context));
    let settings = PreCompactHandoffSettings::from_step_context(&step_context);
    let source = PreCompactHandoffSource::from_snapshot(
        session
            .snapshot_pre_compact_handoff_input(&settings.model_info)
            .await
            .expect("snapshot"),
        settings.clone(),
    );
    let prompt = source.synthesis_prompt();

    assert!(prompt.tools.is_empty());
    assert!(!prompt.parallel_tool_calls);
    assert_eq!(
        prompt.max_output_tokens,
        Some(PRE_COMPACT_HANDOFF_MAX_OUTPUT_TOKENS)
    );
    assert_eq!(source.settings, settings);
    assert!(prompt.input.last().is_some_and(|item| {
        matches!(item, ResponseItem::Message { role, content, .. }
            if role == "developer" && content.iter().any(|content| {
                matches!(content, ContentItem::InputText { text }
                    if text == PRE_COMPACT_HANDOFF_INSTRUCTIONS)
            }))
    }));
    let metadata = session
        .responses_metadata(
            &turn_context,
            crate::responses_metadata::CodexResponsesRequestKind::PreCompactHandoff,
        )
        .await;
    let metadata: serde_json::Value = serde_json::from_str(
        &metadata.turn_metadata_json().expect("request metadata"),
    )
    .expect("metadata JSON");
    assert_eq!(
        metadata["request_kind"].as_str(),
        Some("pre_compact_handoff")
    );
}

#[tokio::test]
async fn collector_combines_assistant_text_only_after_completed() {
    let result = collect_handoff_response(
        response_stream(vec![
            Ok(ResponseEvent::OutputItemDone(assistant_message("first "))),
            Ok(ResponseEvent::OutputItemDone(assistant_message("second"))),
            Ok(completed()),
        ]),
        &CancellationToken::new(),
        |_| {},
    )
    .await
    .expect("collector transport");

    assert_eq!(result, Ok("first second".to_string()));
}

#[tokio::test]
async fn collector_ignores_reasoning_before_assistant_text() {
    let reasoning = reasoning_item();
    let result = collect_handoff_response(
        response_stream(vec![
            Ok(ResponseEvent::OutputItemAdded(reasoning.clone())),
            Ok(ResponseEvent::ReasoningSummaryDelta {
                delta: "internal reasoning".to_string(),
                summary_index: 0,
            }),
            Ok(ResponseEvent::ReasoningSummaryDone {
                item_id: "reasoning-handoff".to_string(),
                text: "internal reasoning complete".to_string(),
                summary_index: 0,
            }),
            Ok(ResponseEvent::OutputItemDone(reasoning)),
            Ok(ResponseEvent::OutputItemDone(assistant_message("handoff only"))),
            Ok(completed()),
        ]),
        &CancellationToken::new(),
        |_| {},
    )
    .await
    .expect("collector transport");

    assert_eq!(result, Ok("handoff only".to_string()));
}

#[tokio::test]
async fn collector_discards_all_partial_text_for_invalid_or_incomplete_output() {
    let tool = ResponseItem::FunctionCall {
        id: None,
        name: "shell".to_string(),
        namespace: None,
        arguments: "{}".to_string(),
        encrypted_function_args: None,
        call_id: "call-handoff".to_string(),
        internal_chat_message_metadata_passthrough: None,
    };
    let unexpected = ResponseItem::AdditionalTools {
        id: None,
        role: "developer".to_string(),
        tools: Vec::new(),
    };
    let oversized = assistant_message(&vec!["word"; PRE_COMPACT_HANDOFF_CARRIER_MAX_TOKENS + 100].join(" "));
    let cases = vec![
        (
            response_stream(vec![
                Ok(ResponseEvent::OutputItemDone(assistant_message("partial"))),
                Ok(ResponseEvent::OutputItemDone(tool)),
            ]),
            PreCompactHandoffFailure::UnexpectedOutput,
        ),
        (
            response_stream(vec![Ok(ResponseEvent::OutputItemDone(unexpected))]),
            PreCompactHandoffFailure::UnexpectedOutput,
        ),
        (
            response_stream(vec![Ok(ResponseEvent::OutputItemDone(output_message(
                "developer",
                "wrong role",
            )))]),
            PreCompactHandoffFailure::UnexpectedOutput,
        ),
        (
            response_stream(vec![
                Ok(ResponseEvent::OutputItemDone(assistant_message("partial"))),
                Ok(ResponseEvent::ToolCallInputDelta {
                    item_id: "item-handoff".to_string(),
                    call_id: Some("call-handoff".to_string()),
                    delta: "{}".to_string(),
                }),
            ]),
            PreCompactHandoffFailure::UnexpectedOutput,
        ),
        (
            response_stream(vec![
                Ok(ResponseEvent::OutputItemDone(assistant_message(""))),
                Ok(completed()),
            ]),
            PreCompactHandoffFailure::EmptyOutput,
        ),
        (
            response_stream(vec![Ok(ResponseEvent::OutputItemDone(assistant_message(
                "partial before EOF",
            )))]),
            PreCompactHandoffFailure::StreamEnded,
        ),
        (
            response_stream(vec![Err(CodexErr::Stream("request failed".to_string()))]),
            PreCompactHandoffFailure::RequestFailed,
        ),
        (
            response_stream(vec![Err(CodexErr::ContextWindowExceeded)]),
            PreCompactHandoffFailure::ContextWindowExceeded,
        ),
        (
            response_stream(vec![
                Ok(ResponseEvent::OutputItemDone(oversized)),
                Ok(completed()),
            ]),
            PreCompactHandoffFailure::CarrierBudgetExceeded,
        ),
    ];

    for (stream, expected) in cases {
        assert_eq!(
            collect_handoff_response(stream, &CancellationToken::new(), |_| {})
                .await
                .expect("ordinary collector failures degrade"),
            Err(expected)
        );
    }
}

#[tokio::test]
async fn collector_propagates_cancellation_as_turn_aborted() {
    let (_tx, rx) = mpsc::channel(1);
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let error = collect_handoff_response(
        ResponseStream {
            rx_event: rx,
            consumer_dropped: CancellationToken::new(),
        },
        &cancellation,
        |_| {},
    )
    .await
    .expect_err("cancellation must not degrade");

    assert!(matches!(error.details(), CodexErrorDetails::TurnAborted));
}

#[tokio::test]
async fn preparation_propagates_cancellation_before_hidden_inference() {
    let (session, turn_context) = make_session_and_context().await;
    let turn_context = Arc::new(turn_context);
    let step_context = crate::session::step_context::StepContext::for_test(Arc::clone(&turn_context));
    let settings = PreCompactHandoffSettings::from_step_context(&step_context);
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    let error = prepare_pre_compact_handoff(
        &session,
        turn_context.as_ref(),
        settings,
        &turn_context.session_telemetry,
        &cancellation,
    )
    .await
    .expect_err("cancellation must not degrade to a no-handoff outcome");

    assert!(matches!(error.details(), CodexErrorDetails::TurnAborted));
}

#[tokio::test]
async fn no_live_thread_prepares_without_inference_or_history_occupancy_change() {
    let (session, turn_context) = make_session_and_context().await;
    let turn_context = Arc::new(turn_context);
    session
        .record_conversation_items(
            turn_context.as_ref(),
            &[message(None, "user", "history sentinel")],
        )
        .await;
    let history_before = session
        .clone_history()
        .await
        .raw_items()
        .cloned()
        .collect::<Vec<_>>();
    let token_usage_before = session.token_usage_info().await;
    let step_context = crate::session::step_context::StepContext::for_test(Arc::clone(&turn_context));
    let settings = PreCompactHandoffSettings::from_step_context(&step_context);

    let prepared = prepare_pre_compact_handoff(
        &session,
        turn_context.as_ref(),
        settings,
        &turn_context.session_telemetry,
        &CancellationToken::new(),
    )
    .await
    .expect("persistence-disabled session must retain upstream behavior");

    assert_eq!(
        prepared.outcome(),
        &PreCompactHandoffOutcome::UnsupportedNoLiveThread
    );
    assert_eq!(prepared.handoff_text(), None);
    assert_eq!(
        session
            .clone_history()
            .await
            .raw_items()
            .cloned()
            .collect::<Vec<_>>(),
        history_before
    );
    assert_eq!(session.token_usage_info().await, token_usage_before);
}
