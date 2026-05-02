use strum::IntoEnumIterator;
use strum_macros::AsRefStr;
use strum_macros::EnumIter;
use strum_macros::EnumString;
use strum_macros::IntoStaticStr;

/// Commands that can be invoked by starting a message with a leading slash.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, EnumString, EnumIter, AsRefStr, IntoStaticStr,
)]
#[strum(serialize_all = "kebab-case")]
pub enum SlashCommand {
    // DO NOT ALPHA-SORT! Enum order is presentation order in the popup, so
    // more frequently used commands should be listed first.
    Model,
    Fast,
    // Merge-safety anchor: keep `/permissions`, `/stop`, and `/subagents` as the
    // only active local legacy slash-command canon; do not reintroduce
    // `/approvals`, `/clean`, or `/agent` peers.
    Permissions,
    Keymap,
    #[strum(serialize = "setup-default-sandbox")]
    ElevateSandbox,
    #[strum(serialize = "sandbox-add-read-dir")]
    SandboxReadRoot,
    Experimental,
    #[strum(to_string = "autoreview")]
    AutoReview,
    Memories,
    Skills,
    Review,
    Rename,
    New,
    Resume,
    Fork,
    Init,
    Compact,
    Sanitize,
    Plan,
    Goal,
    Collab,
    #[strum(to_string = "subagents")]
    MultiAgents,
    Side,
    // Undo,
    Copy,
    Debug,
    Diff,
    Mention,
    Status,
    DebugConfig,
    Title,
    Statusline,
    Theme,
    Mcp,
    Apps,
    Accounts,
    Plugins,
    Logout,
    Quit,
    Exit,
    Feedback,
    Rollout,
    Ps,
    Stop,
    Clear,
    Personality,
    Realtime,
    Settings,
    TestApproval,
    // Debugging commands.
    #[strum(serialize = "debug-m-drop")]
    MemoryDrop,
    #[strum(serialize = "debug-m-update")]
    MemoryUpdate,
}

impl SlashCommand {
    /// User-visible description shown in the popup.
    pub fn description(self) -> &'static str {
        match self {
            SlashCommand::Feedback => "send logs to maintainers",
            SlashCommand::New => "start a new chat during a conversation",
            SlashCommand::Init => "create an AGENTS.md file with instructions for Codex",
            SlashCommand::Compact => "summarize conversation to prevent hitting the context limit",
            SlashCommand::Sanitize => "sanitize session context",
            SlashCommand::Review => "review my current changes and find issues",
            SlashCommand::Rename => "rename the current thread",
            SlashCommand::Resume => "resume a saved chat",
            SlashCommand::Clear => "clear the terminal and start a new chat",
            SlashCommand::Fork => "fork the current chat",
            // SlashCommand::Undo => "ask Codex to undo a turn",
            SlashCommand::Quit | SlashCommand::Exit => "exit Codex",
            SlashCommand::Copy => "copy the latest Codex output to your clipboard",
            SlashCommand::Diff => "show git diff (including untracked files)",
            SlashCommand::Debug => "show the latest raw API response item",
            SlashCommand::Mention => "mention a file",
            SlashCommand::Skills => "use skills to improve how Codex performs specific tasks",
            SlashCommand::Status => "show current session configuration and token usage",
            SlashCommand::DebugConfig => "show config layers and requirement sources for debugging",
            SlashCommand::Title => "configure which items appear in the terminal title",
            SlashCommand::Statusline => "configure which items appear in the status line",
            SlashCommand::Theme => "choose a syntax highlighting theme",
            SlashCommand::Ps => "list background terminals",
            SlashCommand::Stop => "stop all background terminals",
            SlashCommand::MemoryDrop => "DO NOT USE",
            SlashCommand::MemoryUpdate => "DO NOT USE",
            SlashCommand::Model => "choose what model and reasoning effort to use",
            SlashCommand::Fast => {
                "toggle Fast mode to enable fastest inference with increased plan usage"
            }
            SlashCommand::Personality => "choose a communication style for Codex",
            SlashCommand::Realtime => "toggle realtime voice mode (experimental)",
            SlashCommand::Settings => "configure realtime microphone/speaker",
            SlashCommand::Plan => "switch to Plan mode",
            SlashCommand::Goal => "set or view the goal for a long-running task",
            SlashCommand::Collab => "change collaboration mode (experimental)",
            SlashCommand::MultiAgents => "switch the active subagent thread",
            SlashCommand::Side => "start a side conversation in an ephemeral fork",
            SlashCommand::Permissions => "choose what Codex is allowed to do",
            SlashCommand::Keymap => "remap TUI shortcuts",
            SlashCommand::ElevateSandbox => "set up elevated agent sandbox",
            SlashCommand::SandboxReadRoot => {
                "let sandbox read a directory: /sandbox-add-read-dir <absolute_path>"
            }
            SlashCommand::Experimental => "toggle experimental features",
            SlashCommand::AutoReview => "approve one retry of a recent auto-review denial",
            SlashCommand::Memories => "configure memory use and generation",
            SlashCommand::Mcp => "list configured MCP tools; use /mcp verbose for details",
            SlashCommand::Apps => "manage apps",
            SlashCommand::Accounts => "manage ChatGPT accounts",
            SlashCommand::Plugins => "browse plugins",
            SlashCommand::Logout => "log out of Codex",
            SlashCommand::Rollout => "print the rollout file path",
            SlashCommand::TestApproval => "test approval request",
        }
    }

    /// Command string without the leading '/'. Provided for compatibility with
    /// existing code that expects a method named `command()`.
    pub fn command(self) -> &'static str {
        self.into()
    }

    /// Whether this command supports inline args (for example `/review ...`).
    pub fn supports_inline_args(self) -> bool {
        matches!(
            self,
            SlashCommand::Review
                | SlashCommand::Rename
                | SlashCommand::Plan
                | SlashCommand::Goal
                | SlashCommand::Fast
                | SlashCommand::Mcp
                | SlashCommand::Side
                | SlashCommand::Resume
                | SlashCommand::SandboxReadRoot
        )
    }

    /// Whether this command remains available inside an active side conversation.
    pub fn available_in_side_conversation(self) -> bool {
        matches!(
            self,
            SlashCommand::Copy | SlashCommand::Diff | SlashCommand::Mention | SlashCommand::Status
        )
    }

    /// Whether this command can be run while a task is in progress.
    /// Merge-safety anchor: `/accounts` and `/logout` stay blocked during active turns
    /// because account mutations are coordinated through app-level auth flows.
    pub fn available_during_task(self) -> bool {
        match self {
            SlashCommand::New
            | SlashCommand::Resume
            | SlashCommand::Fork
            | SlashCommand::Init
            | SlashCommand::Compact
            | SlashCommand::Sanitize
            // | SlashCommand::Undo
            | SlashCommand::Model
            | SlashCommand::Fast
            | SlashCommand::Personality
            | SlashCommand::Permissions
            | SlashCommand::Keymap
            | SlashCommand::ElevateSandbox
            | SlashCommand::SandboxReadRoot
            | SlashCommand::Experimental
            | SlashCommand::Memories
            | SlashCommand::Review
            | SlashCommand::Plan
            | SlashCommand::Accounts
            | SlashCommand::Clear
            | SlashCommand::Logout
            | SlashCommand::Theme
            | SlashCommand::MemoryDrop
            | SlashCommand::MemoryUpdate => false,
            SlashCommand::Diff
            | SlashCommand::Copy
            | SlashCommand::Debug
            | SlashCommand::Rename
            | SlashCommand::Mention
            | SlashCommand::Skills
            | SlashCommand::Status
            | SlashCommand::DebugConfig
            | SlashCommand::Ps
            | SlashCommand::Stop
            | SlashCommand::Goal
            | SlashCommand::Mcp
            | SlashCommand::Apps
            | SlashCommand::Plugins
            | SlashCommand::AutoReview
            | SlashCommand::Title
            | SlashCommand::Statusline
            | SlashCommand::Feedback
            | SlashCommand::Quit
            | SlashCommand::Exit
            | SlashCommand::Side => true,
            SlashCommand::Rollout => true,
            SlashCommand::TestApproval => true,
            SlashCommand::Realtime => true,
            SlashCommand::Settings => true,
            SlashCommand::Collab => true,
            SlashCommand::MultiAgents => true,
        }
    }

    fn is_visible(self) -> bool {
        match self {
            SlashCommand::SandboxReadRoot => cfg!(target_os = "windows"),
            SlashCommand::Copy => !cfg!(target_os = "android"),
            SlashCommand::Rollout | SlashCommand::TestApproval => cfg!(debug_assertions),
            _ => true,
        }
    }
}

/// Return all built-in commands in a Vec paired with their command string.
pub fn built_in_slash_commands() -> Vec<(&'static str, SlashCommand)> {
    SlashCommand::iter()
        .filter(|command| command.is_visible())
        .map(|c| (c.command(), c))
        .collect()
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use std::str::FromStr;

    use super::SlashCommand;

    #[test]
    fn task_availability_preserves_existing_commands() {
        assert_eq!(SlashCommand::DebugConfig.available_during_task(), true);
        assert_eq!(SlashCommand::Debug.available_during_task(), true);
        assert_eq!(SlashCommand::Stop.available_during_task(), true);
        assert_eq!(SlashCommand::Sanitize.available_during_task(), false);
        assert_eq!(SlashCommand::Accounts.available_during_task(), false);
        assert_eq!(SlashCommand::Logout.available_during_task(), false);
    }

    #[test]
    fn stop_command_is_canonical_name() {
        assert_eq!(SlashCommand::Stop.command(), "stop");
    }

    #[test]
    fn fast_command_description_uses_current_plan_usage_copy() {
        let description = SlashCommand::Fast.description();

        assert!(description.contains("increased plan usage"));
        assert!(!description.contains("2X plan usage"));
    }

    #[test]
    fn clean_is_rejected_as_removed_alias() {
        assert!(SlashCommand::from_str("clean").is_err());
    }

    #[test]
    fn certain_commands_are_available_during_task() {
        assert!(SlashCommand::Goal.available_during_task());
        assert!(SlashCommand::Title.available_during_task());
        assert!(SlashCommand::Statusline.available_during_task());
    }

    #[test]
    fn auto_review_command_is_autoreview() {
        assert_eq!(SlashCommand::AutoReview.command(), "autoreview");
        assert_eq!(
            SlashCommand::from_str("autoreview"),
            Ok(SlashCommand::AutoReview)
        );
    }
}
