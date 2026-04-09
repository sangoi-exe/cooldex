//! Implements the collaboration tool surface for spawning and managing sub-agents.
//!
//! This handler translates model tool calls into `AgentControl` operations and keeps spawned
//! agents aligned with the live turn that created them. Sub-agents start from the turn's effective
//! config, inherit runtime-only state such as provider, approval policy, sandbox, and cwd, and
//! then optionally layer role-specific config on top.
// Merge-safety anchor: legacy collab keeps this file as the public handler entrypoint, but the
// shared child-spawn config/prompt helpers live in `multi_agents_common.rs` so legacy/V2/jobs use
// one CLI-owned owner instead of duplicating fork-sensitive config logic.

use crate::agent::AgentRuntimeState;
use crate::agent::AgentStatus;
use crate::agent::exceeds_thread_spawn_depth_limit;
use crate::codex::Session;
use crate::codex::TurnContext;
use crate::config::Config;
use crate::function_tool::FunctionCallError;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
pub(crate) use crate::tools::handlers::multi_agents_common::*;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;
use codex_protocol::AgentPath;
use codex_protocol::ThreadId;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::protocol::CollabAgentInteractionBeginEvent;
use codex_protocol::protocol::CollabAgentInteractionEndEvent;
use codex_protocol::protocol::CollabAgentInteractionTool;
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
use codex_protocol::user_input::UserInput;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;
use std::collections::HashMap;

pub(crate) fn parse_agent_id_target(target: &str) -> Result<ThreadId, FunctionCallError> {
    ThreadId::from_string(target).map_err(|err| {
        FunctionCallError::RespondToModel(format!("invalid agent id {target}: {err:?}"))
    })
}

pub(crate) use close_agent::Handler as CloseAgentHandler;
pub(crate) use resume_agent::Handler as ResumeAgentHandler;
pub(crate) use send_input::Handler as SendInputHandler;
pub(crate) use spawn::Handler as SpawnAgentHandler;
pub(crate) use wait::Handler as WaitAgentHandler;

pub(crate) mod close_agent;
mod resume_agent;
mod send_input;
mod spawn;
pub(crate) mod wait;

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
                task_name: receiver_agent.task_name.clone(),
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
            task_name: None,
            status: state.status.clone(),
            last_activity: state.last_activity.clone(),
        })
        .collect::<Vec<_>>();
    extras.sort_by(|left, right| left.thread_id.to_string().cmp(&right.thread_id.to_string()));
    entries.extend(extras);
    entries
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
