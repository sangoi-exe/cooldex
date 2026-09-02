use crate::config::Config;
use codex_features::Feature;
use codex_model_provider_info::ModelProviderInfo;
use codex_protocol::config_types::AutoCompactTokenLimitScope;
use codex_protocol::config_types::ReasoningSummary;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use std::fmt;
use std::sync::Arc;

#[derive(Clone, PartialEq)]
struct ModelProviderIdentity {
    id: String,
    info: ModelProviderInfo,
}

/// Effective model-facing identity that must move atomically with a V2 agent.
// Merge-safety anchor: V2 agent identity must carry effective role-specific
// context and compaction limits across eviction and cold reload.
#[derive(Clone, PartialEq)]
pub(crate) struct AgentIdentitySnapshot {
    agent_role: Option<String>,
    model_provider: ModelProviderIdentity,
    model: String,
    model_context_window: Option<Option<i64>>,
    model_auto_compact_token_limit: Option<Option<i64>>,
    model_auto_compact_token_limit_scope: Option<AutoCompactTokenLimitScope>,
    model_reasoning_effort: Option<ReasoningEffort>,
    model_reasoning_summary: Option<ReasoningSummary>,
    base_instructions: Arc<str>,
    developer_instructions: Option<Arc<str>>,
    service_tier: Option<String>,
    shell_tool_enabled: Option<bool>,
}

impl AgentIdentitySnapshot {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn capture(
        agent_role: Option<String>,
        model_provider_id: String,
        model_provider_info: ModelProviderInfo,
        model: String,
        model_context_window: Option<Option<i64>>,
        model_auto_compact_token_limit: Option<Option<i64>>,
        model_auto_compact_token_limit_scope: Option<AutoCompactTokenLimitScope>,
        model_reasoning_effort: Option<ReasoningEffort>,
        model_reasoning_summary: Option<ReasoningSummary>,
        base_instructions: String,
        developer_instructions: Option<String>,
        service_tier: Option<String>,
        shell_tool_enabled: Option<bool>,
    ) -> Self {
        Self {
            agent_role,
            model_provider: ModelProviderIdentity {
                id: model_provider_id,
                info: model_provider_info,
            },
            model,
            model_context_window,
            model_auto_compact_token_limit,
            model_auto_compact_token_limit_scope,
            model_reasoning_effort,
            model_reasoning_summary,
            base_instructions: Arc::from(base_instructions),
            developer_instructions: developer_instructions.map(Arc::from),
            service_tier,
            shell_tool_enabled,
        }
    }

    pub(crate) fn apply(
        &self,
        config: &mut Config,
        session_source: &mut SessionSource,
    ) -> CodexResult<()> {
        let SessionSource::SubAgent(SubAgentSource::ThreadSpawn { agent_role, .. }) =
            session_source
        else {
            return Err(CodexErr::Fatal(
                "agent identity can only be applied to a thread-spawn source".to_string(),
            ));
        };

        agent_role.clone_from(&self.agent_role);
        config.model_provider_id.clone_from(&self.model_provider.id);
        config.model_provider.clone_from(&self.model_provider.info);
        config.model = Some(self.model.clone());
        if let Some(model_context_window) = self.model_context_window {
            config.model_context_window = model_context_window;
        }
        if let Some(model_auto_compact_token_limit) = self.model_auto_compact_token_limit {
            config.model_auto_compact_token_limit = model_auto_compact_token_limit;
        }
        if let Some(model_auto_compact_token_limit_scope) =
            self.model_auto_compact_token_limit_scope
        {
            config.model_auto_compact_token_limit_scope = model_auto_compact_token_limit_scope;
        }
        config
            .model_reasoning_effort
            .clone_from(&self.model_reasoning_effort);
        config.model_reasoning_summary = self.model_reasoning_summary;
        config.base_instructions = Some(self.base_instructions.to_string());
        config.developer_instructions = self
            .developer_instructions
            .as_ref()
            .map(ToString::to_string);
        config.service_tier.clone_from(&self.service_tier);
        if let Some(shell_tool_enabled) = self.shell_tool_enabled {
            config
                .features
                .set_enabled(Feature::ShellTool, shell_tool_enabled)
                .map_err(|error| {
                    CodexErr::Fatal(format!(
                        "failed to restore V2 child shell-tool state: {error}"
                    ))
                })?;
        }
        Ok(())
    }
}

impl fmt::Debug for AgentIdentitySnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentIdentitySnapshot")
            .field("agent_role", &self.agent_role)
            .field("model_provider_id", &self.model_provider.id)
            .field("model_provider_info", &"<redacted>")
            .field("model", &self.model)
            .field("model_context_window", &self.model_context_window)
            .field(
                "model_auto_compact_token_limit",
                &self.model_auto_compact_token_limit,
            )
            .field(
                "model_auto_compact_token_limit_scope",
                &self.model_auto_compact_token_limit_scope,
            )
            .field("model_reasoning_effort", &self.model_reasoning_effort)
            .field("model_reasoning_summary", &self.model_reasoning_summary)
            .field("base_instructions", &"<redacted>")
            .field(
                "developer_instructions",
                &self.developer_instructions.as_ref().map(|_| "<redacted>"),
            )
            .field("service_tier", &self.service_tier)
            .field("shell_tool_enabled", &self.shell_tool_enabled)
            .finish()
    }
}

#[cfg(test)]
#[path = "identity_tests.rs"]
mod tests;
