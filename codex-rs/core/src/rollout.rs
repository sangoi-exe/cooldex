use crate::config::Config;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::SandboxPolicy;
pub use codex_rollout::ARCHIVED_SESSIONS_SUBDIR;
pub use codex_rollout::Cursor;
pub use codex_rollout::EventPersistenceMode;
pub use codex_rollout::INTERACTIVE_SESSION_SOURCES;
pub use codex_rollout::RolloutRecorder;
pub use codex_rollout::RolloutRecorderParams;
pub use codex_rollout::SESSIONS_SUBDIR;
pub use codex_rollout::SessionMeta;
pub use codex_rollout::SortDirection;
pub use codex_rollout::ThreadItem;
pub use codex_rollout::ThreadSortKey;
pub use codex_rollout::ThreadsPage;
pub use codex_rollout::append_thread_name;
pub use codex_rollout::find_archived_thread_path_by_id_str;
#[deprecated(note = "use find_thread_path_by_id_str")]
pub use codex_rollout::find_conversation_path_by_id_str;
pub use codex_rollout::find_thread_meta_by_name_str;
pub use codex_rollout::find_thread_name_by_id;
pub use codex_rollout::find_thread_names_by_ids;
pub use codex_rollout::find_thread_path_by_id_str;
pub use codex_rollout::parse_cursor;
pub use codex_rollout::read_head_for_summary;
pub use codex_rollout::read_session_meta_line;
pub use codex_rollout::rollout_date_parts;
use std::path::Path;

// Merge-safety anchor: resumed child sandbox restoration must prefer the latest persisted
// TurnContext baseline, then the rollout-owned SessionConfigured event, before any follower-state
// fallback is considered.
pub(crate) async fn read_resumed_child_sandbox_policy(
    path: &Path,
) -> std::io::Result<Option<SandboxPolicy>> {
    let (items, _, _) = RolloutRecorder::load_rollout_items_skipping_malformed_lines(path).await?;
    if let Some(turn_context) = items.iter().rev().find_map(|item| match item {
        RolloutItem::TurnContext(turn_context) => Some(turn_context),
        _ => None,
    }) {
        return Ok(Some(turn_context.sandbox_policy.clone()));
    }
    Ok(items.into_iter().find_map(|item| match item {
        RolloutItem::EventMsg(EventMsg::SessionConfigured(event)) => Some(event.sandbox_policy),
        _ => None,
    }))
}

impl codex_rollout::RolloutConfigView for Config {
    fn codex_home(&self) -> &std::path::Path {
        self.codex_home.as_path()
    }

    fn sqlite_home(&self) -> &std::path::Path {
        self.sqlite_home.as_path()
    }

    fn cwd(&self) -> &std::path::Path {
        self.cwd.as_path()
    }

    fn model_provider_id(&self) -> &str {
        self.model_provider_id.as_str()
    }

    fn generate_memories(&self) -> bool {
        self.memories.generate_memories
    }

    // Merge-safety anchor: spawned child file-mutation mode is persisted in rollout metadata and must survive resume without mutating lead-session defaults.
    fn subagent_file_mutation_mode(
        &self,
    ) -> codex_protocol::config_types::SubagentFileMutationMode {
        self.subagent_file_mutation_mode
    }

    fn active_user_config_path(&self) -> std::io::Result<Option<std::path::PathBuf>> {
        Config::active_user_config_path(self).map(Some)
    }
}

pub(crate) mod list {
    pub use codex_rollout::find_thread_path_by_id_str;
}

pub(crate) mod metadata {
    pub(crate) use codex_rollout::builder_from_items;
}

pub(crate) mod policy {
    pub use codex_rollout::EventPersistenceMode;
    pub use codex_rollout::should_persist_response_item_for_memories;
}

pub(crate) mod recorder {
    pub use codex_rollout::RolloutRecorder;
}

pub(crate) use crate::session_rollout_init_error::map_session_init_error;

pub(crate) mod truncation {
    pub(crate) use crate::thread_rollout_truncation::*;
}
