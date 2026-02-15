use crate::codex::Session;
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
use codex_protocol::protocol::RolloutItem;
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;

pub struct RecallHandler;

const DEFAULT_MAX_ITEMS: usize = 24;
const MAX_MAX_ITEMS: usize = 200;
const DEFAULT_MAX_CHARS_PER_ITEM: usize = 1200;
const MAX_MAX_CHARS_PER_ITEM: usize = 16_000;

fn default_max_items() -> usize {
    DEFAULT_MAX_ITEMS
}

fn default_max_chars_per_item() -> usize {
    DEFAULT_MAX_CHARS_PER_ITEM
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecallToolArgs {
    #[serde(default = "default_max_items")]
    max_items: usize,
    #[serde(default = "default_max_chars_per_item")]
    max_chars_per_item: usize,
}

#[derive(Debug, Clone, Copy)]
enum StopReason {
    InvalidContract,
    Unavailable,
    NoCompactionMarker,
    RolloutReadError,
    RolloutParseError,
}

impl StopReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::InvalidContract => "invalid_contract",
            Self::Unavailable => "unavailable",
            Self::NoCompactionMarker => "no_compaction_marker",
            Self::RolloutReadError => "rollout_read_error",
            Self::RolloutParseError => "rollout_parse_error",
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
            session, payload, ..
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

        let response = handle_recall(session.as_ref(), &args).await?;
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
    args: &RecallToolArgs,
) -> Result<serde_json::Value, FunctionCallError> {
    validate_args(args)?;
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
    if parse_errors > 0 {
        return Err(contract_error(
            StopReason::RolloutParseError,
            format!("current session rollout has {parse_errors} parse error(s)"),
        ));
    }
    build_recall_payload(&rollout_items, args.max_items, args.max_chars_per_item)
}

fn validate_args(args: &RecallToolArgs) -> Result<(), FunctionCallError> {
    if args.max_items == 0 || args.max_items > MAX_MAX_ITEMS {
        return Err(contract_error(
            StopReason::InvalidContract,
            format!("recall.max_items must be between 1 and {MAX_MAX_ITEMS}"),
        ));
    }
    if args.max_chars_per_item == 0 || args.max_chars_per_item > MAX_MAX_CHARS_PER_ITEM {
        return Err(contract_error(
            StopReason::InvalidContract,
            format!("recall.max_chars_per_item must be between 1 and {MAX_MAX_CHARS_PER_ITEM}"),
        ));
    }
    Ok(())
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
    max_items: usize,
    max_chars_per_item: usize,
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
        return Err(contract_error(
            StopReason::NoCompactionMarker,
            "current session rollout has no compacted marker",
        ));
    };

    let mut matching_items =
        collect_pre_compact_items(rollout_items, latest_compacted_index, max_chars_per_item);
    let matching_pre_compact_items = matching_items.len();
    if matching_items.len() > max_items {
        let split_point = matching_items.len() - max_items;
        matching_items = matching_items.split_off(split_point);
    }
    let returned_items = matching_items.len();

    Ok(json!({
        "mode": "recall_pre_compact",
        "source": "current_session_rollout",
        "boundary": {
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
            "max_items": max_items,
        },
        "items": matching_items,
    }))
}

fn collect_pre_compact_items(
    rollout_items: &[RolloutItem],
    latest_compacted_index: usize,
    max_chars_per_item: usize,
) -> Vec<RecallItem> {
    let mut output = Vec::new();
    for (index, rollout_item) in rollout_items
        .iter()
        .enumerate()
        .take(latest_compacted_index)
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
                    text: truncate_to_char_limit(trimmed, max_chars_per_item),
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
                    text: truncate_to_char_limit(trimmed, max_chars_per_item),
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

fn truncate_to_char_limit(text: &str, max_chars: usize) -> String {
    let mut char_indices = text.char_indices();
    let Some((cutoff, _)) = char_indices.nth(max_chars) else {
        return text.to_string();
    };
    format!("{}…", &text[..cutoff])
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
    use codex_protocol::models::FunctionCallOutputPayload;
    use codex_protocol::models::ReasoningItemReasoningSummary::SummaryText;
    use codex_protocol::protocol::CompactedItem;
    use pretty_assertions::assert_eq;
    use serde_json::Value;

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

    fn parse_error_stop_reason(error: FunctionCallError) -> String {
        let FunctionCallError::RespondToModel(raw) = error else {
            panic!("expected RespondToModel error");
        };
        let payload: Value = serde_json::from_str(&raw).expect("structured error payload");
        payload
            .get("stop_reason")
            .and_then(Value::as_str)
            .expect("stop_reason")
            .to_string()
    }

    #[test]
    fn recall_requires_compaction_marker() {
        let rollout_items = vec![assistant_message("before", None), reasoning("analysis")];

        let error = build_recall_payload(&rollout_items, 10, 500)
            .expect_err("must fail when no compaction marker is present");

        assert_eq!(
            parse_error_stop_reason(error),
            StopReason::NoCompactionMarker.as_str()
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

        let payload = build_recall_payload(&rollout_items, 10, 500).expect("build recall payload");
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
    fn recall_returns_only_latest_max_items() {
        let rollout_items = vec![
            assistant_message("assistant 1", None),
            reasoning("reasoning 1"),
            assistant_message("assistant 2", Some(MessagePhase::FinalAnswer)),
            compacted_marker(),
        ];

        let payload = build_recall_payload(&rollout_items, 2, 500).expect("build recall payload");
        let items = payload
            .get("items")
            .and_then(Value::as_array)
            .expect("items array");

        assert_eq!(items.len(), 2);
        assert_eq!(
            items[0].get("rollout_index").and_then(Value::as_u64),
            Some(1)
        );
        assert_eq!(
            items[1].get("rollout_index").and_then(Value::as_u64),
            Some(2)
        );
    }

    #[test]
    fn recall_truncates_text_by_char_limit() {
        let rollout_items = vec![assistant_message("abcdefghijk", None), compacted_marker()];

        let payload = build_recall_payload(&rollout_items, 10, 5).expect("build recall payload");
        let items = payload
            .get("items")
            .and_then(Value::as_array)
            .expect("items array");
        let text = items[0].get("text").and_then(Value::as_str).expect("text");
        assert_eq!(text, "abcde…");
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

        let payload = build_recall_payload(&rollout_items, 10, 500).expect("build recall payload");
        let items = payload
            .get("items")
            .and_then(Value::as_array)
            .expect("items array");
        let text = items[0].get("text").and_then(Value::as_str).expect("text");
        assert_eq!(text, "fallback reasoning text");
    }

    #[test]
    fn recall_rejects_invalid_argument_bounds() {
        for args in [
            RecallToolArgs {
                max_items: 0,
                max_chars_per_item: DEFAULT_MAX_CHARS_PER_ITEM,
            },
            RecallToolArgs {
                max_items: MAX_MAX_ITEMS + 1,
                max_chars_per_item: DEFAULT_MAX_CHARS_PER_ITEM,
            },
            RecallToolArgs {
                max_items: DEFAULT_MAX_ITEMS,
                max_chars_per_item: 0,
            },
            RecallToolArgs {
                max_items: DEFAULT_MAX_ITEMS,
                max_chars_per_item: MAX_MAX_CHARS_PER_ITEM + 1,
            },
        ] {
            let error = validate_args(&args).expect_err("invalid args must fail");
            assert_eq!(
                parse_error_stop_reason(error),
                StopReason::InvalidContract.as_str()
            );
        }
    }

    #[tokio::test]
    async fn recall_fails_when_session_rollout_is_unavailable() {
        let (session, _turn) = crate::codex::make_session_and_context().await;
        let args = RecallToolArgs {
            max_items: DEFAULT_MAX_ITEMS,
            max_chars_per_item: DEFAULT_MAX_CHARS_PER_ITEM,
        };

        let error = handle_recall(&session, &args)
            .await
            .expect_err("must fail without rollout recorder");

        assert_eq!(
            parse_error_stop_reason(error),
            StopReason::Unavailable.as_str()
        );
    }
}
