use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::CompactedItem;
use codex_protocol::protocol::ErrorEvent;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::PostCompactRecoveryAppliedItem;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::ThreadRolledBackEvent;
use codex_protocol::protocol::TurnCompleteEvent;
use codex_protocol::protocol::TurnStartedEvent;
use codex_protocol::protocol::UserMessageEvent;
use codex_thread_store::StoredRolloutTail;
use pretty_assertions::assert_eq;
use serde_json::Value;

use super::*;
use crate::session::tests::make_session_and_context;

fn message(role: &str, text: &str) -> ResponseItem {
    let content = if role == "assistant" {
        ContentItem::OutputText {
            text: text.to_string(),
        }
    } else {
        ContentItem::InputText {
            text: text.to_string(),
        }
    };
    ResponseItem::Message {
        id: None,
        role: role.to_string(),
        content: vec![content],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

fn compacted(message: &str, replacement_history: Option<Vec<ResponseItem>>) -> RolloutItem {
    RolloutItem::Compacted(CompactedItem {
        message: message.to_string(),
        replacement_history,
        window_number: Some(1),
        first_window_id: None,
        previous_window_id: None,
        window_id: None,
        post_compact_recovery: None,
    })
}

fn compacted_window(
    message: &str,
    replacement_history: Vec<ResponseItem>,
    window_number: u64,
    first_window_id: &str,
    previous_window_id: Option<&str>,
    window_id: &str,
) -> RolloutItem {
    RolloutItem::Compacted(CompactedItem {
        message: message.to_string(),
        replacement_history: Some(replacement_history),
        window_number: Some(window_number),
        first_window_id: Some(first_window_id.to_string()),
        previous_window_id: previous_window_id.map(ToString::to_string),
        window_id: Some(window_id.to_string()),
        post_compact_recovery: None,
    })
}

fn tail(thread_id: codex_protocol::ThreadId, items: Vec<RolloutItem>) -> StoredRolloutTail {
    StoredRolloutTail {
        thread_id,
        items,
        reached_start: true,
        bytes_read: 1_024,
        records_read: 8,
        segments_read: 1,
    }
}

fn parsed(context: &crate::context::RecallContext) -> Value {
    serde_json::from_str(context.json()).expect("parse recall JSON")
}

fn turn_started(turn_id: &str) -> RolloutItem {
    RolloutItem::EventMsg(EventMsg::TurnStarted(TurnStartedEvent {
        turn_id: turn_id.to_string(),
        trace_id: None,
        started_at: None,
        model_context_window: Some(128_000),
        collaboration_mode_kind: codex_protocol::config_types::ModeKind::Default,
    }))
}

fn user_event(text: &str) -> RolloutItem {
    RolloutItem::EventMsg(EventMsg::UserMessage(UserMessageEvent {
        client_id: None,
        message: text.to_string(),
        images: None,
        local_images: Vec::new(),
        text_elements: Vec::new(),
        ..Default::default()
    }))
}

fn turn_complete(turn_id: &str) -> RolloutItem {
    RolloutItem::EventMsg(EventMsg::TurnComplete(TurnCompleteEvent {
        turn_id: turn_id.to_string(),
        last_agent_message: None,
        error: None,
        started_at: None,
        completed_at: None,
        duration_ms: None,
        time_to_first_token_ms: None,
    }))
}

#[tokio::test]
async fn returns_paired_chronological_groups_before_the_surviving_boundary() {
    let (session, turn_context) = make_session_and_context().await;
    let call = ResponseItem::FunctionCall {
        id: None,
        name: "shell".to_string(),
        namespace: None,
        arguments: "{}".to_string(),
        call_id: "call-1".to_string(),
        internal_chat_message_metadata_passthrough: None,
    };
    let output = ResponseItem::FunctionCallOutput {
        id: None,
        call_id: "call-1".to_string(),
        output: FunctionCallOutputPayload::from_text("done".to_string()),
        internal_chat_message_metadata_passthrough: None,
    };
    let tool_search_call = ResponseItem::ToolSearchCall {
        id: None,
        call_id: Some("search-1".to_string()),
        status: Some("completed".to_string()),
        execution: "server".to_string(),
        arguments: serde_json::json!({"query": "context"}),
        internal_chat_message_metadata_passthrough: None,
    };
    let tool_search_output = ResponseItem::ToolSearchOutput {
        id: None,
        call_id: Some("search-1".to_string()),
        status: "completed".to_string(),
        execution: "server".to_string(),
        tools: vec![serde_json::json!({"name": "read_file"})],
        internal_chat_message_metadata_passthrough: None,
    };
    let context = session
        .build_recall_context(
            &turn_context,
            tail(
                session.thread_id,
                vec![
                    RolloutItem::ResponseItem(message("user", "old question")),
                    RolloutItem::ResponseItem(call),
                    RolloutItem::ResponseItem(output),
                    RolloutItem::ResponseItem(tool_search_call),
                    RolloutItem::ResponseItem(tool_search_output),
                    RolloutItem::ResponseItem(message("assistant", "old answer")),
                    compacted("summary", Some(Vec::new())),
                ],
            ),
        )
        .await
        .expect("build recall context");
    let value = parsed(&context);

    assert_eq!(value["availability"], "available");
    assert_eq!(value["boundary"]["rollout_item_index"], 6);
    assert_eq!(value["groups"].as_array().expect("groups").len(), 4);
    assert_eq!(
        value["groups"][1]["items"]
            .as_array()
            .expect("paired items")
            .len(),
        2
    );
    assert_eq!(
        value["groups"][2]["items"]
            .as_array()
            .expect("tool-search pair")
            .len(),
        2
    );
    assert_eq!(value["omitted_groups"], 0);
    assert_eq!(value["truncated"], false);
}

#[tokio::test]
async fn rollback_removes_the_newest_compaction_boundary() {
    let (session, turn_context) = make_session_and_context().await;
    let context = session
        .build_recall_context(
            &turn_context,
            tail(
                session.thread_id,
                vec![
                    turn_started("turn-1"),
                    user_event("first"),
                    RolloutItem::ResponseItem(message("assistant", "before older")),
                    compacted("older", Some(Vec::new())),
                    turn_complete("turn-1"),
                    turn_started("turn-2"),
                    user_event("second"),
                    RolloutItem::ResponseItem(message("assistant", "before latest")),
                    compacted("latest", Some(Vec::new())),
                    turn_complete("turn-2"),
                    RolloutItem::EventMsg(EventMsg::ThreadRolledBack(ThreadRolledBackEvent {
                        num_turns: 1,
                    })),
                ],
            ),
        )
        .await
        .expect("build rollback recall");
    let value = parsed(&context);

    assert_eq!(value["availability"], "available");
    assert_eq!(value["boundary"]["rollout_item_index"], 3);
    assert_eq!(value["groups"].as_array().expect("groups").len(), 1);
}

#[tokio::test]
async fn selects_the_latest_of_multiple_surviving_compactions() {
    let (session, turn_context) = make_session_and_context().await;
    let context = session
        .build_recall_context(
            &turn_context,
            tail(
                session.thread_id,
                vec![
                    RolloutItem::ResponseItem(message("assistant", "before first")),
                    compacted("first", Some(Vec::new())),
                    RolloutItem::ResponseItem(message("assistant", "before latest")),
                    compacted("latest", Some(Vec::new())),
                ],
            ),
        )
        .await
        .expect("build multiple-compaction recall");
    let value = parsed(&context);

    assert_eq!(value["availability"], "available");
    assert_eq!(value["boundary"]["rollout_item_index"], 3);
    assert_eq!(value["groups"].as_array().expect("groups").len(), 1);
    assert_eq!(
        value["groups"][0]["items"][0]["content"][0]["text"],
        "before latest"
    );
}

#[tokio::test]
async fn bounded_tail_reaches_previous_compaction_without_reaching_session_start() {
    const FIRST_WINDOW_ID: &str = "019b3f6e-7a10-7cc3-8b6e-1d09e2f7a001";
    const CURRENT_WINDOW_ID: &str = "019b3f6e-7a10-7cc3-8b6e-1d09e2f7a002";
    let (session, turn_context) = make_session_and_context().await;
    let call = ResponseItem::FunctionCall {
        id: None,
        name: "shell".to_string(),
        namespace: None,
        arguments: "{}".to_string(),
        call_id: "continuity-call".to_string(),
        internal_chat_message_metadata_passthrough: None,
    };
    let output = ResponseItem::FunctionCallOutput {
        id: None,
        call_id: "continuity-call".to_string(),
        output: FunctionCallOutputPayload::from_text("continuity output".to_string()),
        internal_chat_message_metadata_passthrough: None,
    };
    let mut stored_tail = tail(
        session.thread_id,
        vec![
            compacted_window(
                "first",
                Vec::new(),
                1,
                FIRST_WINDOW_ID,
                None,
                FIRST_WINDOW_ID,
            ),
            RolloutItem::ResponseItem(message("assistant", "between compactions")),
            RolloutItem::ResponseItem(call.clone()),
            RolloutItem::ResponseItem(output.clone()),
            compacted_window(
                "latest",
                vec![
                    ResponseItem::Compaction {
                        id: None,
                        encrypted_content: "opaque".to_string(),
                        internal_chat_message_metadata_passthrough: None,
                    },
                    call,
                    output,
                ],
                2,
                FIRST_WINDOW_ID,
                Some(FIRST_WINDOW_ID),
                CURRENT_WINDOW_ID,
            ),
        ],
    );
    stored_tail.reached_start = false;

    let context = session
        .build_recall_context(&turn_context, stored_tail)
        .await
        .expect("previous compaction should complete the bounded recall window");
    let value = parsed(&context);

    assert_eq!(value["availability"], "available");
    assert_eq!(value["source"]["reached_start"], false);
    assert_eq!(value["source"]["reached_recall_origin"], true);
    assert_eq!(value["excluded_native_continuity_pairs"], 1);
    assert_eq!(value["groups"].as_array().expect("groups").len(), 1);
    assert_eq!(
        value["groups"][0]["items"][0]["content"][0]["text"],
        "between compactions"
    );
}

#[tokio::test]
async fn bounded_tail_without_named_previous_compaction_reports_work_limit() {
    const FIRST_WINDOW_ID: &str = "019b3f6e-7a10-7cc3-8b6e-1d09e2f7a001";
    const CURRENT_WINDOW_ID: &str = "019b3f6e-7a10-7cc3-8b6e-1d09e2f7a002";
    let (session, turn_context) = make_session_and_context().await;
    let mut stored_tail = tail(
        session.thread_id,
        vec![
            RolloutItem::ResponseItem(message("assistant", "bounded suffix only")),
            compacted_window(
                "latest",
                Vec::new(),
                2,
                FIRST_WINDOW_ID,
                Some(FIRST_WINDOW_ID),
                CURRENT_WINDOW_ID,
            ),
        ],
    );
    stored_tail.reached_start = false;

    let context = session
        .build_recall_context(&turn_context, stored_tail)
        .await
        .expect("bounded source should report its incomplete origin");

    assert_eq!(parsed(&context)["availability"], "work_limit");
}

#[tokio::test]
async fn reports_unavailable_source_and_missing_compaction() {
    let (session, turn_context) = make_session_and_context().await;
    let mut incomplete = tail(session.thread_id, Vec::new());
    incomplete.reached_start = false;
    incomplete.bytes_read = RECALL_SOURCE_MAX_BYTES;
    let work_limit = session
        .build_recall_context(&turn_context, incomplete)
        .await
        .expect("work limit result");
    assert_eq!(parsed(&work_limit)["availability"], "work_limit");

    let no_compaction = session
        .build_recall_context(
            &turn_context,
            tail(
                session.thread_id,
                vec![RolloutItem::ResponseItem(message("user", "hello"))],
            ),
        )
        .await
        .expect("no compaction result");
    assert_eq!(parsed(&no_compaction)["availability"], "no_compaction");
}

#[tokio::test]
async fn accepts_legacy_boundary_after_complete_replay() {
    let (session, turn_context) = make_session_and_context().await;
    let context = session
        .build_recall_context(
            &turn_context,
            tail(
                session.thread_id,
                vec![
                    RolloutItem::ResponseItem(message("user", "legacy input")),
                    compacted("legacy summary", /*replacement_history*/ None),
                ],
            ),
        )
        .await
        .expect("legacy recall");
    let value = parsed(&context);

    assert_eq!(value["availability"], "available");
    assert_eq!(value["boundary"]["kind"], "legacy");
    assert_eq!(value["groups"].as_array().expect("groups").len(), 1);
}

#[tokio::test]
async fn rejects_legacy_boundary_when_chronology_spans_multiple_segments() {
    let (session, turn_context) = make_session_and_context().await;
    let mut stored_tail = tail(
        session.thread_id,
        vec![
            RolloutItem::ResponseItem(message("user", "legacy parent input")),
            compacted("legacy summary", /*replacement_history*/ None),
        ],
    );
    stored_tail.segments_read = 2;

    let context = session
        .build_recall_context(&turn_context, stored_tail)
        .await
        .expect("unsupported legacy recall");

    assert_eq!(parsed(&context)["availability"], "unsupported_legacy");
}

#[tokio::test]
async fn keeps_only_the_newest_64_complete_groups() {
    let (session, turn_context) = make_session_and_context().await;
    let mut items = (0..70)
        .map(|index| {
            RolloutItem::ResponseItem(message("assistant", format!("message {index}").as_str()))
        })
        .collect::<Vec<_>>();
    items.push(compacted("summary", Some(Vec::new())));
    let context = session
        .build_recall_context(&turn_context, tail(session.thread_id, items))
        .await
        .expect("group-bounded recall");
    let value = parsed(&context);

    assert_eq!(value["groups"].as_array().expect("groups").len(), 64);
    assert_eq!(value["omitted_groups"], 6);
    assert_eq!(value["truncated"], true);
    assert_eq!(
        value["groups"][0]["items"][0]["content"][0]["text"],
        "message 6"
    );
}

#[tokio::test]
async fn applies_serialized_output_limits_deterministically() {
    let (session, turn_context) = make_session_and_context().await;
    let items = vec![
        RolloutItem::ResponseItem(message("assistant", &"x".repeat(RECALL_RESULT_MAX_BYTES))),
        compacted("summary", Some(Vec::new())),
    ];
    let stored_tail = tail(session.thread_id, items);

    let first = session
        .build_recall_context(&turn_context, stored_tail.clone())
        .await
        .expect("first bounded recall");
    let second = session
        .build_recall_context(&turn_context, stored_tail)
        .await
        .expect("second bounded recall");
    let value = parsed(&first);

    assert_eq!(first.json(), second.json());
    assert!(first.json().len() <= RECALL_RESULT_MAX_BYTES);
    assert!(approx_token_count(first.json()) <= RECALL_RESULT_MAX_TOKENS);
    assert_eq!(value["groups"].as_array().expect("groups").len(), 0);
    assert_eq!(value["omitted_groups"], 1);
    assert_eq!(value["truncated"], true);
}

#[tokio::test]
async fn post_compact_recovery_cross_thread_tail_is_rejected_before_sampling() {
    let (session, turn_context) = make_session_and_context().await;
    let other_thread = codex_protocol::ThreadId::new();
    assert_ne!(other_thread, session.thread_id);

    let result = session
        .build_recall_context(
            &turn_context,
            tail(
                other_thread,
                vec![compacted("other thread summary", Some(Vec::new()))],
            ),
        )
        .await;
    let Err(error) = result else {
        panic!("cross-thread bounded tail must be rejected");
    };

    assert!(
        error.to_string().contains(&other_thread.to_string()),
        "error should identify the rejected tail owner: {error:#}"
    );
    assert!(
        error.to_string().contains(&session.thread_id.to_string()),
        "error should identify the live session owner: {error:#}"
    );
}

#[tokio::test]
async fn post_compact_recovery_raw_rollout_receipt_data_is_not_projected() {
    const RECEIPT_SENTINEL: &str = "GATE_RECEIPT_MUST_REMAIN_RAW_ROLLOUT_DATA";
    let (session, turn_context) = make_session_and_context().await;
    let context = session
        .build_recall_context(
            &turn_context,
            tail(
                session.thread_id,
                vec![
                    RolloutItem::EventMsg(EventMsg::Error(ErrorEvent {
                        message: RECEIPT_SENTINEL.to_string(),
                        codex_error_info: None,
                    })),
                    RolloutItem::PostCompactRecoveryApplied(PostCompactRecoveryAppliedItem {
                        compaction_window_id: "019b3f6e-7a10-7cc3-8b6e-1d09e2f7a001".to_string(),
                        boundary_item_id: "msg_boundary".to_string(),
                        turn_id: "turn_consuming".to_string(),
                    }),
                    RolloutItem::ResponseItem(message("assistant", "model-visible history")),
                    compacted("summary", Some(Vec::new())),
                ],
            ),
        )
        .await
        .expect("build bounded recall without raw rollout metadata");

    assert!(!context.json().contains(RECEIPT_SENTINEL));
    assert!(!context.json().contains("post_compact_recovery_applied"));
    assert!(
        context.json().contains("model-visible history"),
        "model-visible response items should remain projected"
    );
}
