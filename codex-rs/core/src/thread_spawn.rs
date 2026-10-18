//! Shared native MAv2 child preparation and spawning.
//!
//! Callers supply captured step settings; the configured agent backend owns child lifetimes.

use std::sync::Arc;

use crate::agent::api::AgentInput;
use crate::agent::api::SpawnRequest;
use crate::agent::child_config::SpawnConfigOptions;
use crate::agent::child_config::SpawnConfigVersion;
use crate::agent::child_config::prepare_agent_spawn_config_with_service_tier;
use crate::agent::next_thread_spawn_depth;
use crate::agent::types::AgentMessage;
use crate::agent::types::LiveAgent;
use crate::agent::types::MessageDeliveryMode;
use crate::agent::types::SpawnAgentForkMode;
use crate::agent::types::SpawnAgentOptions;
use crate::codex_thread::ThreadConfigSnapshot;
use crate::session::session::Session;
use crate::session::step_context::StepContext;
use codex_protocol::AgentPath;
use codex_protocol::error::CodexErr;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;

/// Child creation options; omitted overrides retain native MAv2 inheritance and role rules.
pub(crate) struct ThreadSpawnRequest {
    pub task_name: String,
    /// Explicit encoding: hosts normally provide plaintext; native tools can forward ciphertext.
    pub message: AgentMessage,
    /// `None` starts fresh; `FullHistory` uses native fork preparation.
    pub fork_mode: Option<SpawnAgentForkMode>,
    pub agent_type: Option<String>,
    pub model: Option<String>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub service_tier: Option<String>,
}

/// Accepted child and its effective configuration. Acceptance does not mean the turn completed.
pub(crate) struct ThreadSpawnResult {
    pub agent: LiveAgent,
    pub config: ThreadConfigSnapshot,
    pub task_name: AgentPath,
}

/// Separates preparation errors from backend failures to preserve native tool error reporting.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ThreadSpawnError {
    #[error("{0}")]
    Preparation(String),
    #[error(transparent)]
    Backend(#[from] CodexErr),
}

pub(crate) async fn spawn(
    session: &Arc<Session>,
    step_context: &Arc<StepContext>,
    call_id: &str,
    request: ThreadSpawnRequest,
) -> Result<ThreadSpawnResult, ThreadSpawnError> {
    let turn = &step_context.turn;
    let ThreadSpawnRequest {
        task_name,
        message,
        fork_mode,
        agent_type,
        model,
        reasoning_effort,
        service_tier,
    } = request;
    let message_text = match &message {
        AgentMessage::Plaintext(text) | AgentMessage::Encrypted(text) => text,
    };
    if message_text.trim().is_empty() {
        return Err(ThreadSpawnError::Preparation(
            "Empty message can't be sent to an agent".to_string(),
        ));
    }
    let is_full_history_fork = matches!(fork_mode, Some(SpawnAgentForkMode::FullHistory));
    // Merge-safety anchor: all V2 callers reject identity overrides and capture the invoking
    // step's complete birth identity before preparation; full-history forks never resolve a new hint.
    if is_full_history_fork {
        reject_full_history_identity_overrides(
            agent_type.as_deref(),
            model.as_deref(),
            reasoning_effort.as_ref(),
            service_tier.as_deref(),
        )?;
    }
    let expected_identity = if is_full_history_fork {
        Some(
            session
                .full_history_agent_identity_snapshot(step_context.as_ref())
                .await,
        )
    } else {
        None
    };
    let role_name = agent_type
        .as_deref()
        .map(str::trim)
        .filter(|role| !role.is_empty());
    let prepared = prepare_agent_spawn_config_with_service_tier(
        session,
        step_context,
        SpawnConfigOptions {
            version: SpawnConfigVersion::V2,
            full_history_fork: is_full_history_fork,
            role_name,
            model: model.as_deref(),
            reasoning_effort,
        },
        service_tier.as_deref(),
    )
    .await
    .map_err(ThreadSpawnError::Preparation)?;
    let mut config = prepared.config;
    let task_name = turn
        .session_source
        .get_agent_path()
        .unwrap_or_else(AgentPath::root)
        .join(&task_name)
        .map_err(ThreadSpawnError::Preparation)?;
    let mut spawn_source = SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
        parent_thread_id: session.thread_id,
        depth: next_thread_spawn_depth(&turn.session_source),
        agent_path: Some(task_name.clone()),
        agent_nickname: None,
        agent_role: prepared.role_name,
    });
    if let Some(identity) = expected_identity {
        identity.apply(&mut config, &mut spawn_source)?;
    }
    let (agent, config) = session
        .services
        .agent_control
        .spawn(SpawnRequest {
            caller: session.thread_id,
            config,
            input: AgentInput::Message {
                message,
                mode: MessageDeliveryMode::TriggerTurn,
            },
            source: spawn_source,
            options: SpawnAgentOptions {
                fork_parent_spawn_call_id: fork_mode.as_ref().map(|_| call_id.to_string()),
                fork_mode,
                parent_thread_id: Some(session.thread_id),
                parent_turn_id: Some(turn.sub_id.clone()),
                root_turn_id: turn.turn_metadata_state.root_turn_id(),
                turn_trigger: turn.turn_metadata_state.current_turn_trigger(),
                environments: Some(step_context.environments.clone()),
                disabled_plugin_ids: Some(turn.disabled_plugin_ids.clone()),
                cyber_access_program: turn.cyber_access_program,
            },
        })
        .await?;
    Ok(ThreadSpawnResult {
        agent,
        config,
        task_name,
    })
}

fn reject_full_history_identity_overrides(
    agent_type: Option<&str>,
    model: Option<&str>,
    reasoning_effort: Option<&ReasoningEffort>,
    service_tier: Option<&str>,
) -> Result<(), ThreadSpawnError> {
    let overrides = [
        agent_type.map(|_| "agent_type"),
        model.map(|_| "model"),
        reasoning_effort.map(|_| "reasoning_effort"),
        service_tier.map(|_| "service_tier"),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    if overrides.is_empty() {
        return Ok(());
    }

    Err(ThreadSpawnError::Preparation(format!(
        "Full-history forks inherit agent_type, model, reasoning_effort, and service_tier from the parent; remove these identity overrides ({}) or set fork_turns to none",
        overrides.join(", ")
    )))
}
