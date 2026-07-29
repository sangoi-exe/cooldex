use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::models::ResponseItem;
use codex_utils_output_truncation::approx_token_count;
use serde::Serialize;
use serde_json::Value;

use super::ContextualUserFragment;
use super::RecallContext;

const OPEN_MARKER: &str = "<post_compact_recovery>";
const CLOSE_MARKER: &str = "</post_compact_recovery>";
const RECALL_OPEN_MARKER: &str = "<post_compact_recall>";
const RECALL_CLOSE_MARKER: &str = "</post_compact_recall>";
const MAX_PACKET_BYTES: usize = 40 * 1024;
const MAX_PACKET_TOKENS: usize = 9_000;
const DEFAULT_RECOVERY_INSTRUCTIONS: &str = "A context compaction just occurred.";

#[derive(Debug, thiserror::Error)]
pub(crate) enum PostCompactRecoveryContextError {
    #[error("failed to parse bounded recall document: {0}")]
    RecallParse(serde_json::Error),
    #[error("failed to serialize post-compact recovery document: {0}")]
    Serialization(serde_json::Error),
    #[error("post-compact recovery packet exceeds its hard cap")]
    PacketCap,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PostCompactRecoveryContext {
    body: String,
    recall: Option<PostCompactRecallContext>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PostCompactRecallContext {
    body: String,
}

#[derive(Serialize)]
struct PostCompactRecoveryDocument<'a> {
    compaction_window_id: &'a str,
    boundary_item_id: &'a str,
    directive: &'a str,
    runtime_boundary: RuntimeBoundary,
}

#[derive(Serialize)]
struct RuntimeBoundary {
    messages_before: &'static str,
    messages_after: &'static str,
    retained_user_messages: &'static str,
    prior_work: &'static str,
    mid_turn_continuation: &'static str,
}

impl PostCompactRecoveryContext {
    pub(crate) fn new(
        compaction_window_id: &str,
        boundary_item_id: &str,
        instructions: &str,
        recall: Option<&RecallContext>,
    ) -> Result<Self, PostCompactRecoveryContextError> {
        let document = PostCompactRecoveryDocument {
            compaction_window_id,
            boundary_item_id,
            directive: instructions,
            runtime_boundary: RuntimeBoundary {
                messages_before: "retained_historical_context",
                messages_after: "live_continuation",
                retained_user_messages: "not_new_user_input",
                prior_work: "do_not_restart",
                mid_turn_continuation: "resume_interrupted_turn",
            },
        };
        let json = serde_json::to_string(&document)
            .map_err(PostCompactRecoveryContextError::Serialization)?;
        let body = format!("\n{}\n", escape_historical_delimiters(&json));
        let recall = recall
            .filter(|recall| recall.is_available())
            .map(PostCompactRecallContext::new)
            .transpose()?;
        let context = Self { body, recall };
        let mut rendered = context.render();
        if let Some(recall) = context.recall.as_ref() {
            rendered.push_str(&recall.render());
        }
        if rendered.len() > MAX_PACKET_BYTES || approx_token_count(&rendered) > MAX_PACKET_TOKENS {
            return Err(PostCompactRecoveryContextError::PacketCap);
        }
        Ok(context)
    }

    pub(crate) fn default_instructions() -> &'static str {
        DEFAULT_RECOVERY_INSTRUCTIONS
    }

    pub(crate) fn recall(&self) -> Option<&PostCompactRecallContext> {
        self.recall.as_ref()
    }
}

impl PostCompactRecallContext {
    fn new(recall: &RecallContext) -> Result<Self, PostCompactRecoveryContextError> {
        let recall: Value = serde_json::from_str(recall.json())
            .map_err(PostCompactRecoveryContextError::RecallParse)?;
        let json = serde_json::to_string(&recall)
            .map_err(PostCompactRecoveryContextError::Serialization)?;
        Ok(Self {
            body: format!("\n{}\n", escape_historical_delimiters(&json)),
        })
    }

    fn output_content(&self) -> Vec<ContentItem> {
        vec![ContentItem::OutputText {
            text: self.render(),
        }]
    }
}

impl ContextualUserFragment for PostCompactRecoveryContext {
    fn role(&self) -> &'static str {
        "developer"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        (OPEN_MARKER, CLOSE_MARKER)
    }

    fn body(&self) -> String {
        self.body.clone()
    }
}

impl ContextualUserFragment for PostCompactRecallContext {
    fn role(&self) -> &'static str {
        "assistant"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        (RECALL_OPEN_MARKER, RECALL_CLOSE_MARKER)
    }

    fn body(&self) -> String {
        self.body.clone()
    }

    fn into(self) -> ResponseItem
    where
        Self: Sized,
    {
        ResponseItem::Message {
            id: None,
            role: self.role().to_string(),
            content: self.output_content(),
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        }
    }

    fn into_boxed_response_item(self: Box<Self>) -> ResponseItem {
        ResponseItem::Message {
            id: None,
            role: self.role().to_string(),
            content: self.output_content(),
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        }
    }

    fn into_response_input_item(self) -> ResponseInputItem
    where
        Self: Sized,
    {
        ResponseInputItem::Message {
            role: self.role().to_string(),
            content: self.output_content(),
            phase: None,
        }
    }
}

fn escape_historical_delimiters(json: &str) -> String {
    json.replace('&', "\\u0026")
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
}

#[cfg(test)]
#[path = "post_compact_recovery_tests.rs"]
mod tests;
