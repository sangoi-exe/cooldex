use codex_protocol::protocol::AgentStatus;

/// Helpers for identifying model-visible "session prefix" messages.
///
/// A session prefix is a user-role message that carries configuration or state needed by
/// follow-up turns (e.g. `<environment_context>`, `<turn_aborted>`, `<tool_context>`,
/// `<reasoning_context>`, `<pinned_notes>`, `<subagent_notification>`). These items are
/// persisted in history so the model can see them, but they are not user intent and must not
/// create user-turn boundaries.
pub(crate) const ENVIRONMENT_CONTEXT_OPEN_TAG: &str = "<environment_context>";
pub(crate) const TURN_ABORTED_OPEN_TAG: &str = "<turn_aborted>";
pub(crate) const TOOL_CONTEXT_OPEN_TAG: &str = "<tool_context>";
pub(crate) const REASONING_CONTEXT_OPEN_TAG: &str = "<reasoning_context>";
pub(crate) const PINNED_NOTES_OPEN_TAG: &str = "<pinned_notes>";
pub(crate) const SUBAGENT_NOTIFICATION_OPEN_TAG: &str = "<subagent_notification>";
pub(crate) const SUBAGENT_NOTIFICATION_CLOSE_TAG: &str = "</subagent_notification>";

fn starts_with_ascii_case_insensitive(text: &str, prefix: &str) -> bool {
    text.get(..prefix.len())
        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(prefix))
}

/// Returns true if `text` starts with a session prefix marker (case-insensitive).
pub(crate) fn is_session_prefix(text: &str) -> bool {
    let trimmed = text.trim_start();
    starts_with_ascii_case_insensitive(trimmed, ENVIRONMENT_CONTEXT_OPEN_TAG)
        || starts_with_ascii_case_insensitive(trimmed, TURN_ABORTED_OPEN_TAG)
        || starts_with_ascii_case_insensitive(trimmed, TOOL_CONTEXT_OPEN_TAG)
        || starts_with_ascii_case_insensitive(trimmed, REASONING_CONTEXT_OPEN_TAG)
        || starts_with_ascii_case_insensitive(trimmed, PINNED_NOTES_OPEN_TAG)
        || starts_with_ascii_case_insensitive(trimmed, SUBAGENT_NOTIFICATION_OPEN_TAG)
}

pub(crate) fn format_subagent_notification_message(agent_id: &str, status: &AgentStatus) -> String {
    let payload_json = serde_json::json!({
        "agent_id": agent_id,
        "status": status,
    })
    .to_string();
    format!("{SUBAGENT_NOTIFICATION_OPEN_TAG}\n{payload_json}\n{SUBAGENT_NOTIFICATION_CLOSE_TAG}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn detects_tool_context_prefix() {
        assert_eq!(
            is_session_prefix("<tool_context>\nsummary\n</tool_context>"),
            true
        );
    }

    #[test]
    fn detects_reasoning_context_prefix_with_leading_whitespace() {
        assert_eq!(
            is_session_prefix(" \n\t<reasoning_context>\nsummary\n</reasoning_context>"),
            true
        );
    }

    #[test]
    fn detects_pinned_notes_prefix() {
        assert_eq!(
            is_session_prefix("<pinned_notes>\nPinned notes:\n- keep this\n</pinned_notes>"),
            true
        );
    }

    #[test]
    fn detects_subagent_notification_prefix() {
        assert_eq!(
            is_session_prefix(
                "<subagent_notification>{\"agent_id\":\"a\"}</subagent_notification>"
            ),
            true
        );
    }

    #[test]
    fn ignores_regular_user_text() {
        assert_eq!(is_session_prefix("please run tests"), false);
    }

    #[test]
    fn is_session_prefix_is_case_insensitive() {
        assert_eq!(
            is_session_prefix("<SUBAGENT_NOTIFICATION>{}</subagent_notification>"),
            true
        );
    }

    #[test]
    fn formats_subagent_notification_message_with_tags_and_payload() {
        let rendered = format_subagent_notification_message("agent-1", &AgentStatus::Running);
        assert_eq!(rendered.starts_with(SUBAGENT_NOTIFICATION_OPEN_TAG), true);
        assert_eq!(rendered.ends_with(SUBAGENT_NOTIFICATION_CLOSE_TAG), true);
        assert_eq!(rendered.contains("\"agent_id\":\"agent-1\""), true);
        assert_eq!(rendered.contains("\"status\""), true);
    }
}
