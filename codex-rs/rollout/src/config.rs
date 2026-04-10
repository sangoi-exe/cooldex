use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use codex_protocol::config_types::SubagentFileMutationMode;

// Merge-safety anchor: rollout config must persist child file-mutation mode so resumed subagents can rehydrate the spawn-only restriction.

pub trait RolloutConfigView {
    fn codex_home(&self) -> &Path;
    fn sqlite_home(&self) -> &Path;
    fn cwd(&self) -> &Path;
    fn model_provider_id(&self) -> &str;
    fn generate_memories(&self) -> bool;
    fn subagent_file_mutation_mode(&self) -> SubagentFileMutationMode {
        SubagentFileMutationMode::Inherit
    }
    fn active_user_config_path(&self) -> std::io::Result<Option<PathBuf>> {
        Ok(None)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RolloutConfig {
    pub codex_home: PathBuf,
    pub sqlite_home: PathBuf,
    pub cwd: PathBuf,
    pub model_provider_id: String,
    pub generate_memories: bool,
    pub subagent_file_mutation_mode: SubagentFileMutationMode,
}

pub type Config = RolloutConfig;

impl RolloutConfig {
    pub fn from_view(view: &impl RolloutConfigView) -> Self {
        Self {
            codex_home: view.codex_home().to_path_buf(),
            sqlite_home: view.sqlite_home().to_path_buf(),
            cwd: view.cwd().to_path_buf(),
            model_provider_id: view.model_provider_id().to_string(),
            generate_memories: view.generate_memories(),
            subagent_file_mutation_mode: view.subagent_file_mutation_mode(),
        }
    }
}

impl RolloutConfigView for RolloutConfig {
    fn codex_home(&self) -> &Path {
        self.codex_home.as_path()
    }

    fn sqlite_home(&self) -> &Path {
        self.sqlite_home.as_path()
    }

    fn cwd(&self) -> &Path {
        self.cwd.as_path()
    }

    fn model_provider_id(&self) -> &str {
        self.model_provider_id.as_str()
    }

    fn generate_memories(&self) -> bool {
        self.generate_memories
    }

    fn subagent_file_mutation_mode(&self) -> SubagentFileMutationMode {
        self.subagent_file_mutation_mode
    }

    fn active_user_config_path(&self) -> std::io::Result<Option<PathBuf>> {
        Ok(None)
    }
}

impl<T: RolloutConfigView + ?Sized> RolloutConfigView for &T {
    fn codex_home(&self) -> &Path {
        (*self).codex_home()
    }

    fn sqlite_home(&self) -> &Path {
        (*self).sqlite_home()
    }

    fn cwd(&self) -> &Path {
        (*self).cwd()
    }

    fn model_provider_id(&self) -> &str {
        (*self).model_provider_id()
    }

    fn generate_memories(&self) -> bool {
        (*self).generate_memories()
    }

    fn subagent_file_mutation_mode(&self) -> SubagentFileMutationMode {
        (*self).subagent_file_mutation_mode()
    }

    fn active_user_config_path(&self) -> std::io::Result<Option<PathBuf>> {
        (*self).active_user_config_path()
    }
}

impl<T: RolloutConfigView + ?Sized> RolloutConfigView for Arc<T> {
    fn codex_home(&self) -> &Path {
        self.as_ref().codex_home()
    }

    fn sqlite_home(&self) -> &Path {
        self.as_ref().sqlite_home()
    }

    fn cwd(&self) -> &Path {
        self.as_ref().cwd()
    }

    fn model_provider_id(&self) -> &str {
        self.as_ref().model_provider_id()
    }

    fn generate_memories(&self) -> bool {
        self.as_ref().generate_memories()
    }

    fn subagent_file_mutation_mode(&self) -> SubagentFileMutationMode {
        self.as_ref().subagent_file_mutation_mode()
    }

    fn active_user_config_path(&self) -> std::io::Result<Option<PathBuf>> {
        self.as_ref().active_user_config_path()
    }
}
