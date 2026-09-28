use std::num::NonZeroUsize;

use codex_history::RolloutItem;
use codex_protocol::ResponseItemId;
use codex_protocol::models::InternalChatMessageMetadataPassthrough;
use codex_protocol::models::ReasoningItemContent;
use codex_protocol::models::ReasoningItemReasoningSummary;
use codex_protocol::models::ResponseItem;
use codex_thread_store::LoadThreadHistoryParams;
use serde::Deserialize;
use serde::Serialize;

use super::Session;
use crate::context::RecallContext;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(untagged)]
pub(crate) enum RecallIntervals {
    Count(NonZeroUsize),
    All(AllIntervals),
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
pub(crate) enum AllIntervals {
    #[serde(rename = "all")]
    All,
}

impl Default for RecallIntervals {
    fn default() -> Self {
        Self::Count(NonZeroUsize::MIN)
    }
}

#[derive(Serialize)]
#[serde(untagged)]
enum RecallItem<'a> {
    Message(&'a ResponseItem),
    Reasoning(RecallReasoning<'a>),
}

/// Recall preserves visible reasoning entries independently of the provider's content-omission policy.
#[derive(Serialize)]
struct RecallReasoning<'a> {
    #[serde(rename = "type")]
    item_type: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<&'a ResponseItemId>,
    summary: &'a [ReasoningItemReasoningSummary],
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<&'a [ReasoningItemContent]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    internal_chat_message_metadata_passthrough: Option<&'a InternalChatMessageMetadataPassthrough>,
}

impl Session {
    pub(crate) async fn load_current_thread_recall_context(
        &self,
        intervals: RecallIntervals,
    ) -> anyhow::Result<RecallContext> {
        if let Some(live_thread) = self.live_thread() {
            live_thread.flush_history().await?;
        }
        let history = self
            .services
            .thread_store
            .load_history(LoadThreadHistoryParams {
                thread_id: self.thread_id,
                include_archived: true,
            })
            .await?;
        Ok(build_recall_context(&history.items, intervals)?)
    }
}

fn build_recall_context(
    items: &[RolloutItem],
    intervals: RecallIntervals,
) -> serde_json::Result<RecallContext> {
    let mut closed_intervals = Vec::new();
    let mut start = 0;
    for (index, item) in items.iter().enumerate() {
        if matches!(item, RolloutItem::Compacted(_)) {
            closed_intervals.push(&items[start..index]);
            start = index + 1;
        }
    }
    let selected_start = match intervals {
        RecallIntervals::Count(count) => closed_intervals.len().saturating_sub(count.get()),
        RecallIntervals::All(AllIntervals::All) => 0,
    };
    let selected = closed_intervals
        .into_iter()
        .skip(selected_start)
        .map(|interval| {
            interval
                .iter()
                .filter_map(|item| {
                    let RolloutItem::ResponseItem(envelope) = item else {
                        return None;
                    };
                    match &envelope.item {
                        ResponseItem::Message { role, .. } if role == "assistant" => {
                            Some(RecallItem::Message(&envelope.item))
                        }
                        ResponseItem::Reasoning {
                            id,
                            summary,
                            content,
                            encrypted_content: _,
                            internal_chat_message_metadata_passthrough,
                        } if summary.iter().any(
                            |ReasoningItemReasoningSummary::SummaryText { text }| !text.is_empty(),
                        ) || content.iter().flatten().any(|entry| match entry {
                            ReasoningItemContent::ReasoningText { text }
                            | ReasoningItemContent::Text { text } => !text.is_empty(),
                        }) => Some(RecallItem::Reasoning(RecallReasoning {
                            item_type: "reasoning",
                            id: id.as_ref(),
                            summary,
                            content: content.as_deref(),
                            internal_chat_message_metadata_passthrough:
                                internal_chat_message_metadata_passthrough.as_ref(),
                        })),
                        _ => None,
                    }
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    Ok(RecallContext::new(serde_json::to_string(&selected)?))
}

#[cfg(test)]
#[path = "recall_tests.rs"]
mod tests;
