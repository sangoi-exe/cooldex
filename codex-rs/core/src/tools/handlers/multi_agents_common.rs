// Merge-safety anchor: shared collab helpers define the fail-loud operator contract and the
// single CLI-owned child-spawn config owner reused by legacy collab, MultiAgentV2, and agent jobs.
use crate::config::Config;
use crate::config::ConfigOverrides;
use crate::config::deserialize_config_toml_with_base;
use crate::function_tool::FunctionCallError;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use crate::subagent_file_mutation::apply_file_mutation_mode_to_config;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use codex_config::types::WindowsSandboxModeToml;
use codex_exec_server::LOCAL_FS;
use codex_features::Feature;
use codex_models_manager::manager::ModelsManager;
use codex_models_manager::manager::RefreshStrategy;
use codex_protocol::AgentPath;
use codex_protocol::ThreadId;
use codex_protocol::config_types::SubagentFileMutationMode;
use codex_protocol::config_types::WindowsSandboxLevel;
use codex_protocol::error::CodexErr;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::openai_models::ReasoningEffortPreset;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::user_input::UserInput;
use serde::Serialize;
use serde_json::Value as JsonValue;

/// Minimum wait timeout to prevent tight polling loops from burning CPU.
pub(crate) const MIN_WAIT_TIMEOUT_MS: i64 = 10_000;
pub(crate) const DEFAULT_WAIT_TIMEOUT_MS: i64 = 30_000;
pub(crate) const MAX_WAIT_TIMEOUT_MS: i64 = 3600 * 1000;

pub(crate) fn function_arguments(payload: ToolPayload) -> Result<String, FunctionCallError> {
    match payload {
        ToolPayload::Function { arguments } => Ok(arguments),
        _ => Err(FunctionCallError::RespondToModel(
            "collab handler received unsupported payload".to_string(),
        )),
    }
}

pub(crate) fn tool_output_json_text<T>(value: &T, tool_name: &str) -> String
where
    T: Serialize,
{
    serde_json::to_string(value).unwrap_or_else(|err| {
        JsonValue::String(format!("failed to serialize {tool_name} result: {err}")).to_string()
    })
}

pub(crate) fn tool_output_response_item<T>(
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

pub(crate) fn tool_output_code_mode_result<T>(value: &T, tool_name: &str) -> JsonValue
where
    T: Serialize,
{
    serde_json::to_value(value).unwrap_or_else(|err| {
        JsonValue::String(format!("failed to serialize {tool_name} result: {err}"))
    })
}

pub(crate) fn collab_spawn_error(err: CodexErr) -> FunctionCallError {
    match err {
        CodexErr::UnsupportedOperation(message) if message == "thread manager dropped" => {
            FunctionCallError::RespondToModel("collab manager unavailable".to_string())
        }
        CodexErr::UnsupportedOperation(message) => FunctionCallError::RespondToModel(message),
        err => FunctionCallError::RespondToModel(format!("collab spawn failed: {err}")),
    }
}

pub(crate) fn collab_agent_error(agent_id: ThreadId, err: CodexErr) -> FunctionCallError {
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

pub(crate) fn thread_spawn_source(
    parent_thread_id: ThreadId,
    parent_session_source: &SessionSource,
    depth: i32,
    agent_role: Option<&str>,
    task_name: Option<String>,
) -> Result<SessionSource, FunctionCallError> {
    let agent_path = task_name
        .as_deref()
        .map(|task_name| {
            parent_session_source
                .get_agent_path()
                .unwrap_or_else(AgentPath::root)
                .join(task_name)
                .map_err(FunctionCallError::RespondToModel)
        })
        .transpose()?;
    Ok(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
        parent_thread_id,
        depth,
        agent_path,
        agent_nickname: None,
        agent_role: agent_role.map(str::to_string),
    }))
}

pub(crate) fn parse_collab_input(
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

pub(crate) fn build_agent_resume_config(
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
    config.model_provider = turn.provider.info().clone();
    // Merge-safety anchor: child spawn config must trust the already-materialized turn context for
    // effective reasoning effort so inherited-vs-explicit ownership stays in `SessionConfiguration`
    // instead of being re-decided here.
    config.model_reasoning_effort = turn.reasoning_effort;
    config.model_reasoning_summary = Some(turn.reasoning_summary);
    apply_child_prompt_overrides(&mut config);
    config.compact_prompt = turn.compact_prompt.clone();
    apply_spawn_agent_runtime_overrides(&mut config, turn)?;

    Ok(config)
}

fn apply_child_prompt_overrides(config: &mut Config) {
    // Merge-safety anchor: child agents now inherit the same AGENTS/project-doc prompt layers as
    // the lead workspace, but they still replace the lead-only post-compact ritual with the
    // sub-agent recall warning. Re-run this after any role/profile reload that reconstructs
    // `Config` from persisted layers.
    config.pos_compact_instructions =
        Some(crate::session::default_pos_compact_warning(config, /*is_subagent*/ true).to_string());
}

pub(crate) async fn finalize_spawn_agent_prompt_config(
    config: &mut Config,
    turn: &TurnContext,
    models_manager: &ModelsManager,
) {
    // Merge-safety anchor: role/profile reloads rebuild `Config` from persisted layers, which can
    // restore the lead post-compact ritual. Normalize the child prompt after the final reload so
    // sub-agents keep intended developer instructions and inherited AGENTS/project-doc context
    // while still deriving base instructions from the child's final model/personality selection.
    apply_child_prompt_overrides(config);
    let model = config
        .model
        .clone()
        .unwrap_or_else(|| turn.model_info.slug.clone());
    let models_manager_config = config.to_models_manager_config();
    let model_info = models_manager
        .get_model_info(model.as_str(), &models_manager_config)
        .await;
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

pub(crate) async fn apply_spawn_agent_profile_override(
    config: &mut Config,
    profile_name: Option<&str>,
) -> Result<SubagentFileMutationMode, FunctionCallError> {
    let Some(profile_name) = profile_name else {
        return Ok(config.subagent_file_mutation_mode);
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

    let profile = merged_config
        .get_config_profile(Some(profile_name.to_string()))
        .map_err(|_| {
            FunctionCallError::RespondToModel(format!("config profile `{profile_name}` not found"))
        })?;
    let inherited_subagent_file_mutation_mode = config.subagent_file_mutation_mode;
    let requested_subagent_file_mutation_mode = profile
        .subagent
        .as_ref()
        .and_then(|subagent| subagent.file_mutation)
        .unwrap_or(SubagentFileMutationMode::Inherit);
    let effective_subagent_file_mutation_mode = if matches!(
        requested_subagent_file_mutation_mode,
        SubagentFileMutationMode::Inherit
    ) {
        inherited_subagent_file_mutation_mode
    } else {
        requested_subagent_file_mutation_mode
    };

    let next_config = Config::load_config_with_layer_stack(
        LOCAL_FS.as_ref(),
        merged_config,
        ConfigOverrides {
            config_profile: Some(profile_name.to_string()),
            cwd: Some(config.cwd.clone().to_path_buf()),
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
    .await
    .map_err(|err| {
        FunctionCallError::RespondToModel(format!(
            "failed to apply profile `{profile_name}`: {err}"
        ))
    })?;

    *config = next_config;
    config.subagent_file_mutation_mode = effective_subagent_file_mutation_mode;
    Ok(effective_subagent_file_mutation_mode)
}

pub(crate) fn reject_full_fork_spawn_overrides(
    agent_type: Option<&str>,
    model: Option<&str>,
    reasoning_effort: Option<ReasoningEffort>,
) -> Result<(), FunctionCallError> {
    if agent_type.is_some() || model.is_some() || reasoning_effort.is_some() {
        return Err(FunctionCallError::RespondToModel(
            "Full-history forked agents inherit the parent agent type, model, and reasoning effort; omit agent_type, model, and reasoning_effort, or spawn without fork_context/fork_turns=all.".to_string(),
        ));
    }
    Ok(())
}

/// Copies runtime-only turn state onto a child config before it is handed to `AgentControl`.
///
/// These values are chosen by the live turn rather than persisted config, so leaving them stale
/// can make a child agent disagree with its parent about approval policy, cwd, or sandboxing.
pub(crate) fn apply_spawn_agent_runtime_overrides(
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
            config.permissions.windows_sandbox_mode = Some(WindowsSandboxModeToml::Elevated);
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
            config.permissions.windows_sandbox_mode = Some(WindowsSandboxModeToml::Unelevated);
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

pub(crate) fn apply_spawn_agent_overrides(config: &mut Config, child_depth: i32) {
    if child_depth >= config.agent_max_depth {
        let _ = config.features.disable(Feature::SpawnCsv);
        let _ = config.features.disable(Feature::Collab);
    }
}

pub(crate) fn apply_spawn_agent_subagent_overrides(
    config: &mut Config,
    subagent_file_mutation_mode: SubagentFileMutationMode,
) -> Result<(), FunctionCallError> {
    if matches!(
        subagent_file_mutation_mode,
        SubagentFileMutationMode::Inherit
    ) {
        return Ok(());
    }

    apply_file_mutation_mode_to_config(config, subagent_file_mutation_mode)
        .map_err(FunctionCallError::RespondToModel)
}
pub(crate) async fn apply_requested_spawn_agent_model_overrides(
    session: &Session,
    turn: &TurnContext,
    config: &mut Config,
    requested_model: Option<&str>,
    requested_reasoning_effort: Option<ReasoningEffort>,
) -> Result<(), FunctionCallError> {
    if requested_model.is_none() && requested_reasoning_effort.is_none() {
        return Ok(());
    }

    if let Some(requested_model) = requested_model {
        let available_models = session
            .services
            .models_manager
            .list_models(RefreshStrategy::Offline)
            .await
            .map_err(collab_spawn_error)?;
        let selected_model_name = find_spawn_agent_model_name(&available_models, requested_model)?;
        let selected_model_info = session
            .services
            .models_manager
            .get_model_info(&selected_model_name, &config.to_models_manager_config())
            .await;

        config.model = Some(selected_model_name.clone());
        if let Some(reasoning_effort) = requested_reasoning_effort {
            validate_spawn_agent_reasoning_effort(
                &selected_model_name,
                &selected_model_info.supported_reasoning_levels,
                reasoning_effort,
            )?;
            config.model_reasoning_effort = Some(reasoning_effort);
        } else {
            config.model_reasoning_effort = selected_model_info.default_reasoning_level;
        }

        return Ok(());
    }

    if let Some(reasoning_effort) = requested_reasoning_effort {
        validate_spawn_agent_reasoning_effort(
            &turn.model_info.slug,
            &turn.model_info.supported_reasoning_levels,
            reasoning_effort,
        )?;
        config.model_reasoning_effort = Some(reasoning_effort);
    }

    Ok(())
}

fn find_spawn_agent_model_name(
    available_models: &[codex_protocol::openai_models::ModelPreset],
    requested_model: &str,
) -> Result<String, FunctionCallError> {
    available_models
        .iter()
        .find(|model| model.model == requested_model)
        .map(|model| model.model.clone())
        .ok_or_else(|| {
            let available = available_models
                .iter()
                .map(|model| model.model.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            FunctionCallError::RespondToModel(format!(
                "Unknown model `{requested_model}` for spawn_agent. Available models: {available}"
            ))
        })
}

fn validate_spawn_agent_reasoning_effort(
    model: &str,
    supported_reasoning_levels: &[ReasoningEffortPreset],
    requested_reasoning_effort: ReasoningEffort,
) -> Result<(), FunctionCallError> {
    if supported_reasoning_levels
        .iter()
        .any(|preset| preset.effort == requested_reasoning_effort)
    {
        return Ok(());
    }

    let supported = supported_reasoning_levels
        .iter()
        .map(|preset| preset.effort.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    Err(FunctionCallError::RespondToModel(format!(
        "Reasoning effort `{requested_reasoning_effort}` is not supported for model `{model}`. Supported reasoning efforts: {supported}"
    )))
}
