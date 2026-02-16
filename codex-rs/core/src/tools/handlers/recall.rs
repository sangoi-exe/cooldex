use crate::codex::Session;
use crate::codex::TurnContext;
use crate::function_tool::FunctionCallError;
use crate::rollout::RolloutRecorder;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;
use async_trait::async_trait;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::MessagePhase;
use codex_protocol::models::ReasoningItemContent;
use codex_protocol::models::ReasoningItemReasoningSummary;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::RolloutItem;
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;

pub struct RecallHandler;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecallToolArgs {}

#[derive(Debug, Clone, Copy)]
enum StopReason {
    InvalidContract,
    Unavailable,
    NoCompactionMarker,
    RolloutReadError,
}

impl StopReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::InvalidContract => "invalid_contract",
            Self::Unavailable => "unavailable",
            Self::NoCompactionMarker => "no_compaction_marker",
            Self::RolloutReadError => "rollout_read_error",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct RecallItem {
    kind: String,
    rollout_index: usize,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    phase: Option<String>,
}

#[async_trait]
impl ToolHandler for RecallHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            payload,
            ..
        } = invocation;

        let ToolPayload::Function { arguments } = payload else {
            return Err(contract_error(
                StopReason::InvalidContract,
                "recall handler received unsupported payload",
            ));
        };

        let args: RecallToolArgs = serde_json::from_str(&arguments).map_err(|error| {
            contract_error(
                StopReason::InvalidContract,
                format!("failed to parse function arguments: {error}"),
            )
        })?;

        let response = handle_recall(session.as_ref(), turn.as_ref(), &args).await?;
        Ok(ToolOutput::Function {
            body: FunctionCallOutputBody::Text(
                serde_json::to_string(&response).unwrap_or_else(|_| "{}".to_string()),
            ),
            success: Some(true),
        })
    }
}

async fn handle_recall(
    session: &Session,
    turn: &TurnContext,
    _args: &RecallToolArgs,
) -> Result<serde_json::Value, FunctionCallError> {
    let rollout_recorder = current_rollout_recorder(session).await?;
    rollout_recorder.flush().await.map_err(|error| {
        contract_error(
            StopReason::RolloutReadError,
            format!("failed to flush current session rollout: {error}"),
        )
    })?;
    let rollout_path = rollout_recorder.rollout_path().to_path_buf();
    let (rollout_items, _thread_id, parse_errors) =
        RolloutRecorder::load_rollout_items(rollout_path.as_path())
            .await
            .map_err(|error| {
                contract_error(
                    StopReason::RolloutReadError,
                    format!("failed to read current session rollout: {error}"),
                )
            })?;
    build_recall_payload(
        &rollout_items,
        turn.config.recall_kbytes_limit,
        parse_errors,
    )
}

async fn current_rollout_recorder(session: &Session) -> Result<RolloutRecorder, FunctionCallError> {
    let recorder = {
        let guard = session.services.rollout.lock().await;
        guard.clone()
    };
    recorder.ok_or_else(|| {
        contract_error(
            StopReason::Unavailable,
            "recall requires an active current-session rollout recorder",
        )
    })
}

fn build_recall_payload(
    rollout_items: &[RolloutItem],
    recall_kbytes_limit: usize,
    parse_errors: usize,
) -> Result<serde_json::Value, FunctionCallError> {
    let compacted_markers_seen = rollout_items
        .iter()
        .filter(|item| matches!(item, RolloutItem::Compacted(_)))
        .count();
    let Some(latest_compacted_index) = rollout_items
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, item)| matches!(item, RolloutItem::Compacted(_)).then_some(index))
    else {
        let message = if parse_errors == 0 {
            "current session rollout has no compacted marker".to_string()
        } else {
            format!(
                "current session rollout has no compacted marker (rollout parse errors: {parse_errors})"
            )
        };
        return Err(contract_error(StopReason::NoCompactionMarker, message));
    };

    let last_context_compacted_event_index =
        previous_context_compacted_event_index_before(rollout_items, latest_compacted_index);
    let start_index = last_context_compacted_event_index.map_or(0, |index| index.saturating_add(1));

    let matching_items =
        collect_pre_compact_items(rollout_items, start_index, latest_compacted_index);
    let matching_pre_compact_items = matching_items.len();

    let recall_bytes_limit = recall_kbytes_limit.saturating_mul(1024);
    let (matching_items, returned_bytes) =
        trim_items_to_bytes_limit(matching_items, recall_bytes_limit);
    let returned_items = matching_items.len();

    Ok(json!({
        "mode": "recall_pre_compact",
        "source": "current_session_rollout",
        "integrity": {
            "status": if parse_errors == 0 { "ok" } else { "degraded" },
            "rollout_parse_errors": parse_errors,
        },
        "boundary": {
            "start_index": start_index,
            "last_context_compacted_event_index": last_context_compacted_event_index,
            "latest_compacted_index": latest_compacted_index,
            "compacted_markers_seen": compacted_markers_seen,
        },
        "filters": {
            "include_reasoning": true,
            "include_assistant_messages": true,
            "exclude_tool_output": true,
        },
        "counts": {
            "matching_pre_compact_items": matching_pre_compact_items,
            "returned_items": returned_items,
            "returned_bytes": returned_bytes,
            "bytes_limit": recall_bytes_limit,
        },
        "items": matching_items,
    }))
}

fn collect_pre_compact_items(
    rollout_items: &[RolloutItem],
    start_index: usize,
    latest_compacted_index: usize,
) -> Vec<RecallItem> {
    let mut output = Vec::new();
    for (index, rollout_item) in rollout_items
        .iter()
        .enumerate()
        .skip(start_index)
        .take(latest_compacted_index.saturating_sub(start_index))
    {
        let RolloutItem::ResponseItem(response_item) = rollout_item else {
            continue;
        };
        match response_item {
            ResponseItem::Reasoning {
                summary, content, ..
            } => {
                let text = reasoning_text(summary, content);
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    continue;
                }
                output.push(RecallItem {
                    kind: "reasoning".to_string(),
                    rollout_index: index,
                    text: trimmed.to_string(),
                    phase: None,
                });
            }
            ResponseItem::Message {
                role,
                content,
                phase,
                ..
            } if role == "assistant" => {
                let text = assistant_message_text(content);
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    continue;
                }
                output.push(RecallItem {
                    kind: "assistant_message".to_string(),
                    rollout_index: index,
                    text: trimmed.to_string(),
                    phase: phase
                        .as_ref()
                        .map(|message_phase| phase_name(message_phase).to_string()),
                });
            }
            _ => {}
        }
    }
    output
}

fn previous_context_compacted_event_index_before(
    rollout_items: &[RolloutItem],
    latest_compacted_index: usize,
) -> Option<usize> {
    let mut compacted_events = rollout_items
        .iter()
        .enumerate()
        .take(latest_compacted_index)
        .filter_map(|(index, item)| {
            matches!(item, RolloutItem::EventMsg(EventMsg::ContextCompacted(_))).then_some(index)
        });
    let _current_compaction_event = compacted_events.next_back();
    compacted_events.next_back()
}

fn trim_items_to_bytes_limit(
    items: Vec<RecallItem>,
    bytes_limit: usize,
) -> (Vec<RecallItem>, usize) {
    if items.is_empty() || bytes_limit == 0 {
        return (Vec::new(), 0);
    }
    let mut used_bytes = 0usize;
    let mut selected_reversed: Vec<RecallItem> = Vec::new();
    for item in items.into_iter().rev() {
        let item_bytes = serde_json::to_vec(&item)
            .map(|bytes| bytes.len())
            .unwrap_or_else(|_| item.text.len());
        if used_bytes.saturating_add(item_bytes) > bytes_limit {
            break;
        }
        used_bytes = used_bytes.saturating_add(item_bytes);
        selected_reversed.push(item);
    }
    selected_reversed.reverse();
    (selected_reversed, used_bytes)
}

fn reasoning_text(
    summary: &[ReasoningItemReasoningSummary],
    content: &Option<Vec<ReasoningItemContent>>,
) -> String {
    let mut segments: Vec<String> = summary
        .iter()
        .filter_map(|summary_item| match summary_item {
            ReasoningItemReasoningSummary::SummaryText { text } => {
                let trimmed = text.trim();
                (!trimmed.is_empty()).then_some(trimmed.to_string())
            }
        })
        .collect();

    if segments.is_empty()
        && let Some(content_items) = content
    {
        for content_item in content_items {
            match content_item {
                ReasoningItemContent::ReasoningText { text }
                | ReasoningItemContent::Text { text } => {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        segments.push(trimmed.to_string());
                    }
                }
            }
        }
    }

    segments.join("\n")
}

fn assistant_message_text(content_items: &[ContentItem]) -> String {
    content_items
        .iter()
        .filter_map(|content_item| match content_item {
            ContentItem::OutputText { text } | ContentItem::InputText { text } => {
                let trimmed = text.trim();
                (!trimmed.is_empty()).then_some(trimmed.to_string())
            }
            ContentItem::InputImage { .. } => None,
        })
        .collect::<Vec<String>>()
        .join("\n")
}

fn phase_name(phase: &MessagePhase) -> &'static str {
    match phase {
        MessagePhase::Commentary => "commentary",
        MessagePhase::FinalAnswer => "final_answer",
    }
}

fn contract_error(reason: StopReason, message: impl Into<String>) -> FunctionCallError {
    FunctionCallError::RespondToModel(
        json!({
            "stop_reason": reason.as_str(),
            "message": message.into(),
        })
        .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::models::BaseInstructions;
    use codex_protocol::models::FunctionCallOutputPayload;
    use codex_protocol::models::ReasoningItemReasoningSummary::SummaryText;
    use codex_protocol::protocol::CompactedItem;
    use codex_protocol::protocol::ContextCompactedEvent;
    use codex_protocol::protocol::UserMessageEvent;
    use pretty_assertions::assert_eq;
    use serde_json::Value;
    use tokio::io::AsyncWriteExt;

    const TEST_RECALL_KBYTES_LIMIT: usize = 256;

    fn assistant_message(text: &str, phase: Option<MessagePhase>) -> RolloutItem {
        RolloutItem::ResponseItem(ResponseItem::Message {
            id: None,
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: text.to_string(),
            }],
            end_turn: None,
            phase,
        })
    }

    fn user_message(text: &str) -> RolloutItem {
        RolloutItem::ResponseItem(ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: text.to_string(),
            }],
            end_turn: None,
            phase: None,
        })
    }

    fn reasoning(summary_text: &str) -> RolloutItem {
        RolloutItem::ResponseItem(ResponseItem::Reasoning {
            id: "reasoning-item".to_string(),
            summary: vec![SummaryText {
                text: summary_text.to_string(),
            }],
            content: None,
            encrypted_content: None,
        })
    }

    fn tool_output(call_id: &str, output: &str) -> RolloutItem {
        RolloutItem::ResponseItem(ResponseItem::FunctionCallOutput {
            call_id: call_id.to_string(),
            output: FunctionCallOutputPayload::from_text(output.to_string()),
        })
    }

    fn compacted_marker() -> RolloutItem {
        RolloutItem::Compacted(CompactedItem {
            message: "auto compacted".to_string(),
            replacement_history: None,
        })
    }

    fn user_message_event(text: &str) -> RolloutItem {
        RolloutItem::EventMsg(EventMsg::UserMessage(UserMessageEvent {
            message: text.to_string(),
            images: None,
            local_images: Vec::new(),
            text_elements: Vec::new(),
        }))
    }

    fn parse_error_payload(error: FunctionCallError) -> Value {
        let FunctionCallError::RespondToModel(raw) = error else {
            panic!("expected RespondToModel error");
        };
        serde_json::from_str(&raw).expect("structured error payload")
    }

    fn parse_error_stop_reason(error: FunctionCallError) -> String {
        let payload = parse_error_payload(error);
        payload
            .get("stop_reason")
            .and_then(Value::as_str)
            .expect("stop_reason")
            .to_string()
    }

    #[test]
    fn recall_requires_compaction_marker() {
        let rollout_items = vec![assistant_message("before", None), reasoning("analysis")];

        let error = build_recall_payload(&rollout_items, TEST_RECALL_KBYTES_LIMIT, 0)
            .expect_err("must fail when no compaction marker is present");
        let payload = parse_error_payload(error);

        assert_eq!(
            payload.get("stop_reason").and_then(Value::as_str),
            Some(StopReason::NoCompactionMarker.as_str())
        );
        assert_eq!(
            payload.get("message").and_then(Value::as_str),
            Some("current session rollout has no compacted marker")
        );
    }

    #[test]
    fn recall_no_compaction_marker_error_reports_parse_errors_when_present() {
        let rollout_items = vec![assistant_message("before", None), reasoning("analysis")];

        let error = build_recall_payload(&rollout_items, TEST_RECALL_KBYTES_LIMIT, 2)
            .expect_err("must fail when no compaction marker is present");
        let payload = parse_error_payload(error);

        assert_eq!(
            payload.get("stop_reason").and_then(Value::as_str),
            Some(StopReason::NoCompactionMarker.as_str())
        );
        assert_eq!(
            payload.get("message").and_then(Value::as_str),
            Some("current session rollout has no compacted marker (rollout parse errors: 2)")
        );
    }

    #[test]
    fn recall_filters_to_pre_compact_assistant_and_reasoning_only() {
        let rollout_items = vec![
            user_message("ignored"),
            assistant_message("first assistant", Some(MessagePhase::Commentary)),
            tool_output("call_1", "tool output should be ignored"),
            reasoning("reasoning before compact"),
            compacted_marker(),
            assistant_message("after compact should not be included", None),
            reasoning("after compact should not be included"),
        ];

        let payload = build_recall_payload(&rollout_items, TEST_RECALL_KBYTES_LIMIT, 0)
            .expect("build recall payload");
        let items = payload
            .get("items")
            .and_then(Value::as_array)
            .expect("items array");

        assert_eq!(items.len(), 2);
        assert_eq!(
            items[0].get("kind").and_then(Value::as_str),
            Some("assistant_message")
        );
        assert_eq!(
            items[1].get("kind").and_then(Value::as_str),
            Some("reasoning")
        );
        assert_eq!(
            payload.pointer("/counts/matching_pre_compact_items"),
            Some(&json!(2))
        );
        assert_eq!(payload.pointer("/counts/returned_items"), Some(&json!(2)));
    }

    #[test]
    fn recall_keeps_full_message_text_without_char_arg() {
        let rollout_items = vec![assistant_message("abcdefghijk", None), compacted_marker()];

        let payload = build_recall_payload(&rollout_items, TEST_RECALL_KBYTES_LIMIT, 0)
            .expect("build recall payload");
        let items = payload
            .get("items")
            .and_then(Value::as_array)
            .expect("items array");

        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].get("text").and_then(Value::as_str),
            Some("abcdefghijk")
        );
    }

    #[test]
    fn recall_counts_do_not_expose_removed_max_items_field() {
        let rollout_items = vec![assistant_message("assistant 1", None), compacted_marker()];
        let payload = build_recall_payload(&rollout_items, TEST_RECALL_KBYTES_LIMIT, 0)
            .expect("build recall payload");

        assert!(payload.pointer("/counts/max_items").is_none());
    }

    #[test]
    fn recall_uses_reasoning_content_when_summary_is_missing() {
        let rollout_items = vec![
            RolloutItem::ResponseItem(ResponseItem::Reasoning {
                id: "reasoning-item".to_string(),
                summary: Vec::new(),
                content: Some(vec![ReasoningItemContent::ReasoningText {
                    text: "fallback reasoning text".to_string(),
                }]),
                encrypted_content: None,
            }),
            compacted_marker(),
        ];

        let payload = build_recall_payload(&rollout_items, TEST_RECALL_KBYTES_LIMIT, 0)
            .expect("build recall payload");
        let items = payload
            .get("items")
            .and_then(Value::as_array)
            .expect("items array");
        let text = items[0].get("text").and_then(Value::as_str).expect("text");
        assert_eq!(text, "fallback reasoning text");
    }

    #[test]
    fn recall_uses_previous_context_compacted_event_as_boundary() {
        let rollout_items = vec![
            assistant_message("assistant before previous compact", None),
            RolloutItem::EventMsg(EventMsg::ContextCompacted(ContextCompactedEvent)),
            reasoning("reasoning after previous compact"),
            assistant_message(
                "assistant after previous compact",
                Some(MessagePhase::FinalAnswer),
            ),
            RolloutItem::EventMsg(EventMsg::ContextCompacted(ContextCompactedEvent)),
            compacted_marker(),
            assistant_message("post compact", None),
        ];

        let payload = build_recall_payload(&rollout_items, TEST_RECALL_KBYTES_LIMIT, 0)
            .expect("build recall payload");
        let items = payload
            .get("items")
            .and_then(Value::as_array)
            .expect("items array");
        assert_eq!(items.len(), 2);
        assert_eq!(
            items[0].get("rollout_index").and_then(Value::as_u64),
            Some(2)
        );
        assert_eq!(
            items[1].get("rollout_index").and_then(Value::as_u64),
            Some(3)
        );
        assert_eq!(payload.pointer("/boundary/start_index"), Some(&json!(2)));
        assert_eq!(
            payload.pointer("/boundary/last_context_compacted_event_index"),
            Some(&json!(1))
        );
    }

    #[test]
    fn recall_falls_back_to_start_when_no_previous_context_compacted_event_exists() {
        let rollout_items = vec![
            assistant_message("assistant before first compact", None),
            RolloutItem::EventMsg(EventMsg::ContextCompacted(ContextCompactedEvent)),
            reasoning("reasoning before first compact marker"),
            compacted_marker(),
        ];

        let payload = build_recall_payload(&rollout_items, TEST_RECALL_KBYTES_LIMIT, 0)
            .expect("build recall payload");
        let items = payload
            .get("items")
            .and_then(Value::as_array)
            .expect("items array");
        assert_eq!(items.len(), 2);
        assert_eq!(payload.pointer("/boundary/start_index"), Some(&json!(0)));
        assert_eq!(
            payload.pointer("/boundary/last_context_compacted_event_index"),
            Some(&Value::Null)
        );
    }

    #[test]
    fn recall_applies_kbytes_limit_from_tail() {
        let alpha = "a".repeat(700);
        let beta = "b".repeat(700);
        let gamma = "c".repeat(700);
        let rollout_items = vec![
            assistant_message(alpha.as_str(), None),
            assistant_message(beta.as_str(), None),
            assistant_message(gamma.as_str(), None),
            compacted_marker(),
        ];

        let unconstrained = build_recall_payload(&rollout_items, TEST_RECALL_KBYTES_LIMIT, 0)
            .expect("build unconstrained payload");
        let unconstrained_bytes = unconstrained
            .pointer("/counts/returned_bytes")
            .and_then(Value::as_u64)
            .expect("returned bytes");

        let constrained =
            build_recall_payload(&rollout_items, 1, 0).expect("build constrained payload");
        let constrained_items = constrained
            .get("items")
            .and_then(Value::as_array)
            .expect("items array");
        let constrained_bytes = constrained
            .pointer("/counts/returned_bytes")
            .and_then(Value::as_u64)
            .expect("returned bytes");

        assert!(constrained_items.len() < 3);
        assert!(constrained_bytes <= 1024);
        assert!(constrained_bytes < unconstrained_bytes);
        assert_eq!(
            constrained.pointer("/counts/bytes_limit"),
            Some(&json!(1024))
        );
    }

    #[test]
    fn recall_reports_degraded_integrity_when_rollout_has_parse_errors() {
        let rollout_items = vec![
            assistant_message("assistant before compact", None),
            compacted_marker(),
        ];

        let payload = build_recall_payload(&rollout_items, TEST_RECALL_KBYTES_LIMIT, 1)
            .expect("build recall payload");

        assert_eq!(
            payload.pointer("/integrity/status"),
            Some(&json!("degraded"))
        );
        assert_eq!(
            payload.pointer("/integrity/rollout_parse_errors"),
            Some(&json!(1))
        );
    }

    #[test]
    fn recall_rejects_removed_max_items_argument() {
        let parse = serde_json::from_str::<RecallToolArgs>(r#"{"max_items":8}"#);
        assert!(parse.is_err());
    }

    #[test]
    fn recall_rejects_removed_max_chars_per_item_argument() {
        let parse = serde_json::from_str::<RecallToolArgs>(r#"{"max_chars_per_item":400}"#);
        assert!(parse.is_err());
    }

    #[tokio::test]
    async fn recall_fails_when_session_rollout_is_unavailable() {
        let (session, turn) = crate::codex::make_session_and_context().await;
        let args = RecallToolArgs {};

        let error = handle_recall(&session, &turn, &args)
            .await
            .expect_err("must fail without rollout recorder");

        assert_eq!(
            parse_error_stop_reason(error),
            StopReason::Unavailable.as_str()
        );
    }

    #[tokio::test]
    async fn recall_returns_degraded_integrity_when_rollout_contains_invalid_line() {
        let (session, turn) = crate::codex::make_session_and_context().await;
        let recorder = crate::rollout::RolloutRecorder::new(
            turn.config.as_ref(),
            crate::rollout::RolloutRecorderParams::new(
                session.conversation_id,
                None,
                turn.session_source.clone(),
                BaseInstructions::default(),
                Vec::new(),
            ),
            None,
            None,
        )
        .await
        .expect("create rollout recorder");
        let rollout_path = recorder.rollout_path().to_path_buf();
        {
            let mut guard = session.services.rollout.lock().await;
            *guard = Some(recorder.clone());
        }

        session
            .persist_rollout_items(&[
                user_message_event("continue"),
                assistant_message("assistant before compact", Some(MessagePhase::Commentary)),
                compacted_marker(),
            ])
            .await;
        session.ensure_rollout_materialized().await;
        recorder.flush().await.expect("flush rollout");

        let mut file = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&rollout_path)
            .await
            .expect("open rollout");
        file.write_all(
            b"timestamp\":\"2026-02-16T00:00:00.000Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\"}}\n",
        )
        .await
        .expect("append malformed rollout line");
        file.flush().await.expect("flush malformed line");

        let payload = handle_recall(&session, &turn, &RecallToolArgs {})
            .await
            .expect("recall response");

        assert_eq!(
            payload.pointer("/integrity/status"),
            Some(&json!("degraded"))
        );
        assert_eq!(
            payload.pointer("/integrity/rollout_parse_errors"),
            Some(&json!(1))
        );
        assert_eq!(payload.pointer("/counts/returned_items"), Some(&json!(1)));
    }
}
