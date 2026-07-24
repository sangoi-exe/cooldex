use codex_utils_output_truncation::approx_token_count;
use serde::Serialize;
use serde_json::Value;

use super::ContextualUserFragment;
use super::RecallContext;

const OPEN_MARKER: &str = "<post_compact_recovery>";
const CLOSE_MARKER: &str = "</post_compact_recovery>";
const MAX_PACKET_BYTES: usize = 40 * 1024;
const MAX_PACKET_TOKENS: usize = 9_000;

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
}

#[derive(Serialize)]
struct PostCompactRecoveryDocument<'a> {
    compaction_window_id: &'a str,
    boundary_item_id: &'a str,
    runtime_boundary: RuntimeBoundary,
    recall: Value,
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
        recall: &RecallContext,
    ) -> Result<Self, PostCompactRecoveryContextError> {
        let recall = serde_json::from_str(recall.json())
            .map_err(PostCompactRecoveryContextError::RecallParse)?;
        let document = PostCompactRecoveryDocument {
            compaction_window_id,
            boundary_item_id,
            runtime_boundary: RuntimeBoundary {
                messages_before: "retained_historical_context",
                messages_after: "live_continuation",
                retained_user_messages: "not_new_user_input",
                prior_work: "do_not_restart",
                mid_turn_continuation: "resume_interrupted_turn",
            },
            recall,
        };
        let json = serde_json::to_string(&document)
            .map_err(PostCompactRecoveryContextError::Serialization)?;
        let body = format!("\n{}\n", escape_historical_delimiters(&json));
        let rendered = format!("{OPEN_MARKER}{body}{CLOSE_MARKER}");
        if rendered.len() > MAX_PACKET_BYTES
            || approx_token_count(&rendered) > MAX_PACKET_TOKENS
        {
            return Err(PostCompactRecoveryContextError::PacketCap);
        }
        Ok(Self { body })
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

fn escape_historical_delimiters(json: &str) -> String {
    json.replace('&', "\\u0026")
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
}

#[cfg(test)]
#[path = "post_compact_recovery_tests.rs"]
mod tests;
