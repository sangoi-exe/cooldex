/// Helpers for identifying model-visible "session prefix" messages.
///
/// A session prefix is a user-role message that carries configuration or state needed by
/// follow-up turns (e.g. `<environment_context>`, `<turn_aborted>`, `<tool_context>`,
/// `<reasoning_context>`, `<pinned_notes>`). These items are persisted in
/// history so the model can see them, but they are not user intent and must not create user-turn
/// boundaries.
pub(crate) const ENVIRONMENT_CONTEXT_OPEN_TAG: &str = "<environment_context>";
pub(crate) const TURN_ABORTED_OPEN_TAG: &str = "<turn_aborted>";
pub(crate) const TOOL_CONTEXT_OPEN_TAG: &str = "<tool_context>";
pub(crate) const REASONING_CONTEXT_OPEN_TAG: &str = "<reasoning_context>";
pub(crate) const PINNED_NOTES_OPEN_TAG: &str = "<pinned_notes>";

/// Returns true if `text` starts with a session prefix marker (case-insensitive).
pub(crate) fn is_session_prefix(text: &str) -> bool {
    let trimmed = text.trim_start();
    let lowered = trimmed.to_ascii_lowercase();
    lowered.starts_with(ENVIRONMENT_CONTEXT_OPEN_TAG)
        || lowered.starts_with(TURN_ABORTED_OPEN_TAG)
        || lowered.starts_with(TOOL_CONTEXT_OPEN_TAG)
        || lowered.starts_with(REASONING_CONTEXT_OPEN_TAG)
        || lowered.starts_with(PINNED_NOTES_OPEN_TAG)
}

#[cfg(test)]
mod tests {
    use super::is_session_prefix;

    #[test]
    fn detects_tool_context_prefix() {
        assert!(is_session_prefix(
            "<tool_context>\nsummary\n</tool_context>"
        ));
    }

    #[test]
    fn detects_reasoning_context_prefix_with_leading_whitespace() {
        assert!(is_session_prefix(
            " \n\t<reasoning_context>\nsummary\n</reasoning_context>"
        ));
    }

    #[test]
    fn detects_pinned_notes_prefix() {
        assert!(is_session_prefix(
            "<pinned_notes>\nPinned notes:\n- keep this\n</pinned_notes>"
        ));
    }

    #[test]
    fn ignores_regular_user_text() {
        assert!(!is_session_prefix("please run tests"));
    }
}
