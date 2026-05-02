use codex_protocol::protocol::PINNED_NOTES_CLOSE_TAG;
use codex_protocol::protocol::PINNED_NOTES_OPEN_TAG;

use super::ContextualUserFragment;

// Merge-safety anchor: prompt_gc pinned-note fragments are workspace-local
// contextual markers and must remain recognized by forked-child filtering.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PinnedNotes {
    pub(crate) text: String,
}

impl ContextualUserFragment for PinnedNotes {
    const ROLE: &'static str = "user";
    const START_MARKER: &'static str = PINNED_NOTES_OPEN_TAG;
    const END_MARKER: &'static str = PINNED_NOTES_CLOSE_TAG;

    fn body(&self) -> String {
        format!("\n{}\n", self.text)
    }
}
