use codex_protocol::protocol::TOOL_CONTEXT_CLOSE_TAG;
use codex_protocol::protocol::TOOL_CONTEXT_OPEN_TAG;

use super::ContextualUserFragment;

// Merge-safety anchor: prompt_gc/tool-context fragments are workspace-local
// contextual markers and must remain recognized by forked-child filtering.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ToolContext {
    pub(crate) text: String,
}

impl ContextualUserFragment for ToolContext {
    const ROLE: &'static str = "user";
    const START_MARKER: &'static str = TOOL_CONTEXT_OPEN_TAG;
    const END_MARKER: &'static str = TOOL_CONTEXT_CLOSE_TAG;

    fn body(&self) -> String {
        format!("\n{}\n", self.text)
    }
}
