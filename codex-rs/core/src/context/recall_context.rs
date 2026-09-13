use super::ContextualUserFragment;
use codex_protocol::models::ContentItemKind;

/// One bounded historical context result returned by the explicit `recall` tool.
// Merge-safety anchor: explicit recall availability remains solely in its bounded JSON contract,
// avoiding a redundant internal flag that could drift from rendered tool output.
pub(crate) struct RecallContext {
    json: String,
}

impl RecallContext {
    pub(crate) fn new(json: String) -> Self {
        Self { json }
    }

    pub(crate) fn unavailable(json: String) -> Self {
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

    fn body(&self) -> String {
        self.json.clone()
    }
}
