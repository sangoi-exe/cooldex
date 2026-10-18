use super::ContextualUserFragment;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ContentItemKind;
use codex_protocol::models::input_text;

/// Chronological persisted assistant and reasoning intervals returned by the explicit `recall` tool.
// Merge-safety anchor: Recall renders its interval list directly, without an availability envelope or local truncation.
pub(crate) struct RecallContext {
    json: String,
}

impl RecallContext {
    pub(crate) fn new(json: String) -> Self {
        Self { json }
    }

    pub(crate) fn json(&self) -> &str {
        self.json.as_str()
    }
}

impl ContextualUserFragment for RecallContext {
    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind("recall.context".to_string())
    }

    fn role(&self) -> &'static str {
        "user"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("", "")
    }

    fn body(&self) -> ContentItem {
        input_text(self.json.clone())
    }
}
