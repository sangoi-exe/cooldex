use codex_protocol::items::AgentMessageContent;
use codex_protocol::items::TurnItem;
use codex_protocol::protocol::AgentStatus;
use codex_protocol::protocol::CollabAgentActivity;
use codex_protocol::protocol::CollabAgentActivityKind;
use codex_protocol::protocol::EventMsg;
use serde::Deserialize;
use serde::Serialize;
use std::path::PathBuf;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

const MAX_ACTIVITY_SUMMARY_CHARS: usize = 120;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct AgentRuntimeState {
    pub(crate) status: AgentStatus,
    pub(crate) last_activity: Option<CollabAgentActivity>,
}

/// Derive the next agent status from a single emitted event.
/// Returns `None` when the event does not affect status tracking.
pub(crate) fn agent_status_from_event(msg: &EventMsg) -> Option<AgentStatus> {
    match msg {
        EventMsg::TurnStarted(_) => Some(AgentStatus::Running),
        EventMsg::TurnComplete(ev) => Some(AgentStatus::Completed(ev.last_agent_message.clone())),
        EventMsg::TurnAborted(ev) => match ev.reason {
            codex_protocol::protocol::TurnAbortReason::Interrupted => {
                Some(AgentStatus::Interrupted)
            }
            _ => Some(AgentStatus::Errored(format!("{:?}", ev.reason))),
        },
        EventMsg::Error(ev) => Some(AgentStatus::Errored(ev.message.clone())),
        EventMsg::ShutdownComplete => Some(AgentStatus::Shutdown),
        _ => None,
    }
}

/// Derive the latest human-facing activity summary from a single emitted event.
/// Returns `None` when the event should not change progress tracking.
pub(crate) fn agent_last_activity_from_event(msg: &EventMsg) -> Option<CollabAgentActivity> {
    let occurred_at = unix_seconds_now();
    match msg {
        EventMsg::TurnStarted(_) => Some(activity(
            CollabAgentActivityKind::Status,
            "Started working".to_string(),
            occurred_at,
        )),
        EventMsg::TurnComplete(ev) => Some(activity(
            CollabAgentActivityKind::Message,
            match ev.last_agent_message.as_deref().map(preview_text) {
                Some(message) if !message.is_empty() => format!("Final response: {message}"),
                _ => "Completed turn".to_string(),
            },
            occurred_at,
        )),
        EventMsg::TurnAborted(ev) => Some(activity(
            CollabAgentActivityKind::Status,
            format!("Turn aborted: {:?}", ev.reason),
            occurred_at,
        )),
        EventMsg::Error(ev) => Some(activity(
            CollabAgentActivityKind::Status,
            format!("Error: {}", preview_text(&ev.message)),
            occurred_at,
        )),
        EventMsg::ShutdownComplete => Some(activity(
            CollabAgentActivityKind::Status,
            "Shutdown complete".to_string(),
            occurred_at,
        )),
        EventMsg::AgentMessageDelta(ev) => text_activity(
            CollabAgentActivityKind::Message,
            "Drafting response",
            &ev.delta,
            occurred_at,
        ),
        EventMsg::AgentMessageContentDelta(ev) => text_activity(
            CollabAgentActivityKind::Message,
            "Drafting response",
            &ev.delta,
            occurred_at,
        ),
        EventMsg::AgentReasoningDelta(ev) => text_activity(
            CollabAgentActivityKind::Reasoning,
            "Reasoning",
            &ev.delta,
            occurred_at,
        ),
        EventMsg::ReasoningContentDelta(ev) => text_activity(
            CollabAgentActivityKind::Reasoning,
            "Reasoning",
            &ev.delta,
            occurred_at,
        ),
        EventMsg::ExecCommandBegin(ev) => Some(activity(
            CollabAgentActivityKind::Command,
            format!("Running command: {}", preview_command(&ev.command)),
            occurred_at,
        )),
        EventMsg::ExecCommandOutputDelta(ev) => Some(activity(
            CollabAgentActivityKind::Command,
            format!(
                "Command output: {}",
                preview_bytes(ev.chunk.as_slice()).unwrap_or_else(|| "stream updated".to_string())
            ),
            occurred_at,
        )),
        EventMsg::ExecCommandEnd(ev) => Some(activity(
            CollabAgentActivityKind::Command,
            format!(
                "Command finished (exit {}): {}",
                ev.exit_code,
                preview_command(&ev.command)
            ),
            occurred_at,
        )),
        EventMsg::PatchApplyBegin(ev) => Some(activity(
            CollabAgentActivityKind::Edit,
            format!(
                "Applying patch: {}",
                patch_target_summary(ev.changes.keys().cloned().collect())
            ),
            occurred_at,
        )),
        EventMsg::PatchApplyEnd(ev) => Some(activity(
            CollabAgentActivityKind::Edit,
            if ev.success {
                format!(
                    "Applied patch: {}",
                    patch_target_summary(ev.changes.keys().cloned().collect())
                )
            } else {
                let stderr_preview = preview_text(ev.stderr.as_str());
                format!(
                    "Patch failed: {}",
                    if stderr_preview.is_empty() {
                        "see patch error".to_string()
                    } else {
                        stderr_preview
                    }
                )
            },
            occurred_at,
        )),
        EventMsg::ItemStarted(ev) => turn_item_activity("Started", &ev.item, occurred_at),
        EventMsg::ItemCompleted(ev) => turn_item_activity("Completed", &ev.item, occurred_at),
        _ => None,
    }
}

pub(crate) fn is_final(status: &AgentStatus) -> bool {
    !matches!(
        status,
        AgentStatus::PendingInit | AgentStatus::Running | AgentStatus::Interrupted
    )
}

fn turn_item_activity(
    prefix: &str,
    item: &TurnItem,
    occurred_at: i64,
) -> Option<CollabAgentActivity> {
    let (kind, summary) = match item {
        TurnItem::UserMessage(_) => return None,
        TurnItem::AgentMessage(item) => {
            let text = item
                .content
                .iter()
                .map(|content| match content {
                    AgentMessageContent::Text { text } => text.as_str(),
                })
                .collect::<Vec<_>>()
                .join(" ");
            (
                CollabAgentActivityKind::Message,
                format!("{prefix} response: {}", preview_text(&text)),
            )
        }
        TurnItem::Plan(item) => (
            CollabAgentActivityKind::Task,
            format!("{prefix} plan: {}", preview_text(item.text.as_str())),
        ),
        TurnItem::HookPrompt(item) => {
            let text = item
                .fragments
                .iter()
                .map(|fragment| fragment.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            (
                CollabAgentActivityKind::Task,
                format!("{prefix} hook prompt: {}", preview_text(text.as_str())),
            )
        }
        TurnItem::Reasoning(item) => {
            let text = item
                .summary_text
                .last()
                .or_else(|| item.raw_content.last())
                .map(String::as_str)
                .unwrap_or("reasoning updated");
            (
                CollabAgentActivityKind::Reasoning,
                format!("{prefix} reasoning: {}", preview_text(text)),
            )
        }
        TurnItem::WebSearch(item) => (
            CollabAgentActivityKind::Task,
            format!("{prefix} web search: {}", preview_text(item.query.as_str())),
        ),
        TurnItem::ImageGeneration(item) => (
            CollabAgentActivityKind::Task,
            format!(
                "{prefix} image generation: {}",
                preview_text(item.status.as_str())
            ),
        ),
        TurnItem::ContextCompaction(_) => (
            CollabAgentActivityKind::Task,
            format!("{prefix} context compaction"),
        ),
    };
    Some(activity(kind, summary, occurred_at))
}

fn text_activity(
    kind: CollabAgentActivityKind,
    prefix: &str,
    text: &str,
    occurred_at: i64,
) -> Option<CollabAgentActivity> {
    let summary = preview_text(text);
    if summary.is_empty() {
        return None;
    }
    Some(activity(kind, format!("{prefix}: {summary}"), occurred_at))
}

fn activity(
    kind: CollabAgentActivityKind,
    summary: String,
    occurred_at: i64,
) -> CollabAgentActivity {
    CollabAgentActivity {
        kind,
        summary,
        occurred_at,
    }
}

fn preview_command(command: &[String]) -> String {
    let joined = command.join(" ");
    if joined.trim().is_empty() {
        "command".to_string()
    } else {
        preview_text(&joined)
    }
}

fn patch_target_summary(mut paths: Vec<PathBuf>) -> String {
    if paths.is_empty() {
        return "no files".to_string();
    }
    paths.sort_by_key(|path| path.display().to_string());
    if paths.len() == 1 {
        return preview_text(&paths[0].display().to_string());
    }
    format!("{} files", paths.len())
}

fn preview_bytes(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    let preview = preview_text(&text);
    if preview.is_empty() {
        None
    } else {
        Some(preview)
    }
}

fn preview_text(text: &str) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= MAX_ACTIVITY_SUMMARY_CHARS {
        normalized
    } else {
        normalized
            .chars()
            .take(MAX_ACTIVITY_SUMMARY_CHARS.saturating_sub(1))
            .collect::<String>()
            + "..."
    }
}

fn unix_seconds_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::protocol::AgentMessageDeltaEvent;
    use codex_protocol::protocol::EventMsg;
    use codex_protocol::protocol::FileChange;
    use codex_protocol::protocol::PatchApplyBeginEvent;
    use pretty_assertions::assert_eq;
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[test]
    fn agent_message_delta_updates_last_activity() {
        let activity =
            agent_last_activity_from_event(&EventMsg::AgentMessageDelta(AgentMessageDeltaEvent {
                delta: "Working through the data-flow rewrite.".to_string(),
            }))
            .expect("message delta should update activity");

        assert_eq!(activity.kind, CollabAgentActivityKind::Message);
        assert_eq!(
            activity.summary,
            "Drafting response: Working through the data-flow rewrite.".to_string()
        );
        assert!(activity.occurred_at > 0);
    }

    #[test]
    fn patch_begin_updates_last_activity() {
        let activity =
            agent_last_activity_from_event(&EventMsg::PatchApplyBegin(PatchApplyBeginEvent {
                call_id: "call-1".to_string(),
                turn_id: "turn-1".to_string(),
                auto_approved: true,
                changes: HashMap::from([(
                    PathBuf::from("src/lib.rs"),
                    FileChange::Add {
                        content: "fn main() {}".to_string(),
                    },
                )]),
            }))
            .expect("patch begin should update activity");

        assert_eq!(activity.kind, CollabAgentActivityKind::Edit);
        assert_eq!(activity.summary, "Applying patch: src/lib.rs".to_string());
        assert!(activity.occurred_at > 0);
    }
}
