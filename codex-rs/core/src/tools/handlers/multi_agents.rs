//! Implements the collaboration tool surface for spawning and managing sub-agents.
//!
//! This handler translates model tool calls into `AgentControl` operations and keeps spawned
//! agents aligned with the live turn that created them. Sub-agents start from the turn's effective
//! config, inherit runtime-only state such as provider, approval policy, sandbox, and cwd, and
//! then optionally layer role-specific config on top.

use crate::agent::AgentRuntimeState;
use crate::agent::AgentStatus;
use crate::agent::exceeds_thread_spawn_depth_limit;
use crate::codex::Session;
use crate::codex::TurnContext;
use crate::config::Config;
use crate::config::ConfigOverrides;
use crate::config::deserialize_config_toml_with_base;
use crate::error::CodexErr;
use crate::features::Feature;
use crate::function_tool::FunctionCallError;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;
use async_trait::async_trait;
use codex_protocol::ThreadId;
use codex_protocol::config_types::WindowsSandboxLevel;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::protocol::CollabAgentInteractionBeginEvent;
use codex_protocol::protocol::CollabAgentInteractionEndEvent;
use codex_protocol::protocol::CollabAgentRef;
use codex_protocol::protocol::CollabAgentSpawnBeginEvent;
use codex_protocol::protocol::CollabAgentSpawnEndEvent;
use codex_protocol::protocol::CollabAgentStatusEntry;
use codex_protocol::protocol::CollabCloseBeginEvent;
use codex_protocol::protocol::CollabCloseEndEvent;
use codex_protocol::protocol::CollabResumeBeginEvent;
use codex_protocol::protocol::CollabResumeEndEvent;
use codex_protocol::protocol::CollabWaitingBeginEvent;
use codex_protocol::protocol::CollabWaitingEndEvent;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::user_input::UserInput;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::sync::Arc;

pub(crate) use close_agent::Handler as CloseAgentHandler;
pub(crate) use resume_agent::Handler as ResumeAgentHandler;
pub(crate) use send_input::Handler as SendInputHandler;
pub(crate) use spawn::Handler as SpawnAgentHandler;
pub(crate) use wait::Handler as WaitAgentHandler;

/// Minimum wait timeout to prevent tight polling loops from burning CPU.
pub(crate) const MIN_WAIT_TIMEOUT_MS: i64 = 10_000;
pub(crate) const DEFAULT_WAIT_TIMEOUT_MS: i64 = 30_000;
pub(crate) const MAX_WAIT_TIMEOUT_MS: i64 = 3600 * 1000;

#[derive(Debug, Deserialize)]
struct CloseAgentArgs {
    id: String,
}

fn function_arguments(payload: ToolPayload) -> Result<String, FunctionCallError> {
    match payload {
        ToolPayload::Function { arguments } => Ok(arguments),
        _ => Err(FunctionCallError::RespondToModel(
            "collab handler received unsupported payload".to_string(),
        )),
    }
}

fn tool_output_json_text<T>(value: &T, tool_name: &str) -> String
where
    T: Serialize,
{
    serde_json::to_string(value).unwrap_or_else(|err| {
        JsonValue::String(format!("failed to serialize {tool_name} result: {err}")).to_string()
    })
}

fn tool_output_response_item<T>(
    call_id: &str,
    payload: &ToolPayload,
    value: &T,
    success: Option<bool>,
    tool_name: &str,
) -> ResponseInputItem
where
    T: Serialize,
{
    FunctionToolOutput::from_text(tool_output_json_text(value, tool_name), success)
        .to_response_item(call_id, payload)
}

fn tool_output_code_mode_result<T>(value: &T, tool_name: &str) -> JsonValue
where
    T: Serialize,
{
    serde_json::to_value(value).unwrap_or_else(|err| {
        JsonValue::String(format!("failed to serialize {tool_name} result: {err}"))
    })
}

pub mod close_agent;
mod resume_agent;
mod send_input;
mod spawn;
pub(crate) mod wait;

fn agent_id(id: &str) -> Result<ThreadId, FunctionCallError> {
    ThreadId::from_string(id)
        .map_err(|e| FunctionCallError::RespondToModel(format!("invalid agent id {id}: {e:?}")))
}

fn build_wait_agent_statuses(
    agent_states: &HashMap<ThreadId, AgentRuntimeState>,
    receiver_agents: &[CollabAgentRef],
) -> Vec<CollabAgentStatusEntry> {
    if agent_states.is_empty() {
        return Vec::new();
    }

    let mut entries = Vec::with_capacity(agent_states.len());
    let mut seen = HashMap::with_capacity(receiver_agents.len());
    for receiver_agent in receiver_agents {
        seen.insert(receiver_agent.thread_id, ());
        if let Some(state) = agent_states.get(&receiver_agent.thread_id) {
            entries.push(CollabAgentStatusEntry {
                thread_id: receiver_agent.thread_id,
                agent_nickname: receiver_agent.agent_nickname.clone(),
                agent_role: receiver_agent.agent_role.clone(),
                status: state.status.clone(),
                last_activity: state.last_activity.clone(),
            });
        }
    }

    let mut extras = agent_states
        .iter()
        .filter(|(thread_id, _)| !seen.contains_key(thread_id))
        .map(|(thread_id, state)| CollabAgentStatusEntry {
            thread_id: *thread_id,
            agent_nickname: None,
            agent_role: None,
            status: state.status.clone(),
            last_activity: state.last_activity.clone(),
        })
        .collect::<Vec<_>>();
    extras.sort_by(|left, right| left.thread_id.to_string().cmp(&right.thread_id.to_string()));
    entries.extend(extras);
    entries
}

fn collab_spawn_error(err: CodexErr) -> FunctionCallError {
    match err {
        CodexErr::UnsupportedOperation(_) => {
            FunctionCallError::RespondToModel("collab manager unavailable".to_string())
        }
        err => FunctionCallError::RespondToModel(format!("collab spawn failed: {err}")),
    }
}

fn collab_agent_error(agent_id: ThreadId, err: CodexErr) -> FunctionCallError {
    match err {
        CodexErr::ThreadNotFound(id) => {
            FunctionCallError::RespondToModel(format!("agent with id {id} not found"))
        }
        CodexErr::InternalAgentDied => {
            FunctionCallError::RespondToModel(format!("agent with id {agent_id} is closed"))
        }
        CodexErr::UnsupportedOperation(_) => {
            FunctionCallError::RespondToModel("collab manager unavailable".to_string())
        }
        err => FunctionCallError::RespondToModel(format!("collab tool failed: {err}")),
    }
}

fn thread_spawn_source(
    parent_thread_id: ThreadId,
    depth: i32,
    agent_role: Option<&str>,
) -> SessionSource {
    SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
        parent_thread_id,
        depth,
        agent_nickname: None,
        agent_role: agent_role.map(str::to_string),
    })
}

fn parse_collab_input(
    message: Option<String>,
    items: Option<Vec<UserInput>>,
) -> Result<Vec<UserInput>, FunctionCallError> {
    match (message, items) {
        (Some(_), Some(_)) => Err(FunctionCallError::RespondToModel(
            "Provide either message or items, but not both".to_string(),
        )),
        (None, None) => Err(FunctionCallError::RespondToModel(
            "Provide one of: message or items".to_string(),
        )),
        (Some(message), None) => {
            if message.trim().is_empty() {
                return Err(FunctionCallError::RespondToModel(
                    "Empty message can't be sent to an agent".to_string(),
                ));
            }
            Ok(vec![UserInput::Text {
                text: message,
                text_elements: Vec::new(),
            }])
        }
        (None, Some(items)) => {
            if items.is_empty() {
                return Err(FunctionCallError::RespondToModel(
                    "Items can't be empty".to_string(),
                ));
            }
            Ok(items)
        }
    }
}

fn input_preview(items: &[UserInput]) -> String {
    let parts: Vec<String> = items
        .iter()
        .map(|item| match item {
            UserInput::Text { text, .. } => text.clone(),
            UserInput::Image { .. } => "[image]".to_string(),
            UserInput::LocalImage { path } => format!("[local_image:{}]", path.display()),
            UserInput::Skill { name, path } => {
                format!("[skill:${name}]({})", path.display())
            }
            UserInput::Mention { name, path } => format!("[mention:${name}]({path})"),
            _ => "[input]".to_string(),
        })
        .collect();

    parts.join("\n")
}

/// Builds the base config snapshot for a newly spawned sub-agent.
///
/// The returned config starts from the parent's effective config and then refreshes the
/// runtime-owned fields carried on `turn`, including model selection, reasoning settings,
/// approval policy, sandbox, and cwd. Role-specific overrides are layered after this step;
/// skipping this helper and cloning stale config state directly can send the child agent out with
/// the wrong provider or runtime policy.
pub(crate) fn build_agent_spawn_config(turn: &TurnContext) -> Result<Config, FunctionCallError> {
    let mut config = build_agent_shared_config(turn)?;
    let subagent_instructions = config.subagent_base_instructions.clone();
    // Merge-safety anchor: seed child base instructions for the no-reload path here. If role or
    // profile reloads rebuild the config later, `finalize_spawn_agent_prompt_config` recomputes
    // this same source from the child config that actually ships.
    config.base_instructions = Some(
        subagent_instructions
            .unwrap_or_else(|| turn.model_info.get_model_instructions(turn.personality)),
    );
    Ok(config)
}

fn build_agent_resume_config(
    turn: &TurnContext,
    child_depth: i32,
) -> Result<Config, FunctionCallError> {
    let mut config = build_agent_shared_config(turn)?;
    apply_spawn_agent_overrides(&mut config, child_depth);
    // For resume, keep base instructions sourced from rollout/session metadata.
    config.base_instructions = None;
    Ok(config)
}

fn build_agent_shared_config(turn: &TurnContext) -> Result<Config, FunctionCallError> {
    let mut config = turn.config.as_ref().clone();
    config.model = Some(turn.model_info.slug.clone());
    config.model_provider = turn.provider.clone();
    // Merge-safety anchor: child spawn config must trust the already-materialized turn context for
    // effective reasoning effort so inherited-vs-explicit ownership stays in `SessionConfiguration`
    // instead of being re-decided here.
    config.model_reasoning_effort = turn.reasoning_effort;
    config.model_reasoning_summary = Some(turn.reasoning_summary);
    strip_child_prompt_inheritance(&mut config);
    config.compact_prompt = turn.compact_prompt.clone();
    apply_spawn_agent_runtime_overrides(&mut config, turn)?;

    Ok(config)
}

fn strip_child_prompt_inheritance(config: &mut Config) {
    // Merge-safety anchor: child agents must still drop inherited user/project-doc prompt state
    // and lead-only post-compact ritual, but child `developer_instructions` now stay available so
    // role files and lead config can intentionally specialize spawned sub-agents. Re-run this
    // after any role/profile reload that reconstructs `Config` from persisted layers.
    config.user_instructions = None;
    config.pos_compact_instructions =
        Some(crate::codex::SUBAGENT_AUTO_COMPACT_RECALL_WARNING_BODY.to_string());
    config.project_doc_max_bytes = 0;
    let _ = config.features.disable(Feature::ChildAgentsMd);
}

async fn finalize_spawn_agent_prompt_config(
    config: &mut Config,
    turn: &TurnContext,
    models_manager: &crate::models_manager::manager::ModelsManager,
) {
    // Merge-safety anchor: role/profile reloads rebuild `Config` from persisted layers, which can
    // repopulate user/project-doc context and feature flags. Normalize the child prompt after the
    // final reload so sub-agents keep intended developer instructions while still dropping
    // lead-only user/project-doc inheritance and deriving base instructions from the child's final
    // model/personality selection.
    strip_child_prompt_inheritance(config);
    let model = config
        .model
        .clone()
        .unwrap_or_else(|| turn.model_info.slug.clone());
    let model_info = models_manager.get_model_info(model.as_str(), config).await;
    if !model_info.used_fallback_model_metadata
        && let Some(reasoning_effort) = config.model_reasoning_effort
        && !model_info
            .supported_reasoning_levels
            .iter()
            .any(|preset| preset.effort == reasoning_effort)
    {
        let normalized_reasoning_effort = model_info.default_reasoning_level;
        tracing::warn!(
            model = model.as_str(),
            ?reasoning_effort,
            ?normalized_reasoning_effort,
            "spawn_agent reasoning effort unsupported by final child model; normalizing to model default"
        );
        config.model_reasoning_effort = normalized_reasoning_effort;
    }
    config.base_instructions = Some(
        config
            .subagent_base_instructions
            .clone()
            .unwrap_or_else(|| model_info.get_model_instructions(config.personality)),
    );
}

fn apply_spawn_agent_profile_override(
    config: &mut Config,
    profile_name: Option<&str>,
) -> Result<(), FunctionCallError> {
    let Some(profile_name) = profile_name else {
        return Ok(());
    };

    // Merge-safety anchor: profile reload is the only spawn-time path that may replace inherited
    // child model/reasoning settings now that direct spawn-agent overrides are gone.
    let merged_toml = config.config_layer_stack.effective_config();
    let merged_config = deserialize_config_toml_with_base(merged_toml, &config.codex_home)
        .map_err(|err| {
            FunctionCallError::RespondToModel(format!(
                "failed to load config for profile `{profile_name}`: {err}"
            ))
        })?;

    merged_config
        .get_config_profile(Some(profile_name.to_string()))
        .map_err(|_| {
            FunctionCallError::RespondToModel(format!("config profile `{profile_name}` not found"))
        })?;

    let next_config = Config::load_config_with_layer_stack(
        merged_config,
        ConfigOverrides {
            config_profile: Some(profile_name.to_string()),
            cwd: Some(config.cwd.clone()),
            codex_linux_sandbox_exe: config.codex_linux_sandbox_exe.clone(),
            main_execve_wrapper_exe: config.main_execve_wrapper_exe.clone(),
            js_repl_node_path: config.js_repl_node_path.clone(),
            js_repl_node_module_dirs: Some(config.js_repl_node_module_dirs.clone()),
            zsh_path: config.zsh_path.clone(),
            ..Default::default()
        },
        config.codex_home.clone(),
        config.config_layer_stack.clone(),
    )
    .map_err(|err| {
        FunctionCallError::RespondToModel(format!(
            "failed to apply profile `{profile_name}`: {err}"
        ))
    })?;

    *config = next_config;
    Ok(())
}

/// Copies runtime-only turn state onto a child config before it is handed to `AgentControl`.
///
/// These values are chosen by the live turn rather than persisted config, so leaving them stale
/// can make a child agent disagree with its parent about approval policy, cwd, or sandboxing.
fn apply_spawn_agent_runtime_overrides(
    config: &mut Config,
    turn: &TurnContext,
) -> Result<(), FunctionCallError> {
    config
        .permissions
        .approval_policy
        .set(turn.approval_policy.value())
        .map_err(|err| {
            FunctionCallError::RespondToModel(format!("approval_policy is invalid: {err}"))
        })?;
    config.permissions.shell_environment_policy = turn.shell_environment_policy.clone();
    config.codex_linux_sandbox_exe = turn.codex_linux_sandbox_exe.clone();
    config.cwd = turn.cwd.clone();
    config
        .permissions
        .sandbox_policy
        .set(turn.sandbox_policy.get().clone())
        .map_err(|err| {
            FunctionCallError::RespondToModel(format!("sandbox_policy is invalid: {err}"))
        })?;
    config.permissions.file_system_sandbox_policy = turn.file_system_sandbox_policy.clone();
    config.permissions.network_sandbox_policy = turn.network_sandbox_policy;
    match turn.windows_sandbox_level {
        WindowsSandboxLevel::Elevated => {
            config.permissions.windows_sandbox_mode =
                Some(crate::config::types::WindowsSandboxModeToml::Elevated);
            config
                .features
                .enable(Feature::WindowsSandbox)
                .map_err(|err| {
                    FunctionCallError::RespondToModel(format!(
                        "windows sandbox features are invalid: {err}"
                    ))
                })?;
            config
                .features
                .enable(Feature::WindowsSandboxElevated)
                .map_err(|err| {
                    FunctionCallError::RespondToModel(format!(
                        "windows sandbox features are invalid: {err}"
                    ))
                })?;
        }
        WindowsSandboxLevel::RestrictedToken => {
            config.permissions.windows_sandbox_mode =
                Some(crate::config::types::WindowsSandboxModeToml::Unelevated);
            config
                .features
                .enable(Feature::WindowsSandbox)
                .map_err(|err| {
                    FunctionCallError::RespondToModel(format!(
                        "windows sandbox features are invalid: {err}"
                    ))
                })?;
            config
                .features
                .disable(Feature::WindowsSandboxElevated)
                .map_err(|err| {
                    FunctionCallError::RespondToModel(format!(
                        "windows sandbox features are invalid: {err}"
                    ))
                })?;
        }
        WindowsSandboxLevel::Disabled => {
            config.permissions.windows_sandbox_mode = None;
            config
                .features
                .disable(Feature::WindowsSandbox)
                .map_err(|err| {
                    FunctionCallError::RespondToModel(format!(
                        "windows sandbox features are invalid: {err}"
                    ))
                })?;
            config
                .features
                .disable(Feature::WindowsSandboxElevated)
                .map_err(|err| {
                    FunctionCallError::RespondToModel(format!(
                        "windows sandbox features are invalid: {err}"
                    ))
                })?;
        }
    }
    Ok(())
}

fn apply_spawn_agent_overrides(config: &mut Config, child_depth: i32) {
    if child_depth >= config.agent_max_depth {
        let _ = config.features.disable(Feature::SpawnCsv);
        let _ = config.features.disable(Feature::Collab);
    }
}

fn ensure_running_subagent_preemption_allowed(
    config: &Config,
    action: &str,
    target_agent_id: ThreadId,
    status: &AgentStatus,
) -> Result<(), FunctionCallError> {
    if config.agent_allow_running_subagent_preemption || crate::agent::status::is_final(status) {
        return Ok(());
    }

    Err(FunctionCallError::RespondToModel(format!(
        "agents.allow_running_subagent_preemption=false blocks {action} for active agent {target_agent_id} with status {status:?}"
    )))
}

async fn collect_current_agent_states(
    session: &Session,
    receiver_thread_ids: &[ThreadId],
) -> HashMap<ThreadId, AgentRuntimeState> {
    let mut states = HashMap::with_capacity(receiver_thread_ids.len());
    for thread_id in receiver_thread_ids {
        states.insert(
            *thread_id,
            session
                .services
                .agent_control
                .get_runtime_state(*thread_id)
                .await,
        );
    }
    states
}

fn current_statuses(
    agent_states: &HashMap<ThreadId, AgentRuntimeState>,
) -> HashMap<ThreadId, AgentStatus> {
    agent_states
        .iter()
        .map(|(thread_id, state)| (*thread_id, state.status.clone()))
        .collect()
}

fn collab_wait_state(
    return_when: codex_protocol::protocol::CollabWaitReturnWhen,
    condition_enabled: bool,
    disable_timeout: bool,
    timed_out: Option<bool>,
) -> codex_protocol::protocol::CollabWaitState {
    codex_protocol::protocol::CollabWaitState {
        return_when,
        disable_timeout,
        condition_enabled,
        timed_out,
    }
}

#[cfg(test)]
#[path = "multi_agents_tests.rs"]
mod tests;
