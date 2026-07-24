use super::ContextualUserFragment;

/// One bounded historical context result returned by the explicit `recall` tool.
pub(crate) struct RecallContext {
    json: String,
    available: bool,
}

impl RecallContext {
    pub(crate) fn new(json: String) -> Self {
        Self {
            json,
            available: true,
        }
    }

    pub(crate) fn unavailable(json: String) -> Self {
        Self {
            json,
            available: false,
        }
    }

    pub(crate) fn json(&self) -> &str {
        self.json.as_str()
    }

    pub(crate) fn is_available(&self) -> bool {
        self.available
    }
}

impl ContextualUserFragment for RecallContext {
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
