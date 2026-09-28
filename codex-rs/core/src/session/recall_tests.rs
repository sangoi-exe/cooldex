use std::num::NonZeroUsize;

use codex_history::CompactedItem;
use codex_history::PostCompactRecoveryAppliedItem;
use codex_history::PostCompactRecoveryPayloadKind;
use codex_protocol::ResponseItemId;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ReasoningItemContent;
use codex_protocol::models::ReasoningItemReasoningSummary;
use codex_protocol::protocol::ErrorEvent;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ThreadRolledBackEvent;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;

use super::*;

fn message(role: &str, text: &str) -> ResponseItem {
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

fn reasoning(summary: &[&str], content: &[&str], encrypted_content: Option<&str>) -> ResponseItem {
    ResponseItem::Reasoning {
        id: Some(ResponseItemId::from_server("reasoning-id".to_string())),
        summary: summary
            .iter()
            .map(|text| ReasoningItemReasoningSummary::SummaryText {
                text: text.to_string(),
            })
            .collect(),
        content: Some(
            content
                .iter()
                .map(|text| ReasoningItemContent::ReasoningText {
                    text: text.to_string(),
                })
                .collect(),
        ),
        encrypted_content: encrypted_content.map(str::to_string),
        internal_chat_message_metadata_passthrough: None,
    }
}

fn response(item: ResponseItem) -> RolloutItem {
    RolloutItem::ResponseItem(item.into())
}

fn compacted(replacement_history: Option<Vec<ResponseItem>>) -> RolloutItem {
    RolloutItem::Compacted(CompactedItem {
        message: "compaction summary".to_string(),
        replacement_history: replacement_history
            .map(|items| items.into_iter().map(Into::into).collect()),
        guardian_history: None,
        retained_context: None,
        window_number: None,
        first_window_id: None,
        previous_window_id: None,
        window_id: None,
        post_compact_recovery: None,
        mcp_resource_origins: None,
        compaction_response_id: None,
        latest_token_usage_record: None,
        resume_metadata: None,
    })
}

fn newest(intervals: usize) -> RecallIntervals {
    RecallIntervals::Count(NonZeroUsize::new(intervals).expect("positive interval count"))
}

fn parsed(items: &[RolloutItem], intervals: RecallIntervals) -> Value {
    let context = build_recall_context(items, intervals).expect("build recall intervals");
    serde_json::from_str(context.json()).expect("parse recall JSON")
}

#[test]
fn selects_newest_closed_intervals_without_skipping_empty_intervals() {
    let first = message("assistant", "first interval");
    let second = message("assistant", "second interval, first item");
    let third = message("assistant", "second interval, second item");
    let items = vec![
        response(first.clone()),
        compacted(/*replacement_history*/ None),
        response(second.clone()),
        response(third.clone()),
        compacted(Some(Vec::new())),
        response(message("user", "an interval with no eligible content")),
        compacted(/*replacement_history*/ None),
        compacted(/*replacement_history*/ None),
        response(message("assistant", "current open suffix")),
    ];
    let all = json!([[first], [second, third], [], []]);

    for (intervals, expected) in [
        (RecallIntervals::default(), json!([[]])),
        (newest(/*intervals*/ 2), json!([[], []])),
        (newest(/*intervals*/ 3), json!([all[1], [], []])),
        (newest(/*intervals*/ usize::MAX), all.clone()),
        (RecallIntervals::All(AllIntervals::All), all),
    ] {
        assert_eq!(parsed(&items, intervals), expected);
    }
}

#[test]
fn no_compaction_returns_no_intervals() {
    let items = vec![
        response(message("assistant", "uncompacted answer")),
        response(reasoning(&["visible summary"], &[], /*encrypted_content*/ None)),
    ];

    for intervals in [
        RecallIntervals::default(),
        newest(/*intervals*/ 8),
        RecallIntervals::All(AllIntervals::All),
    ] {
        assert_eq!(parsed(&items, intervals), json!([]));
    }
}

#[test]
fn one_compaction_closes_start_to_first_without_emitting_replacement_or_suffix() {
    let before = message("assistant", "persisted before compaction");
    let items = vec![
        response(before.clone()),
        compacted(Some(vec![message("assistant", "synthetic replacement history")])),
        response(message("assistant", "current open suffix")),
    ];

    for intervals in [
        RecallIntervals::default(),
        newest(/*intervals*/ 8),
        RecallIntervals::All(AllIntervals::All),
    ] {
        assert_eq!(parsed(&items, intervals), json!([[before]]));
    }
}

#[test]
fn returns_only_assistant_messages_and_visible_reasoning_in_persisted_order() {
    let assistant = message("assistant", "persisted assistant answer");
    let visible_summary = reasoning(&["", "visible summary"], &[], Some("encrypted summary"));
    let visible_content = reasoning(&[], &["", "visible content"], Some("encrypted content"));
    let visible_text: ResponseItem = serde_json::from_value(json!({
        "type": "reasoning",
        "summary": [],
        "content": [{"type": "text", "text": "visible text"}],
        "encrypted_content": "encrypted text"
    })).unwrap();
    let mut items = vec![
        response(message("system", "system instructions")),
        response(message("developer", "developer instructions")),
        response(message("user", "user input")),
        response(assistant.clone()),
        response(reasoning(&[], &[], Some("encrypted-only reasoning"))),
        response(reasoning(&[""], &[""], Some("empty visible entries"))),
        response(visible_summary),
        response(visible_content),
        response(visible_text),
        RolloutItem::EventMsg(EventMsg::Error(ErrorEvent {
            message: "diagnostic".to_string(),
            codex_error_info: None,
            misalignment: None,
        })),
        RolloutItem::PostCompactRecoveryApplied(PostCompactRecoveryAppliedItem {
            compaction_window_id: "window-id".to_string(),
            boundary_item_id: "boundary-id".to_string(),
            turn_id: "turn-id".to_string(),
            payload_kind: PostCompactRecoveryPayloadKind::HandoffAndRecovery,
        }),
        RolloutItem::InterAgentCommunicationMetadata { trigger_turn: true },
    ];
    for item in [
        json!({"type": "additional_tools", "role": "assistant", "tools": []}),
        json!({"type": "agent_message", "author": "/root", "recipient": "/root/child", "content": []}),
        json!({"type": "local_shell_call", "status": "completed", "action": {"type": "exec", "command": ["true"]}}),
        json!({"type": "function_call", "name": "shell", "arguments": "{}", "call_id": "call-id"}),
        json!({"type": "function_call_output", "call_id": "call-id", "output": "tool output"}),
        json!({"type": "custom_tool_call", "name": "apply_patch", "input": "patch", "call_id": "custom-id"}),
        json!({"type": "custom_tool_call_output", "call_id": "custom-id", "output": "custom output"}),
        json!({"type": "tool_search_call", "execution": "client", "arguments": {"query": "tools"}}),
        json!({"type": "tool_search_output", "status": "completed", "execution": "client", "tools": []}),
        json!({"type": "web_search_call"}),
        json!({"type": "image_generation_call", "status": "completed", "result": "image"}),
        json!({"type": "compaction", "encrypted_content": "encrypted compaction"}),
        json!({"type": "context_compaction", "encrypted_content": "context compaction"}),
        json!({"type": "configuration_update", "reasoning": {"effort": "medium"}}),
        json!({"type": "compaction_trigger"}),
        json!({"type": "other"}),
    ] {
        items.push(response(serde_json::from_value(item).expect("canonical response fixture")));
    }
    items.push(compacted(/*replacement_history*/ None));
    let expected_summary = json!({
        "type": "reasoning",
        "id": "reasoning-id",
        "summary": [
            {"type": "summary_text", "text": ""},
            {"type": "summary_text", "text": "visible summary"}
        ],
        "content": []
    });
    let expected_content = json!({
        "type": "reasoning",
        "id": "reasoning-id",
        "summary": [],
        "content": [
            {"type": "reasoning_text", "text": ""},
            {"type": "reasoning_text", "text": "visible content"}
        ]
    });
    let expected_text = json!({
        "type": "reasoning",
        "summary": [],
        "content": [{"type": "text", "text": "visible text"}]
    });

    assert_eq!(
        parsed(&items, RecallIntervals::default()),
        json!([[assistant, expected_summary, expected_content, expected_text]])
    );
}

#[test]
fn persisted_compaction_markers_are_not_reinterpreted_by_rollback_events() {
    let first = message("assistant", "first interval");
    let second = message("assistant", "second interval");
    let items = vec![
        response(first.clone()),
        compacted(/*replacement_history*/ None),
        response(second.clone()),
        compacted(/*replacement_history*/ None),
        RolloutItem::EventMsg(EventMsg::ThreadRolledBack(ThreadRolledBackEvent {
            num_turns: 1,
        })),
    ];

    assert_eq!(parsed(&items, RecallIntervals::All(AllIntervals::All)), json!([[first], [second]]));
}
