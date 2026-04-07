use codex_app_server_protocol::Turn;
use codex_app_server_protocol::truncate_turns_since_last_context_compaction;
use codex_config::types::ResumeHistoryMode;
use codex_core::config::Config;

// Merge-safety anchor: all TUI-visible persisted-session replay paths must use
// this shared `[tui].resume_history` owner so plain and app-server-backed resume
// stay aligned on the last visible `Context compacted` boundary.
pub(crate) fn apply_resume_history_mode(config: &Config, turns: Vec<Turn>) -> Vec<Turn> {
    match config.tui_resume_history {
        ResumeHistoryMode::Full => turns,
        ResumeHistoryMode::SinceLastCompaction => {
            truncate_turns_since_last_context_compaction(turns)
        }
    }
}
