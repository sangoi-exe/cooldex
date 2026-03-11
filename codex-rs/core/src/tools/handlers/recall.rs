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

#[derive(Debug, Clone, Copy)]
enum RecallBoundaryKind {
    ContextCompactedEvent,
    ReplacementHistoryCompacted,
}

impl RecallBoundaryKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::ContextCompactedEvent => "context_compacted_event",
            Self::ReplacementHistoryCompacted => "replacement_history_compacted",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct RecallBoundary {
    index: usize,
    kind: RecallBoundaryKind,
}

#[derive(Debug, Clone, Serialize)]
struct RecallItem {
    kind: String,
    source: String,
    rollout_index: Option<usize>,
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
        turn.config.recall_debug.unwrap_or(false),
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
    recall_debug: bool,
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
        // Merge-safety anchor: "no_compaction_marker" drives the fail-loud
        // recall-first recovery path after auto-compaction warnings.
        let message = if parse_errors == 0 {
            "current session rollout has no compacted marker".to_string()
        } else {
            format!(
                "current session rollout has no compacted marker (rollout parse errors: {parse_errors})"
            )
        };
        return Err(contract_error(StopReason::NoCompactionMarker, message));
    };

    let last_boundary = previous_recall_boundary_before(rollout_items, latest_compacted_index);
    let start_index = last_boundary.map_or(0, |boundary| boundary.index.saturating_add(1));

    // Merge-safety anchor: replacement-history boundaries must hydrate stored
    // sanitized history before appending newer rollout items.
    let mut matching_items = last_boundary
        .filter(|boundary| {
            matches!(
                boundary.kind,
                RecallBoundaryKind::ReplacementHistoryCompacted
            )
        })
        .and_then(|boundary| replacement_history_for_boundary(rollout_items, boundary.index))
        .map(collect_replacement_history_items)
        .unwrap_or_default();
    matching_items.extend(collect_pre_compact_items(
        rollout_items,
        start_index,
        latest_compacted_index,
    ));
    let matching_pre_compact_items = matching_items.len();

    let recall_bytes_limit = recall_kbytes_limit.saturating_mul(1024);
    let (matching_items, returned_bytes) =
        trim_items_to_bytes_limit(matching_items, recall_bytes_limit);
    if !recall_debug {
        let compact = build_compact_recall_payload(matching_items, recall_bytes_limit);
        return Ok(json!({
            "mode": "recall_pre_compact_compact",
            "source": "current_session_rollout",
            "items": compact.items,
        }));
    }

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
            "last_boundary_index": last_boundary.map(|boundary| boundary.index),
            "last_boundary_kind": last_boundary.map(|boundary| boundary.kind.as_str()),
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

#[derive(Debug)]
struct CompactRecallPayload {
    items: Vec<String>,
}

fn build_compact_recall_payload(
    matching_items: Vec<RecallItem>,
    recall_bytes_limit: usize,
) -> CompactRecallPayload {
    let compact_entries: Vec<String> = matching_items
        .into_iter()
        .map(|item| format!("{} {}", compact_item_tag(item.kind.as_str()), item.text))
        .collect();
    let (trimmed_entries, _) = trim_strings_to_bytes_limit(compact_entries, recall_bytes_limit);

    let mut start_index = 0usize;
    loop {
        let numbered = trimmed_entries
            .iter()
            .skip(start_index)
            .enumerate()
            .map(|(index, entry)| format!("{}: {}", index + 1, entry))
            .collect::<Vec<String>>();
        let numbered_bytes = estimate_string_items_bytes(numbered.iter());
        if numbered_bytes <= recall_bytes_limit || numbered.is_empty() {
            return CompactRecallPayload { items: numbered };
        }
        start_index = start_index.saturating_add(1);
    }
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
        if let Some(item) = recall_item_from_response_item(response_item, "rollout", Some(index)) {
            output.push(item);
        }
    }
    output
}

fn collect_replacement_history_items(replacement_history: &[ResponseItem]) -> Vec<RecallItem> {
    replacement_history
        .iter()
        .filter_map(|response_item| {
            recall_item_from_response_item(response_item, "replacement_history", None)
        })
        .collect()
}

fn replacement_history_for_boundary(
    rollout_items: &[RolloutItem],
    boundary_index: usize,
) -> Option<&[ResponseItem]> {
    match rollout_items.get(boundary_index) {
        Some(RolloutItem::Compacted(compacted)) => compacted.replacement_history.as_deref(),
        _ => None,
    }
}

fn previous_recall_boundary_before(
    rollout_items: &[RolloutItem],
    latest_compacted_index: usize,
) -> Option<RecallBoundary> {
    rollout_items
        .iter()
        .enumerate()
        .take(latest_compacted_index)
        .filter_map(|(index, item)| match item {
            RolloutItem::EventMsg(EventMsg::ContextCompacted(_)) => Some(RecallBoundary {
                index,
                kind: RecallBoundaryKind::ContextCompactedEvent,
            }),
            RolloutItem::Compacted(compacted) if compacted.replacement_history.is_some() => {
                Some(RecallBoundary {
                    index,
                    kind: RecallBoundaryKind::ReplacementHistoryCompacted,
                })
            }
            _ => None,
        })
        .next_back()
}

fn recall_item_from_response_item(
    response_item: &ResponseItem,
    source: &str,
    rollout_index: Option<usize>,
) -> Option<RecallItem> {
    match response_item {
        ResponseItem::Reasoning {
            summary, content, ..
        } => {
            let text = reasoning_text(summary, content);
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return None;
            }
            Some(RecallItem {
                kind: "reasoning".to_string(),
                source: source.to_string(),
                rollout_index,
                text: trimmed.to_string(),
                phase: None,
            })
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
                return None;
            }
            Some(RecallItem {
                kind: "assistant_message".to_string(),
                source: source.to_string(),
                rollout_index,
                text: trimmed.to_string(),
                phase: phase
                    .as_ref()
                    .map(|message_phase| phase_name(message_phase).to_string()),
            })
        }
        _ => None,
    }
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

fn trim_strings_to_bytes_limit(items: Vec<String>, bytes_limit: usize) -> (Vec<String>, usize) {
    if items.is_empty() || bytes_limit == 0 {
        return (Vec::new(), 0);
    }
    let mut used_bytes = 0usize;
    let mut selected_reversed: Vec<String> = Vec::new();
    for item in items.into_iter().rev() {
        let item_bytes = serde_json::to_vec(&item)
            .map(|bytes| bytes.len())
            .unwrap_or_else(|_| item.len());
        if used_bytes.saturating_add(item_bytes) > bytes_limit {
            break;
        }
        used_bytes = used_bytes.saturating_add(item_bytes);
        selected_reversed.push(item);
    }
    selected_reversed.reverse();
    (selected_reversed, used_bytes)
}

fn estimate_string_items_bytes<'a>(items: impl Iterator<Item = &'a String>) -> usize {
    items.fold(0usize, |acc, item| {
        let item_bytes = serde_json::to_vec(item)
            .map(|bytes| bytes.len())
            .unwrap_or_else(|_| item.len());
        acc.saturating_add(item_bytes)
    })
}

fn compact_item_tag(kind: &str) -> &'static str {
    match kind {
        "reasoning" => "[r]",
        "assistant_message" => "[am]",
        _ => "[other]",
    }
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

    fn replacement_history_compacted_marker() -> RolloutItem {
        replacement_history_compacted_marker_with_history(Vec::new())
    }

    fn replacement_history_compacted_marker_with_history(
        replacement_history: Vec<ResponseItem>,
    ) -> RolloutItem {
        RolloutItem::Compacted(CompactedItem {
            message: "replacement history compacted".to_string(),
            replacement_history: Some(replacement_history),
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

        let error = build_recall_payload(&rollout_items, TEST_RECALL_KBYTES_LIMIT, 0, true)
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

        let error = build_recall_payload(&rollout_items, TEST_RECALL_KBYTES_LIMIT, 2, true)
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

        let payload = build_recall_payload(&rollout_items, TEST_RECALL_KBYTES_LIMIT, 0, true)
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

        let payload = build_recall_payload(&rollout_items, TEST_RECALL_KBYTES_LIMIT, 0, true)
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
        let payload = build_recall_payload(&rollout_items, TEST_RECALL_KBYTES_LIMIT, 0, true)
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

        let payload = build_recall_payload(&rollout_items, TEST_RECALL_KBYTES_LIMIT, 0, true)
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
            compacted_marker(),
            RolloutItem::EventMsg(EventMsg::ContextCompacted(ContextCompactedEvent)),
            reasoning("reasoning after previous compact"),
            assistant_message(
                "assistant after previous compact",
                Some(MessagePhase::FinalAnswer),
            ),
            compacted_marker(),
            RolloutItem::EventMsg(EventMsg::ContextCompacted(ContextCompactedEvent)),
            assistant_message("post compact", None),
        ];

        let payload = build_recall_payload(&rollout_items, TEST_RECALL_KBYTES_LIMIT, 0, true)
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
            payload.pointer("/boundary/last_boundary_index"),
            Some(&json!(1))
        );
        assert_eq!(
            payload.pointer("/boundary/last_boundary_kind"),
            Some(&json!("context_compacted_event"))
        );
    }

    #[test]
    fn recall_falls_back_to_start_when_no_previous_boundary_exists() {
        let rollout_items = vec![
            assistant_message("assistant before first compact", None),
            reasoning("reasoning before first compact marker"),
            compacted_marker(),
            RolloutItem::EventMsg(EventMsg::ContextCompacted(ContextCompactedEvent)),
        ];

        let payload = build_recall_payload(&rollout_items, TEST_RECALL_KBYTES_LIMIT, 0, true)
            .expect("build recall payload");
        let items = payload
            .get("items")
            .and_then(Value::as_array)
            .expect("items array");
        assert_eq!(items.len(), 2);
        assert_eq!(payload.pointer("/boundary/start_index"), Some(&json!(0)));
        assert_eq!(
            payload.pointer("/boundary/last_boundary_index"),
            Some(&Value::Null)
        );
        assert_eq!(
            payload.pointer("/boundary/last_boundary_kind"),
            Some(&Value::Null)
        );
    }

    #[test]
    fn recall_uses_previous_replacement_history_compaction_as_boundary() {
        let replacement_history = vec![
            ResponseItem::Reasoning {
                id: "replacement-reasoning".to_string(),
                summary: vec![SummaryText {
                    text: "sanitized reasoning base".to_string(),
                }],
                content: None,
                encrypted_content: None,
            },
            ResponseItem::Message {
                id: None,
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: "sanitized assistant base".to_string(),
                }],
                end_turn: None,
                phase: Some(MessagePhase::Commentary),
            },
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "user should still be ignored".to_string(),
                }],
                end_turn: None,
                phase: None,
            },
        ];
        let rollout_items = vec![
            replacement_history_compacted_marker_with_history(replacement_history),
            reasoning("reasoning after replacement-history compact"),
            assistant_message("assistant after replacement-history compact", None),
            compacted_marker(),
            RolloutItem::EventMsg(EventMsg::ContextCompacted(ContextCompactedEvent)),
            assistant_message("post compact", None),
        ];

        let payload = build_recall_payload(&rollout_items, TEST_RECALL_KBYTES_LIMIT, 0, true)
            .expect("build recall payload");
        let items = payload
            .get("items")
            .and_then(Value::as_array)
            .expect("items array");
        assert_eq!(items.len(), 4);
        assert_eq!(items[0].get("source"), Some(&json!("replacement_history")));
        assert_eq!(items[1].get("source"), Some(&json!("replacement_history")));
        assert_eq!(items[2].get("source"), Some(&json!("rollout")));
        assert_eq!(items[3].get("source"), Some(&json!("rollout")));
        assert_eq!(items[0].get("rollout_index").and_then(Value::as_u64), None);
        assert_eq!(items[1].get("rollout_index").and_then(Value::as_u64), None);
        assert_eq!(
            items[2].get("rollout_index").and_then(Value::as_u64),
            Some(1)
        );
        assert_eq!(
            items[3].get("rollout_index").and_then(Value::as_u64),
            Some(2)
        );
        assert_eq!(
            items[0].get("text"),
            Some(&json!("sanitized reasoning base"))
        );
        assert_eq!(
            items[1].get("text"),
            Some(&json!("sanitized assistant base"))
        );
        assert_eq!(items[1].get("phase"), Some(&json!("commentary")));
        assert_eq!(payload.pointer("/boundary/start_index"), Some(&json!(1)));
        assert_eq!(
            payload.pointer("/boundary/last_boundary_index"),
            Some(&json!(0))
        );
        assert_eq!(
            payload.pointer("/boundary/last_boundary_kind"),
            Some(&json!("replacement_history_compacted"))
        );
    }

    #[test]
    fn recall_uses_most_recent_boundary_marker_across_supported_types() {
        let rollout_items = vec![
            replacement_history_compacted_marker(),
            reasoning("reasoning after replacement-history boundary"),
            compacted_marker(),
            RolloutItem::EventMsg(EventMsg::ContextCompacted(ContextCompactedEvent)),
            assistant_message("assistant after latest context boundary", None),
            compacted_marker(),
            RolloutItem::EventMsg(EventMsg::ContextCompacted(ContextCompactedEvent)),
            assistant_message("post compact", None),
        ];

        let payload = build_recall_payload(&rollout_items, TEST_RECALL_KBYTES_LIMIT, 0, true)
            .expect("build recall payload");
        let items = payload
            .get("items")
            .and_then(Value::as_array)
            .expect("items array");
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].get("rollout_index").and_then(Value::as_u64),
            Some(4)
        );
        assert_eq!(payload.pointer("/boundary/start_index"), Some(&json!(4)));
        assert_eq!(
            payload.pointer("/boundary/last_boundary_index"),
            Some(&json!(3))
        );
        assert_eq!(
            payload.pointer("/boundary/last_boundary_kind"),
            Some(&json!("context_compacted_event"))
        );
    }

    #[test]
    fn recall_ignores_non_replacement_history_compacted_markers_as_lower_boundaries() {
        let rollout_items = vec![
            compacted_marker(),
            reasoning("reasoning after ignored compacted marker"),
            replacement_history_compacted_marker(),
            assistant_message("assistant after replacement-history boundary", None),
            compacted_marker(),
            RolloutItem::EventMsg(EventMsg::ContextCompacted(ContextCompactedEvent)),
            assistant_message("post compact", None),
        ];

        let payload = build_recall_payload(&rollout_items, TEST_RECALL_KBYTES_LIMIT, 0, true)
            .expect("build recall payload");
        let items = payload
            .get("items")
            .and_then(Value::as_array)
            .expect("items array");
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].get("rollout_index").and_then(Value::as_u64),
            Some(3)
        );
        assert_eq!(payload.pointer("/boundary/start_index"), Some(&json!(3)));
        assert_eq!(
            payload.pointer("/boundary/last_boundary_index"),
            Some(&json!(2))
        );
        assert_eq!(
            payload.pointer("/boundary/last_boundary_kind"),
            Some(&json!("replacement_history_compacted"))
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

        let unconstrained = build_recall_payload(&rollout_items, TEST_RECALL_KBYTES_LIMIT, 0, true)
            .expect("build unconstrained payload");
        let unconstrained_bytes = unconstrained
            .pointer("/counts/returned_bytes")
            .and_then(Value::as_u64)
            .expect("returned bytes");

        let constrained =
            build_recall_payload(&rollout_items, 1, 0, true).expect("build constrained payload");
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

        let payload = build_recall_payload(&rollout_items, TEST_RECALL_KBYTES_LIMIT, 1, true)
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
    fn recall_compact_mode_returns_string_items_and_hides_debug_metadata() {
        let rollout_items = vec![
            assistant_message("assistant one", Some(MessagePhase::Commentary)),
            reasoning("reasoning one"),
            compacted_marker(),
        ];

        let payload = build_recall_payload(&rollout_items, TEST_RECALL_KBYTES_LIMIT, 3, false)
            .expect("build compact recall payload");
        let items = payload
            .get("items")
            .and_then(Value::as_array)
            .expect("items array");

        assert_eq!(
            payload.get("mode").and_then(Value::as_str),
            Some("recall_pre_compact_compact")
        );
        assert_eq!(
            payload.get("source").and_then(Value::as_str),
            Some("current_session_rollout")
        );
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].as_str(), Some("1: [am] assistant one"));
        assert_eq!(items[1].as_str(), Some("2: [r] reasoning one"));
        assert!(payload.get("integrity").is_none());
        assert!(payload.get("boundary").is_none());
        assert!(payload.get("counts").is_none());
    }

    #[test]
    fn recall_compact_mode_applies_kbytes_limit_from_tail() {
        let alpha = "a".repeat(700);
        let beta = "b".repeat(700);
        let gamma = "c".repeat(700);
        let rollout_items = vec![
            assistant_message(alpha.as_str(), None),
            assistant_message(beta.as_str(), None),
            assistant_message(gamma.as_str(), None),
            compacted_marker(),
        ];

        let payload = build_recall_payload(&rollout_items, 1, 0, false)
            .expect("build compact constrained payload");
        let items = payload
            .get("items")
            .and_then(Value::as_array)
            .expect("items array");

        assert!(items.len() < 3);
        for (index, item) in items.iter().enumerate() {
            let expected_prefix = format!("{}: [am] ", index + 1);
            let text = item.as_str().expect("compact entry string");
            assert!(
                text.starts_with(expected_prefix.as_str()),
                "compact item should be sequentially numbered: {text}"
            );
        }
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
        let (session, mut turn) = crate::codex::make_session_and_context().await;
        let mut config = (*turn.config).clone();
        config.recall_debug = Some(true);
        turn.config = std::sync::Arc::new(config);
        let recorder = crate::rollout::RolloutRecorder::new(
            turn.config.as_ref(),
            crate::rollout::RolloutRecorderParams::new(
                session.conversation_id,
                None,
                turn.session_source.clone(),
                BaseInstructions::default(),
                Vec::new(),
                crate::rollout::policy::EventPersistenceMode::Limited,
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
