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
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;
use async_trait::async_trait;
use codex_protocol::ThreadId;
use codex_protocol::config_types::WindowsSandboxLevel;
use codex_protocol::models::FunctionCallOutputBody;
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
use codex_protocol::protocol::CollabWaitReturnWhen;
use codex_protocol::protocol::CollabWaitState;
use codex_protocol::protocol::CollabWaitingBeginEvent;
use codex_protocol::protocol::CollabWaitingEndEvent;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::user_input::UserInput;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;
use std::collections::HashSet;

/// Function-tool handler for the multi-agent collaboration API.
pub struct MultiAgentHandler;

/// Minimum wait timeout to prevent tight polling loops from burning CPU.
pub(crate) const MIN_WAIT_TIMEOUT_MS: i64 = 10_000;
pub(crate) const DEFAULT_WAIT_TIMEOUT_MS: i64 = 30_000;
pub(crate) const MAX_WAIT_TIMEOUT_MS: i64 = 3600 * 1000;

#[derive(Debug, Deserialize)]
struct CloseAgentArgs {
    id: String,
}

#[async_trait]
impl ToolHandler for MultiAgentHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    fn matches_kind(&self, payload: &ToolPayload) -> bool {
        matches!(payload, ToolPayload::Function { .. })
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            tool_name,
            payload,
            call_id,
            ..
        } = invocation;

        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "collab handler received unsupported payload".to_string(),
                ));
            }
        };

        match tool_name.as_str() {
            "spawn_agent" => spawn::handle(session, turn, call_id, arguments).await,
            "send_input" => send_input::handle(session, turn, call_id, arguments).await,
            "resume_agent" => resume_agent::handle(session, turn, call_id, arguments).await,
            "wait" => wait::handle(session, turn, call_id, arguments).await,
            "close_agent" => close_agent::handle(session, turn, call_id, arguments).await,
            other => Err(FunctionCallError::RespondToModel(format!(
                "unsupported collab tool {other}"
            ))),
        }
    }
}

mod spawn {
    use super::*;
    use crate::agent::control::SpawnAgentOptions;
    use crate::agent::role::DEFAULT_ROLE_NAME;
    use crate::agent::role::apply_role_to_config;

    use crate::agent::exceeds_thread_spawn_depth_limit;
    use crate::agent::next_thread_spawn_depth;
    use std::sync::Arc;

    #[derive(Debug, Deserialize)]
    struct SpawnAgentArgs {
        message: Option<String>,
        items: Option<Vec<UserInput>>,
        agent_type: Option<String>,
        profile: Option<String>,
        #[serde(default)]
        fork_context: bool,
    }

    #[derive(Debug, Serialize)]
    struct SpawnAgentResult {
        agent_id: String,
        nickname: Option<String>,
    }

    pub async fn handle(
        session: Arc<Session>,
        turn: Arc<TurnContext>,
        call_id: String,
        arguments: String,
    ) -> Result<ToolOutput, FunctionCallError> {
        let args: SpawnAgentArgs = parse_arguments(&arguments)?;
        let role_name = args
            .agent_type
            .as_deref()
            .map(str::trim)
            .filter(|role| !role.is_empty());
        let profile_name = args
            .profile
            .as_deref()
            .map(str::trim)
            .filter(|profile| !profile.is_empty());
        let input_items = parse_collab_input(args.message, args.items)?;
        let prompt = input_preview(&input_items);
        let session_source = turn.session_source.clone();
        let child_depth = next_thread_spawn_depth(&session_source);
        let max_depth = turn.config.agent_max_depth;
        if exceeds_thread_spawn_depth_limit(child_depth, max_depth) {
            return Err(FunctionCallError::RespondToModel(
                "Agent depth limit reached. Solve the task yourself.".to_string(),
            ));
        }
        session
            .send_event(
                &turn,
                CollabAgentSpawnBeginEvent {
                    call_id: call_id.clone(),
                    sender_thread_id: session.conversation_id,
                    prompt: prompt.clone(),
                }
                .into(),
            )
            .await;
        let mut config = build_agent_spawn_config(turn.as_ref())?;
        apply_role_to_config(&mut config, role_name)
            .await
            .map_err(FunctionCallError::RespondToModel)?;
        apply_spawn_agent_profile_override(&mut config, profile_name)?;
        apply_spawn_agent_runtime_overrides(&mut config, turn.as_ref())?;
        finalize_spawn_agent_prompt_config(
            &mut config,
            turn.as_ref(),
            session.services.models_manager.as_ref(),
        )
        .await;
        apply_spawn_agent_overrides(&mut config, child_depth);

        let result = session
            .services
            .agent_control
            .spawn_agent_with_options(
                config,
                input_items,
                Some(thread_spawn_source(
                    session.conversation_id,
                    child_depth,
                    role_name,
                )),
                SpawnAgentOptions {
                    fork_parent_spawn_call_id: args.fork_context.then(|| call_id.clone()),
                },
            )
            .await
            .map_err(collab_spawn_error);
        let (new_thread_id, status) = match &result {
            Ok(thread_id) => (
                Some(*thread_id),
                session.services.agent_control.get_status(*thread_id).await,
            ),
            Err(_) => (None, AgentStatus::NotFound),
        };
        let (new_agent_nickname, new_agent_role) = match new_thread_id {
            Some(thread_id) => session
                .services
                .agent_control
                .get_agent_nickname_and_role(thread_id)
                .await
                .unwrap_or((None, None)),
            None => (None, None),
        };
        let nickname = new_agent_nickname.clone();
        session
            .send_event(
                &turn,
                CollabAgentSpawnEndEvent {
                    call_id,
                    sender_thread_id: session.conversation_id,
                    new_thread_id,
                    new_agent_nickname,
                    new_agent_role,
                    prompt,
                    status,
                }
                .into(),
            )
            .await;
        let new_thread_id = result?;
        let role_tag = role_name.unwrap_or(DEFAULT_ROLE_NAME);
        turn.session_telemetry
            .counter("codex.multi_agent.spawn", 1, &[("role", role_tag)]);

        let content = serde_json::to_string(&SpawnAgentResult {
            agent_id: new_thread_id.to_string(),
            nickname,
        })
        .map_err(|err| {
            FunctionCallError::Fatal(format!("failed to serialize spawn_agent result: {err}"))
        })?;

        Ok(ToolOutput::Function {
            body: FunctionCallOutputBody::Text(content),
            success: Some(true),
        })
    }
}

mod send_input {
    use super::*;
    use std::sync::Arc;

    #[derive(Debug, Deserialize)]
    struct SendInputArgs {
        id: String,
        message: Option<String>,
        items: Option<Vec<UserInput>>,
        #[serde(default)]
        interrupt: bool,
    }

    #[derive(Debug, Serialize)]
    struct SendInputResult {
        submission_id: String,
    }

    pub async fn handle(
        session: Arc<Session>,
        turn: Arc<TurnContext>,
        call_id: String,
        arguments: String,
    ) -> Result<ToolOutput, FunctionCallError> {
        let args: SendInputArgs = parse_arguments(&arguments)?;
        let receiver_thread_id = agent_id(&args.id)?;
        let input_items = parse_collab_input(args.message, args.items)?;
        let prompt = input_preview(&input_items);
        let (receiver_agent_nickname, receiver_agent_role) = session
            .services
            .agent_control
            .get_agent_nickname_and_role(receiver_thread_id)
            .await
            .unwrap_or((None, None));
        if args.interrupt {
            let status = session
                .services
                .agent_control
                .get_status(receiver_thread_id)
                .await;
            ensure_running_subagent_preemption_allowed(
                turn.config.as_ref(),
                "interrupt",
                receiver_thread_id,
                &status,
            )?;
            session
                .services
                .agent_control
                .interrupt_agent(receiver_thread_id)
                .await
                .map_err(|err| collab_agent_error(receiver_thread_id, err))?;
        }
        session
            .send_event(
                &turn,
                CollabAgentInteractionBeginEvent {
                    call_id: call_id.clone(),
                    sender_thread_id: session.conversation_id,
                    receiver_thread_id,
                    prompt: prompt.clone(),
                }
                .into(),
            )
            .await;
        let result = session
            .services
            .agent_control
            .send_input(receiver_thread_id, input_items)
            .await
            .map_err(|err| collab_agent_error(receiver_thread_id, err));
        let status = session
            .services
            .agent_control
            .get_status(receiver_thread_id)
            .await;
        session
            .send_event(
                &turn,
                CollabAgentInteractionEndEvent {
                    call_id,
                    sender_thread_id: session.conversation_id,
                    receiver_thread_id,
                    receiver_agent_nickname,
                    receiver_agent_role,
                    prompt,
                    status,
                }
                .into(),
            )
            .await;
        let submission_id = result?;

        let content = serde_json::to_string(&SendInputResult { submission_id }).map_err(|err| {
            FunctionCallError::Fatal(format!("failed to serialize send_input result: {err}"))
        })?;

        Ok(ToolOutput::Function {
            body: FunctionCallOutputBody::Text(content),
            success: Some(true),
        })
    }
}

mod resume_agent {
    use super::*;
    use crate::agent::next_thread_spawn_depth;
    use std::sync::Arc;

    #[derive(Debug, Deserialize)]
    struct ResumeAgentArgs {
        id: String,
    }

    #[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
    pub(super) struct ResumeAgentResult {
        pub(super) status: AgentStatus,
    }

    pub async fn handle(
        session: Arc<Session>,
        turn: Arc<TurnContext>,
        call_id: String,
        arguments: String,
    ) -> Result<ToolOutput, FunctionCallError> {
        let args: ResumeAgentArgs = parse_arguments(&arguments)?;
        let receiver_thread_id = agent_id(&args.id)?;
        let (receiver_agent_nickname, receiver_agent_role) = session
            .services
            .agent_control
            .get_agent_nickname_and_role(receiver_thread_id)
            .await
            .unwrap_or((None, None));
        let child_depth = next_thread_spawn_depth(&turn.session_source);
        let max_depth = turn.config.agent_max_depth;
        if exceeds_thread_spawn_depth_limit(child_depth, max_depth) {
            return Err(FunctionCallError::RespondToModel(
                "Agent depth limit reached. Solve the task yourself.".to_string(),
            ));
        }

        session
            .send_event(
                &turn,
                CollabResumeBeginEvent {
                    call_id: call_id.clone(),
                    sender_thread_id: session.conversation_id,
                    receiver_thread_id,
                    receiver_agent_nickname: receiver_agent_nickname.clone(),
                    receiver_agent_role: receiver_agent_role.clone(),
                }
                .into(),
            )
            .await;

        let mut status = session
            .services
            .agent_control
            .get_status(receiver_thread_id)
            .await;
        let error = if matches!(status, AgentStatus::NotFound) {
            // If the thread is no longer active, attempt to restore it from rollout.
            match try_resume_closed_agent(&session, &turn, receiver_thread_id, child_depth).await {
                Ok(resumed_status) => {
                    status = resumed_status;
                    None
                }
                Err(err) => {
                    status = session
                        .services
                        .agent_control
                        .get_status(receiver_thread_id)
                        .await;
                    Some(err)
                }
            }
        } else {
            None
        };

        let (receiver_agent_nickname, receiver_agent_role) = session
            .services
            .agent_control
            .get_agent_nickname_and_role(receiver_thread_id)
            .await
            .unwrap_or((receiver_agent_nickname, receiver_agent_role));
        session
            .send_event(
                &turn,
                CollabResumeEndEvent {
                    call_id,
                    sender_thread_id: session.conversation_id,
                    receiver_thread_id,
                    receiver_agent_nickname,
                    receiver_agent_role,
                    status: status.clone(),
                }
                .into(),
            )
            .await;

        if let Some(err) = error {
            return Err(err);
        }
        turn.session_telemetry
            .counter("codex.multi_agent.resume", 1, &[]);

        let content = serde_json::to_string(&ResumeAgentResult { status }).map_err(|err| {
            FunctionCallError::Fatal(format!("failed to serialize resume_agent result: {err}"))
        })?;

        Ok(ToolOutput::Function {
            body: FunctionCallOutputBody::Text(content),
            success: Some(true),
        })
    }

    async fn try_resume_closed_agent(
        session: &Arc<Session>,
        turn: &Arc<TurnContext>,
        receiver_thread_id: ThreadId,
        child_depth: i32,
    ) -> Result<AgentStatus, FunctionCallError> {
        let config = build_agent_resume_config(turn.as_ref(), child_depth)?;
        let resumed_thread_id = session
            .services
            .agent_control
            .resume_agent_from_rollout(
                config,
                receiver_thread_id,
                thread_spawn_source(session.conversation_id, child_depth, None),
            )
            .await
            .map_err(|err| collab_agent_error(receiver_thread_id, err))?;

        Ok(session
            .services
            .agent_control
            .get_status(resumed_thread_id)
            .await)
    }
}

pub(crate) mod wait {
    use super::*;
    use crate::agent::status::is_final;
    use futures::FutureExt;
    use futures::StreamExt;
    use futures::stream::FuturesUnordered;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::watch::Receiver;
    use tokio::time::Instant;

    use tokio::time::timeout_at;

    #[derive(Debug, Deserialize)]
    struct WaitArgs {
        ids: Vec<String>,
        #[serde(default)]
        disable_timeout: bool,
        #[serde(default)]
        return_when: CollabWaitReturnWhen,
        timeout_ms: Option<i64>,
    }

    #[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
    pub(crate) struct WaitResult {
        pub(crate) agents: HashMap<ThreadId, AgentRuntimeState>,
        pub(crate) timed_out: bool,
    }

    pub async fn handle(
        session: Arc<Session>,
        turn: Arc<TurnContext>,
        call_id: String,
        arguments: String,
    ) -> Result<ToolOutput, FunctionCallError> {
        let args: WaitArgs = parse_arguments(&arguments)?;
        if args.ids.is_empty() {
            return Err(FunctionCallError::RespondToModel(
                "ids must be non-empty".to_owned(),
            ));
        }
        if args.disable_timeout && args.timeout_ms.is_some() {
            return Err(FunctionCallError::RespondToModel(
                "disable_timeout cannot be combined with timeout_ms".to_owned(),
            ));
        }
        let receiver_thread_ids = args
            .ids
            .iter()
            .map(|id| agent_id(id))
            .collect::<Result<Vec<_>, _>>()?;
        let mut seen_receiver_thread_ids = HashSet::with_capacity(receiver_thread_ids.len());
        let mut receiver_agents = Vec::with_capacity(receiver_thread_ids.len());
        for receiver_thread_id in &receiver_thread_ids {
            if !seen_receiver_thread_ids.insert(*receiver_thread_id) {
                return Err(FunctionCallError::RespondToModel(
                    "ids must not contain duplicates".to_owned(),
                ));
            }
            let (agent_nickname, agent_role) = session
                .services
                .agent_control
                .get_agent_nickname_and_role(*receiver_thread_id)
                .await
                .unwrap_or((None, None));
            receiver_agents.push(CollabAgentRef {
                thread_id: *receiver_thread_id,
                agent_nickname,
                agent_role,
            });
        }

        // Validate timeout.
        // Very short timeouts encourage busy-polling loops in the orchestrator prompt and can
        // cause high CPU usage even with a single active worker, so clamp to a minimum.
        let timeout_ms = if args.disable_timeout {
            None
        } else {
            let timeout_ms = args.timeout_ms.unwrap_or(DEFAULT_WAIT_TIMEOUT_MS);
            let timeout_ms = match timeout_ms {
                ms if ms <= 0 => {
                    return Err(FunctionCallError::RespondToModel(
                        "timeout_ms must be greater than zero".to_owned(),
                    ));
                }
                ms => ms.clamp(MIN_WAIT_TIMEOUT_MS, MAX_WAIT_TIMEOUT_MS),
            };
            Some(timeout_ms)
        };

        session
            .send_event(
                &turn,
                CollabWaitingBeginEvent {
                    sender_thread_id: session.conversation_id,
                    receiver_thread_ids: receiver_thread_ids.clone(),
                    receiver_agents: receiver_agents.clone(),
                    call_id: call_id.clone(),
                    // Merge-safety anchor: wait-mode metadata must stay aligned with operator
                    // surfaces so any_final/all_final and timeout-disabled waits do not collapse
                    // back into the old generic "finished waiting" behavior.
                    wait_state: collab_wait_state(args.return_when, args.disable_timeout, None),
                }
                .into(),
            )
            .await;

        let mut status_rxs = Vec::with_capacity(receiver_thread_ids.len());
        let mut final_status_count = 0;
        for id in &receiver_thread_ids {
            match session.services.agent_control.subscribe_status(*id).await {
                Ok(rx) => {
                    let status = rx.borrow().clone();
                    if is_final(&status) {
                        final_status_count += 1;
                    }
                    status_rxs.push((*id, rx));
                }
                Err(CodexErr::ThreadNotFound(_)) => {
                    final_status_count += 1;
                }
                Err(err) => {
                    let agent_states =
                        collect_current_agent_states(session.as_ref(), &receiver_thread_ids).await;
                    let statuses = current_statuses(&agent_states);
                    session
                        .send_event(
                            &turn,
                            CollabWaitingEndEvent {
                                sender_thread_id: session.conversation_id,
                                call_id: call_id.clone(),
                                agent_statuses: build_wait_agent_statuses(
                                    &agent_states,
                                    &receiver_agents,
                                ),
                                statuses,
                                wait_state: collab_wait_state(
                                    args.return_when,
                                    args.disable_timeout,
                                    None,
                                ),
                            }
                            .into(),
                        )
                        .await;
                    return Err(collab_agent_error(*id, err));
                }
            }
        }

        let mut condition_met = wait_condition_already_met(
            args.return_when,
            final_status_count,
            receiver_thread_ids.len(),
        );
        if !condition_met {
            condition_met = wait_for_condition(
                session.clone(),
                status_rxs,
                args.return_when,
                timeout_ms
                    .map(|timeout_ms| Instant::now() + Duration::from_millis(timeout_ms as u64)),
            )
            .await?;
        }

        let agents = collect_current_agent_states(session.as_ref(), &receiver_thread_ids).await;
        let statuses_map = current_statuses(&agents);
        let agent_statuses = build_wait_agent_statuses(&agents, &receiver_agents);
        let result = WaitResult {
            agents,
            timed_out: !condition_met,
        };

        // Final event emission.
        session
            .send_event(
                &turn,
                CollabWaitingEndEvent {
                    sender_thread_id: session.conversation_id,
                    call_id,
                    agent_statuses,
                    statuses: statuses_map,
                    wait_state: collab_wait_state(
                        args.return_when,
                        args.disable_timeout,
                        Some(result.timed_out),
                    ),
                }
                .into(),
            )
            .await;

        let content = serde_json::to_string(&result).map_err(|err| {
            FunctionCallError::Fatal(format!("failed to serialize wait result: {err}"))
        })?;

        Ok(ToolOutput::Function {
            body: FunctionCallOutputBody::Text(content),
            success: None,
        })
    }

    async fn wait_for_final_status(
        session: Arc<Session>,
        thread_id: ThreadId,
        mut status_rx: Receiver<AgentStatus>,
    ) -> Option<(ThreadId, AgentStatus)> {
        let mut status = status_rx.borrow().clone();
        if is_final(&status) {
            return Some((thread_id, status));
        }

        loop {
            if status_rx.changed().await.is_err() {
                let latest = session.services.agent_control.get_status(thread_id).await;
                return is_final(&latest).then_some((thread_id, latest));
            }
            status = status_rx.borrow().clone();
            if is_final(&status) {
                return Some((thread_id, status));
            }
        }
    }

    fn wait_condition_already_met(
        return_when: CollabWaitReturnWhen,
        final_status_count: usize,
        total_status_count: usize,
    ) -> bool {
        match return_when {
            CollabWaitReturnWhen::AnyFinal => final_status_count > 0,
            CollabWaitReturnWhen::AllFinal => final_status_count == total_status_count,
        }
    }

    fn collab_wait_state(
        return_when: CollabWaitReturnWhen,
        disable_timeout: bool,
        timed_out: Option<bool>,
    ) -> CollabWaitState {
        CollabWaitState {
            return_when,
            disable_timeout,
            timed_out,
        }
    }

    async fn wait_for_condition(
        session: Arc<Session>,
        status_rxs: Vec<(ThreadId, Receiver<AgentStatus>)>,
        return_when: CollabWaitReturnWhen,
        deadline: Option<Instant>,
    ) -> Result<bool, FunctionCallError> {
        match return_when {
            CollabWaitReturnWhen::AnyFinal => {
                wait_for_any_final(session, status_rxs, deadline).await
            }
            CollabWaitReturnWhen::AllFinal => {
                wait_for_all_final(session, status_rxs, deadline).await
            }
        }
    }

    async fn wait_for_any_final(
        session: Arc<Session>,
        status_rxs: Vec<(ThreadId, Receiver<AgentStatus>)>,
        deadline: Option<Instant>,
    ) -> Result<bool, FunctionCallError> {
        let mut futures = FuturesUnordered::new();
        for (id, rx) in status_rxs {
            let session = session.clone();
            futures.push(wait_for_final_status(session, id, rx));
        }
        wait_for_any_final_from_futures(&mut futures, deadline).await
    }

    async fn wait_for_any_final_from_futures<F>(
        futures: &mut FuturesUnordered<F>,
        deadline: Option<Instant>,
    ) -> Result<bool, FunctionCallError>
    where
        F: std::future::Future<Output = Option<(ThreadId, AgentStatus)>> + Unpin,
    {
        if let Some(deadline) = deadline {
            loop {
                match timeout_at(deadline, futures.next()).await {
                    Ok(Some(Some(_result))) => return Ok(true),
                    Ok(Some(None)) => continue,
                    Ok(None) => {
                        return Err(FunctionCallError::Fatal(
                            "wait exhausted all status subscriptions before any requested agent reached a final status".to_string(),
                        ));
                    }
                    Err(_) => return Ok(false),
                }
            }
        }

        loop {
            match futures.next().await {
                Some(Some(_result)) => return Ok(true),
                Some(None) => continue,
                None => {
                    return Err(FunctionCallError::Fatal(
                        "wait exhausted all status subscriptions before any requested agent reached a final status".to_string(),
                    ));
                }
            }
        }
    }

    async fn wait_for_all_final(
        session: Arc<Session>,
        status_rxs: Vec<(ThreadId, Receiver<AgentStatus>)>,
        deadline: Option<Instant>,
    ) -> Result<bool, FunctionCallError> {
        let mut futures = FuturesUnordered::new();
        for (id, rx) in status_rxs {
            if is_final(&rx.borrow().clone()) {
                continue;
            }
            let session = session.clone();
            futures.push(wait_for_final_status(session, id, rx));
        }

        let mut remaining = futures.len();
        if remaining == 0 {
            return Ok(true);
        }

        if let Some(deadline) = deadline {
            loop {
                match timeout_at(deadline, futures.next()).await {
                    Ok(Some(Some(_result))) => {
                        remaining -= 1;
                        if remaining == 0 {
                            return Ok(true);
                        }
                    }
                    Ok(Some(None)) | Ok(None) => {
                        return Err(FunctionCallError::Fatal(
                            "wait exhausted a status subscription before all requested agents reached a final status".to_string(),
                        ));
                    }
                    Err(_) => return Ok(false),
                }
            }
        }

        loop {
            match futures.next().await {
                Some(Some(_result)) => {
                    remaining -= 1;
                    if remaining == 0 {
                        return Ok(true);
                    }
                }
                Some(None) | None => {
                    return Err(FunctionCallError::Fatal(
                        "wait exhausted a status subscription before all requested agents reached a final status".to_string(),
                    ));
                }
            }
        }
    }
}

pub mod close_agent {
    use super::*;
    use std::sync::Arc;

    #[derive(Debug, Deserialize, Serialize)]
    pub(super) struct CloseAgentResult {
        pub(super) status: AgentStatus,
    }

    pub async fn handle(
        session: Arc<Session>,
        turn: Arc<TurnContext>,
        call_id: String,
        arguments: String,
    ) -> Result<ToolOutput, FunctionCallError> {
        let args: CloseAgentArgs = parse_arguments(&arguments)?;
        let agent_id = agent_id(&args.id)?;
        let (receiver_agent_nickname, receiver_agent_role) = session
            .services
            .agent_control
            .get_agent_nickname_and_role(agent_id)
            .await
            .unwrap_or((None, None));
        session
            .send_event(
                &turn,
                CollabCloseBeginEvent {
                    call_id: call_id.clone(),
                    sender_thread_id: session.conversation_id,
                    receiver_thread_id: agent_id,
                }
                .into(),
            )
            .await;
        let status = match session
            .services
            .agent_control
            .subscribe_status(agent_id)
            .await
        {
            Ok(mut status_rx) => status_rx.borrow_and_update().clone(),
            Err(err) => {
                let status = session.services.agent_control.get_status(agent_id).await;
                session
                    .send_event(
                        &turn,
                        CollabCloseEndEvent {
                            call_id: call_id.clone(),
                            sender_thread_id: session.conversation_id,
                            receiver_thread_id: agent_id,
                            receiver_agent_nickname: receiver_agent_nickname.clone(),
                            receiver_agent_role: receiver_agent_role.clone(),
                            status,
                        }
                        .into(),
                    )
                    .await;
                return Err(collab_agent_error(agent_id, err));
            }
        };
        let result = match ensure_running_subagent_preemption_allowed(
            turn.config.as_ref(),
            "close",
            agent_id,
            &status,
        ) {
            Ok(()) if !matches!(status, AgentStatus::Shutdown) => session
                .services
                .agent_control
                .shutdown_agent(agent_id)
                .await
                .map_err(|err| collab_agent_error(agent_id, err))
                .map(|_| ()),
            Ok(()) => Ok(()),
            Err(err) => Err(err),
        };
        session
            .send_event(
                &turn,
                CollabCloseEndEvent {
                    call_id,
                    sender_thread_id: session.conversation_id,
                    receiver_thread_id: agent_id,
                    receiver_agent_nickname,
                    receiver_agent_role,
                    status: status.clone(),
                }
                .into(),
            )
            .await;
        result?;

        let content = serde_json::to_string(&CloseAgentResult { status }).map_err(|err| {
            FunctionCallError::Fatal(format!("failed to serialize close_agent result: {err}"))
        })?;

        Ok(ToolOutput::Function {
            body: FunctionCallOutputBody::Text(content),
            success: Some(true),
        })
    }
}

fn agent_id(id: &str) -> Result<ThreadId, FunctionCallError> {
    ThreadId::from_string(id)
        .map_err(|e| FunctionCallError::RespondToModel(format!("invalid agent id {id}: {e:?}")))
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
    // Merge-safety anchor: resume keeps base instructions sourced from
    // rollout/session metadata instead of reusing parent prompt channels.
    config.base_instructions = None;
    Ok(config)
}

fn build_agent_shared_config(turn: &TurnContext) -> Result<Config, FunctionCallError> {
    let base_config = turn.config.clone();
    let mut config = (*base_config).clone();
    config.model = Some(turn.model_info.slug.clone());
    config.model_provider = turn.provider.clone();
    config.model_reasoning_effort = turn.reasoning_effort;
    config.model_reasoning_summary = Some(turn.reasoning_summary);
    strip_child_prompt_inheritance(&mut config);
    config.compact_prompt = turn.compact_prompt.clone();
    apply_spawn_agent_runtime_overrides(&mut config, turn)?;

    Ok(config)
}

fn strip_child_prompt_inheritance(config: &mut Config) {
    // Workspace customization: child agents must not inherit the lead prompt stack.
    // `subagent_instructions_file` provides the child-only base instructions instead, while
    // AGENTS/project-doc and lead developer instructions stay isolated. Re-run this after any
    // role/profile reload that reconstructs `Config` from persisted layers.
    config.developer_instructions = None;
    config.user_instructions = None;
    config.project_doc_max_bytes = 0;
    let _ = config.features.disable(Feature::ChildAgentsMd);
}

async fn finalize_spawn_agent_prompt_config(
    config: &mut Config,
    turn: &TurnContext,
    models_manager: &crate::models_manager::manager::ModelsManager,
) {
    // Merge-safety anchor: role/profile reloads rebuild `Config` from persisted layers, which can
    // repopulate developer instructions, AGENTS/project-doc context, and feature flags. Normalize
    // the child prompt after the final reload so sub-agents stay isolated from the lead prompt
    // stack and derive base instructions from the child's final model/personality selection.
    strip_child_prompt_inheritance(config);
    let model = config
        .model
        .clone()
        .unwrap_or_else(|| turn.model_info.slug.clone());
    let model_info = models_manager.get_model_info(model.as_str(), config).await;
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
    config.permissions.file_system_sandbox_policy =
        crate::protocol::FileSystemSandboxPolicy::from(turn.sandbox_policy.get());
    config.permissions.network_sandbox_policy =
        crate::protocol::NetworkSandboxPolicy::from(turn.sandbox_policy.get());
    // Child session startup re-derives the effective Windows sandbox level from config. Keep the
    // config-side mode aligned with the live turn override so spawned and resumed children do not
    // silently fall back to stale Windows sandbox policy.
    match turn.windows_sandbox_level {
        WindowsSandboxLevel::Elevated => {
            config.permissions.windows_sandbox_mode =
                Some(crate::config::types::WindowsSandboxModeToml::Elevated);
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
        let _ = config.features.disable(Feature::Collab);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AuthManager;
    use crate::CodexAuth;
    use crate::ThreadManager;
    use crate::agent::role::apply_role_to_config;
    use crate::built_in_model_providers;
    use crate::codex::make_session_and_context;
    use crate::config::AgentRoleConfig;
    use crate::config::ConfigToml;
    use crate::config::DEFAULT_AGENT_MAX_DEPTH;
    use crate::config::profile::ConfigProfile;
    use crate::config::types::ShellEnvironmentPolicy;
    use crate::config::types::WindowsSandboxModeToml;
    use crate::config_loader::ConfigLayerStack;
    use crate::features::Feature;
    use crate::function_tool::FunctionCallError;
    use crate::protocol::AskForApproval;
    use crate::protocol::Op;
    use crate::protocol::SandboxPolicy;
    use crate::protocol::SessionSource;
    use crate::protocol::SubAgentSource;
    use crate::turn_diff_tracker::TurnDiffTracker;
    use codex_protocol::ThreadId;
    use codex_protocol::config_types::WindowsSandboxLevel;
    use codex_protocol::models::ContentItem;
    use codex_protocol::models::ResponseItem;
    use codex_protocol::openai_models::ReasoningEffort as ReasoningEffortConfig;
    use codex_protocol::protocol::InitialHistory;
    use codex_protocol::protocol::RolloutItem;
    use codex_utils_absolute_path::AbsolutePathBuf;
    use pretty_assertions::assert_eq;
    use serde::Deserialize;
    use serde_json::json;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::Mutex;
    use tokio::time::timeout;

    fn invocation(
        session: Arc<crate::codex::Session>,
        turn: Arc<TurnContext>,
        tool_name: &str,
        payload: ToolPayload,
    ) -> ToolInvocation {
        ToolInvocation {
            session,
            turn,
            tracker: Arc::new(Mutex::new(TurnDiffTracker::default())),
            call_id: "call-1".to_string(),
            tool_name: tool_name.to_string(),
            payload,
        }
    }

    fn function_payload(args: serde_json::Value) -> ToolPayload {
        ToolPayload::Function {
            arguments: args.to_string(),
        }
    }

    fn with_running_subagent_preemption(
        mut turn: TurnContext,
        allow_running_subagent_preemption: bool,
    ) -> TurnContext {
        let mut config = (*turn.config).clone();
        config.agent_allow_running_subagent_preemption = allow_running_subagent_preemption;
        turn.config = Arc::new(config);
        turn
    }

    fn thread_manager() -> ThreadManager {
        ThreadManager::with_models_provider_for_tests(
            CodexAuth::from_api_key("dummy"),
            built_in_model_providers()["openai"].clone(),
        )
    }

    fn load_test_config(
        cfg: ConfigToml,
        codex_home: &tempfile::TempDir,
        workspace: &tempfile::TempDir,
    ) -> Config {
        Config::load_config_with_layer_stack(
            cfg,
            ConfigOverrides {
                cwd: Some(workspace.path().to_path_buf()),
                ..Default::default()
            },
            codex_home.path().to_path_buf(),
            ConfigLayerStack::default(),
        )
        .expect("test config")
    }

    #[tokio::test]
    async fn spawn_config_prefers_subagent_base_instructions_when_present() {
        let (_session, mut turn) = make_session_and_context().await;
        let mut config = (*turn.config).clone();
        config.subagent_base_instructions = Some("subagent-only instructions".to_string());
        config.user_instructions = Some("base-user".to_string());
        config.project_doc_max_bytes = 4096;
        config.features.enable(Feature::ChildAgentsMd).unwrap();
        turn.config = Arc::new(config);
        turn.developer_instructions = Some("dev".to_string());

        let spawn_config = build_agent_spawn_config(&turn).expect("spawn config");

        assert_eq!(
            spawn_config.base_instructions,
            Some("subagent-only instructions".to_string())
        );
        assert_eq!(spawn_config.developer_instructions, None);
        assert_eq!(spawn_config.user_instructions, None);
        assert_eq!(spawn_config.project_doc_max_bytes, 0);
        assert!(!spawn_config.features.enabled(Feature::ChildAgentsMd));
    }

    #[tokio::test]
    async fn resume_config_strips_inherited_instruction_channels() {
        let (_session, mut turn) = make_session_and_context().await;
        let mut config = (*turn.config).clone();
        config.user_instructions = Some("base-user".to_string());
        config.project_doc_max_bytes = 4096;
        config.features.enable(Feature::ChildAgentsMd).unwrap();
        turn.config = Arc::new(config);
        turn.developer_instructions = Some("dev".to_string());

        let resume_config = build_agent_resume_config(&turn, 1).expect("resume config");

        assert_eq!(resume_config.base_instructions, None);
        assert_eq!(resume_config.developer_instructions, None);
        assert_eq!(resume_config.user_instructions, None);
        assert_eq!(resume_config.project_doc_max_bytes, 0);
        assert!(!resume_config.features.enabled(Feature::ChildAgentsMd));
    }

    #[tokio::test]
    async fn handler_rejects_non_function_payloads() {
        let (session, turn) = make_session_and_context().await;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            ToolPayload::Custom {
                input: "hello".to_string(),
            },
        );
        let Err(err) = MultiAgentHandler.handle(invocation).await else {
            panic!("payload should be rejected");
        };
        assert_eq!(
            err,
            FunctionCallError::RespondToModel(
                "collab handler received unsupported payload".to_string()
            )
        );
    }

    #[tokio::test]
    async fn handler_rejects_unknown_tool() {
        let (session, turn) = make_session_and_context().await;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "unknown_tool",
            function_payload(json!({})),
        );
        let Err(err) = MultiAgentHandler.handle(invocation).await else {
            panic!("tool should be rejected");
        };
        assert_eq!(
            err,
            FunctionCallError::RespondToModel("unsupported collab tool unknown_tool".to_string())
        );
    }

    #[tokio::test]
    async fn spawn_agent_rejects_empty_message() {
        let (session, turn) = make_session_and_context().await;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            function_payload(json!({"message": "   "})),
        );
        let Err(err) = MultiAgentHandler.handle(invocation).await else {
            panic!("empty message should be rejected");
        };
        assert_eq!(
            err,
            FunctionCallError::RespondToModel(
                "Empty message can't be sent to an agent".to_string()
            )
        );
    }

    #[tokio::test]
    async fn spawn_agent_rejects_when_message_and_items_are_both_set() {
        let (session, turn) = make_session_and_context().await;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            function_payload(json!({
                "message": "hello",
                "items": [{"type": "mention", "name": "drive", "path": "app://drive"}]
            })),
        );
        let Err(err) = MultiAgentHandler.handle(invocation).await else {
            panic!("message+items should be rejected");
        };
        assert_eq!(
            err,
            FunctionCallError::RespondToModel(
                "Provide either message or items, but not both".to_string()
            )
        );
    }

    #[tokio::test]
    async fn spawn_agent_uses_explorer_role_and_preserves_approval_policy() {
        #[derive(Debug, Deserialize)]
        struct SpawnAgentResult {
            agent_id: String,
            nickname: Option<String>,
        }

        let (mut session, mut turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();
        let mut config = (*turn.config).clone();
        let provider = built_in_model_providers()["ollama"].clone();
        config.model_provider_id = "ollama".to_string();
        config.model_provider = provider.clone();
        config
            .permissions
            .approval_policy
            .set(AskForApproval::OnRequest)
            .expect("approval policy should be set");
        turn.approval_policy
            .set(AskForApproval::OnRequest)
            .expect("approval policy should be set");
        turn.provider = provider;
        turn.config = Arc::new(config);

        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            function_payload(json!({
                "message": "inspect this repo",
                "agent_type": "explorer"
            })),
        );
        let output = MultiAgentHandler
            .handle(invocation)
            .await
            .expect("spawn_agent should succeed");
        let ToolOutput::Function {
            body: FunctionCallOutputBody::Text(content),
            ..
        } = output
        else {
            panic!("expected function output");
        };
        let result: SpawnAgentResult =
            serde_json::from_str(&content).expect("spawn_agent result should be json");
        let agent_id = agent_id(&result.agent_id).expect("agent_id should be valid");
        assert!(
            result
                .nickname
                .as_deref()
                .is_some_and(|nickname| !nickname.is_empty())
        );
        let snapshot = manager
            .get_thread(agent_id)
            .await
            .expect("spawned agent thread should exist")
            .config_snapshot()
            .await;
        assert_eq!(snapshot.approval_policy, AskForApproval::OnRequest);
        assert_eq!(snapshot.model_provider_id, "ollama");
    }

    #[tokio::test]
    async fn spawn_agent_uses_lead_reasoning_effort_when_override_omitted() {
        #[derive(Debug, Deserialize)]
        struct SpawnAgentResult {
            agent_id: String,
        }

        let (mut session, mut turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();

        turn.reasoning_effort = Some(ReasoningEffortConfig::Minimal);

        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            function_payload(json!({
                "message": "inspect this repo"
            })),
        );
        let output = MultiAgentHandler
            .handle(invocation)
            .await
            .expect("spawn_agent should succeed");
        let ToolOutput::Function {
            body: FunctionCallOutputBody::Text(content),
            ..
        } = output
        else {
            panic!("expected function output");
        };
        let result: SpawnAgentResult =
            serde_json::from_str(&content).expect("spawn_agent result should be json");
        let agent_id = agent_id(&result.agent_id).expect("agent_id should be valid");

        let snapshot = manager
            .get_thread(agent_id)
            .await
            .expect("spawned agent thread should exist")
            .config_snapshot()
            .await;
        assert_eq!(
            snapshot.reasoning_effort,
            Some(ReasoningEffortConfig::Minimal)
        );
    }

    #[tokio::test]
    async fn spawn_agent_rejects_unknown_profile() {
        let (mut session, turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();

        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            function_payload(json!({
                "message": "inspect this repo",
                "profile": "missing-profile"
            })),
        );
        let Err(err) = MultiAgentHandler.handle(invocation).await else {
            panic!("missing profile should be rejected");
        };
        assert_eq!(
            err,
            FunctionCallError::RespondToModel(
                "config profile `missing-profile` not found".to_string()
            )
        );
    }

    #[tokio::test]
    async fn spawn_agent_errors_when_manager_dropped() {
        let (session, turn) = make_session_and_context().await;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            function_payload(json!({"message": "hello"})),
        );
        let Err(err) = MultiAgentHandler.handle(invocation).await else {
            panic!("spawn should fail without a manager");
        };
        assert_eq!(
            err,
            FunctionCallError::RespondToModel("collab manager unavailable".to_string())
        );
    }

    #[tokio::test]
    async fn spawn_agent_reapplies_runtime_sandbox_after_role_config() {
        fn pick_allowed_sandbox_policy(
            constraint: &crate::config::Constrained<SandboxPolicy>,
            base: SandboxPolicy,
        ) -> SandboxPolicy {
            let candidates = [
                SandboxPolicy::DangerFullAccess,
                SandboxPolicy::new_workspace_write_policy(),
                SandboxPolicy::new_read_only_policy(),
            ];
            candidates
                .into_iter()
                .find(|candidate| *candidate != base && constraint.can_set(candidate).is_ok())
                .unwrap_or(base)
        }

        #[derive(Debug, Deserialize)]
        struct SpawnAgentResult {
            agent_id: String,
            nickname: Option<String>,
        }

        let (mut session, mut turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();
        let expected_sandbox = pick_allowed_sandbox_policy(
            &turn.config.permissions.sandbox_policy,
            turn.config.permissions.sandbox_policy.get().clone(),
        );
        turn.approval_policy
            .set(AskForApproval::OnRequest)
            .expect("approval policy should be set");
        turn.sandbox_policy
            .set(expected_sandbox.clone())
            .expect("sandbox policy should be set");
        assert_ne!(
            expected_sandbox,
            turn.config.permissions.sandbox_policy.get().clone(),
            "test requires a runtime sandbox override that differs from base config"
        );

        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            function_payload(json!({
                "message": "await this command",
                "agent_type": "explorer"
            })),
        );
        let output = MultiAgentHandler
            .handle(invocation)
            .await
            .expect("spawn_agent should succeed");
        let ToolOutput::Function {
            body: FunctionCallOutputBody::Text(content),
            ..
        } = output
        else {
            panic!("expected function output");
        };
        let result: SpawnAgentResult =
            serde_json::from_str(&content).expect("spawn_agent result should be json");
        let agent_id = agent_id(&result.agent_id).expect("agent_id should be valid");
        assert!(
            result
                .nickname
                .as_deref()
                .is_some_and(|nickname| !nickname.is_empty())
        );

        let snapshot = manager
            .get_thread(agent_id)
            .await
            .expect("spawned agent thread should exist")
            .config_snapshot()
            .await;
        assert_eq!(snapshot.sandbox_policy, expected_sandbox);
        assert_eq!(snapshot.approval_policy, AskForApproval::OnRequest);
    }

    #[tokio::test]
    async fn spawn_agent_rejects_when_depth_limit_exceeded() {
        let (mut session, mut turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();

        let max_depth = turn.config.agent_max_depth;
        turn.session_source = SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id: session.conversation_id,
            depth: max_depth,
            agent_nickname: None,
            agent_role: None,
        });

        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            function_payload(json!({"message": "hello"})),
        );
        let Err(err) = MultiAgentHandler.handle(invocation).await else {
            panic!("spawn should fail when depth limit exceeded");
        };
        assert_eq!(
            err,
            FunctionCallError::RespondToModel(
                "Agent depth limit reached. Solve the task yourself.".to_string()
            )
        );
    }

    #[tokio::test]
    async fn spawn_agent_allows_depth_up_to_configured_max_depth() {
        #[derive(Debug, Deserialize)]
        struct SpawnAgentResult {
            agent_id: String,
            nickname: Option<String>,
        }

        let (mut session, mut turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();

        let mut config = (*turn.config).clone();
        config.agent_max_depth = DEFAULT_AGENT_MAX_DEPTH + 1;
        turn.config = Arc::new(config);
        turn.session_source = SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id: session.conversation_id,
            depth: DEFAULT_AGENT_MAX_DEPTH,
            agent_nickname: None,
            agent_role: None,
        });

        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            function_payload(json!({"message": "hello"})),
        );
        let output = MultiAgentHandler
            .handle(invocation)
            .await
            .expect("spawn should succeed within configured depth");
        let ToolOutput::Function {
            body: FunctionCallOutputBody::Text(content),
            success,
            ..
        } = output
        else {
            panic!("expected function output");
        };
        let result: SpawnAgentResult =
            serde_json::from_str(&content).expect("spawn_agent result should be json");
        assert!(!result.agent_id.is_empty());
        assert!(
            result
                .nickname
                .as_deref()
                .is_some_and(|nickname| !nickname.is_empty())
        );
        assert_eq!(success, Some(true));
    }

    #[tokio::test]
    async fn send_input_rejects_empty_message() {
        let (session, turn) = make_session_and_context().await;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "send_input",
            function_payload(json!({"id": ThreadId::new().to_string(), "message": ""})),
        );
        let Err(err) = MultiAgentHandler.handle(invocation).await else {
            panic!("empty message should be rejected");
        };
        assert_eq!(
            err,
            FunctionCallError::RespondToModel(
                "Empty message can't be sent to an agent".to_string()
            )
        );
    }

    #[tokio::test]
    async fn send_input_rejects_when_message_and_items_are_both_set() {
        let (session, turn) = make_session_and_context().await;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "send_input",
            function_payload(json!({
                "id": ThreadId::new().to_string(),
                "message": "hello",
                "items": [{"type": "mention", "name": "drive", "path": "app://drive"}]
            })),
        );
        let Err(err) = MultiAgentHandler.handle(invocation).await else {
            panic!("message+items should be rejected");
        };
        assert_eq!(
            err,
            FunctionCallError::RespondToModel(
                "Provide either message or items, but not both".to_string()
            )
        );
    }

    #[tokio::test]
    async fn send_input_rejects_invalid_id() {
        let (session, turn) = make_session_and_context().await;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "send_input",
            function_payload(json!({"id": "not-a-uuid", "message": "hi"})),
        );
        let Err(err) = MultiAgentHandler.handle(invocation).await else {
            panic!("invalid id should be rejected");
        };
        let FunctionCallError::RespondToModel(msg) = err else {
            panic!("expected respond-to-model error");
        };
        assert!(msg.starts_with("invalid agent id not-a-uuid:"));
    }

    #[tokio::test]
    async fn send_input_reports_missing_agent() {
        let (mut session, turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();
        let agent_id = ThreadId::new();
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "send_input",
            function_payload(json!({"id": agent_id.to_string(), "message": "hi"})),
        );
        let Err(err) = MultiAgentHandler.handle(invocation).await else {
            panic!("missing agent should be reported");
        };
        assert_eq!(
            err,
            FunctionCallError::RespondToModel(format!("agent with id {agent_id} not found"))
        );
    }

    #[tokio::test]
    async fn send_input_interrupts_before_prompt() {
        let (mut session, turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();
        let config = turn.config.as_ref().clone();
        let thread = manager.start_thread(config).await.expect("start thread");
        let agent_id = thread.thread_id;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "send_input",
            function_payload(json!({
                "id": agent_id.to_string(),
                "message": "hi",
                "interrupt": true
            })),
        );
        MultiAgentHandler
            .handle(invocation)
            .await
            .expect("send_input should succeed");

        let ops = manager.captured_ops();
        let ops_for_agent: Vec<&Op> = ops
            .iter()
            .filter_map(|(id, op)| (*id == agent_id).then_some(op))
            .collect();
        assert_eq!(ops_for_agent.len(), 2);
        assert!(matches!(ops_for_agent[0], Op::Interrupt));
        assert!(matches!(ops_for_agent[1], Op::UserInput { .. }));

        let _ = thread
            .thread
            .submit(Op::Shutdown {})
            .await
            .expect("shutdown should submit");
    }

    #[tokio::test]
    async fn send_input_interrupt_rejects_active_agent_when_preemption_disabled() {
        let (mut session, turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();
        let config = turn.config.as_ref().clone();
        let thread = manager.start_thread(config).await.expect("start thread");
        let agent_id = thread.thread_id;
        let turn = with_running_subagent_preemption(turn, false);
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "send_input",
            function_payload(json!({
                "id": agent_id.to_string(),
                "message": "hi",
                "interrupt": true
            })),
        );

        let Err(err) = MultiAgentHandler.handle(invocation).await else {
            panic!("interrupt should be rejected when preemption is disabled");
        };
        let FunctionCallError::RespondToModel(message) = err else {
            panic!("expected respond-to-model error");
        };
        assert!(message.contains("agents.allow_running_subagent_preemption"));
        assert!(message.contains("interrupt"));
        assert!(message.contains(&agent_id.to_string()));
        assert!(message.contains("status"));

        let submitted_any = manager.captured_ops().iter().any(|(id, _)| *id == agent_id);
        assert!(!submitted_any);

        let _ = thread
            .thread
            .submit(Op::Shutdown {})
            .await
            .expect("shutdown should submit");
    }

    #[tokio::test]
    async fn send_input_without_interrupt_is_unchanged_when_preemption_disabled() {
        let (mut session, turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();
        let config = turn.config.as_ref().clone();
        let thread = manager.start_thread(config).await.expect("start thread");
        let agent_id = thread.thread_id;
        let turn = with_running_subagent_preemption(turn, false);
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "send_input",
            function_payload(json!({
                "id": agent_id.to_string(),
                "message": "hi"
            })),
        );
        let output = MultiAgentHandler
            .handle(invocation)
            .await
            .expect("send_input should succeed");
        let ToolOutput::Function {
            body: FunctionCallOutputBody::Text(content),
            success,
            ..
        } = output
        else {
            panic!("expected function output");
        };
        let result: serde_json::Value =
            serde_json::from_str(&content).expect("send_input result should be json");
        let submission_id = result
            .get("submission_id")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        assert!(!submission_id.is_empty());
        assert_eq!(success, Some(true));

        let ops = manager.captured_ops();
        let ops_for_agent: Vec<&Op> = ops
            .iter()
            .filter_map(|(id, op)| (*id == agent_id).then_some(op))
            .collect();
        assert_eq!(ops_for_agent.len(), 1);
        assert!(matches!(ops_for_agent[0], Op::UserInput { .. }));

        let _ = thread
            .thread
            .submit(Op::Shutdown {})
            .await
            .expect("shutdown should submit");
    }

    #[tokio::test]
    async fn preemption_guard_rejects_pending_init_and_running_statuses_when_disabled() {
        let (_session, turn) = make_session_and_context().await;
        let turn = with_running_subagent_preemption(turn, false);
        let agent_id = ThreadId::new();

        for (action, status) in [
            ("interrupt", AgentStatus::PendingInit),
            ("close", AgentStatus::Running),
        ] {
            let err = ensure_running_subagent_preemption_allowed(
                turn.config.as_ref(),
                action,
                agent_id,
                &status,
            )
            .expect_err("active status should be rejected");
            let FunctionCallError::RespondToModel(message) = err else {
                panic!("expected respond-to-model error");
            };
            assert!(message.contains("agents.allow_running_subagent_preemption"));
            assert!(message.contains(action));
            assert!(message.contains(&agent_id.to_string()));
            assert!(message.contains(&format!("{status:?}")));
        }
    }

    #[tokio::test]
    async fn preemption_guard_allows_completed_and_errored_statuses_when_disabled() {
        let (_session, turn) = make_session_and_context().await;
        let turn = with_running_subagent_preemption(turn, false);
        let agent_id = ThreadId::new();

        for status in [
            AgentStatus::Completed(Some("done".to_string())),
            AgentStatus::Errored("boom".to_string()),
        ] {
            ensure_running_subagent_preemption_allowed(
                turn.config.as_ref(),
                "close",
                agent_id,
                &status,
            )
            .expect("final status should be allowed");
        }
    }

    #[tokio::test]
    async fn send_input_accepts_structured_items() {
        let (mut session, turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();
        let config = turn.config.as_ref().clone();
        let thread = manager.start_thread(config).await.expect("start thread");
        let agent_id = thread.thread_id;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "send_input",
            function_payload(json!({
                "id": agent_id.to_string(),
                "items": [
                    {"type": "mention", "name": "drive", "path": "app://google_drive"},
                    {"type": "text", "text": "read the folder"}
                ]
            })),
        );
        MultiAgentHandler
            .handle(invocation)
            .await
            .expect("send_input should succeed");

        let expected = Op::UserInput {
            items: vec![
                UserInput::Mention {
                    name: "drive".to_string(),
                    path: "app://google_drive".to_string(),
                },
                UserInput::Text {
                    text: "read the folder".to_string(),
                    text_elements: Vec::new(),
                },
            ],
            final_output_json_schema: None,
        };
        let captured = manager
            .captured_ops()
            .into_iter()
            .find(|(id, op)| *id == agent_id && *op == expected);
        assert_eq!(captured, Some((agent_id, expected)));

        let _ = thread
            .thread
            .submit(Op::Shutdown {})
            .await
            .expect("shutdown should submit");
    }

    #[tokio::test]
    async fn resume_agent_rejects_invalid_id() {
        let (session, turn) = make_session_and_context().await;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "resume_agent",
            function_payload(json!({"id": "not-a-uuid"})),
        );
        let Err(err) = MultiAgentHandler.handle(invocation).await else {
            panic!("invalid id should be rejected");
        };
        let FunctionCallError::RespondToModel(msg) = err else {
            panic!("expected respond-to-model error");
        };
        assert!(msg.starts_with("invalid agent id not-a-uuid:"));
    }

    #[tokio::test]
    async fn resume_agent_reports_missing_agent() {
        let (mut session, turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();
        let agent_id = ThreadId::new();
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "resume_agent",
            function_payload(json!({"id": agent_id.to_string()})),
        );
        let Err(err) = MultiAgentHandler.handle(invocation).await else {
            panic!("missing agent should be reported");
        };
        assert_eq!(
            err,
            FunctionCallError::RespondToModel(format!("agent with id {agent_id} not found"))
        );
    }

    #[tokio::test]
    async fn resume_agent_noops_for_active_agent() {
        let (mut session, turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();
        let config = turn.config.as_ref().clone();
        let thread = manager.start_thread(config).await.expect("start thread");
        let agent_id = thread.thread_id;
        let status_before = manager.agent_control().get_status(agent_id).await;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "resume_agent",
            function_payload(json!({"id": agent_id.to_string()})),
        );

        let output = MultiAgentHandler
            .handle(invocation)
            .await
            .expect("resume_agent should succeed");
        let ToolOutput::Function {
            body: FunctionCallOutputBody::Text(content),
            success,
            ..
        } = output
        else {
            panic!("expected function output");
        };
        let result: resume_agent::ResumeAgentResult =
            serde_json::from_str(&content).expect("resume_agent result should be json");
        assert_eq!(result.status, status_before);
        assert_eq!(success, Some(true));

        let thread_ids = manager.list_thread_ids().await;
        assert_eq!(thread_ids, vec![agent_id]);

        let _ = thread
            .thread
            .submit(Op::Shutdown {})
            .await
            .expect("shutdown should submit");
    }

    #[tokio::test]
    async fn resume_agent_restores_closed_agent_and_accepts_send_input() {
        let (mut session, turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();
        let config = turn.config.as_ref().clone();
        let thread = manager
            .resume_thread_with_history(
                config,
                InitialHistory::Forked(vec![RolloutItem::ResponseItem(ResponseItem::Message {
                    id: None,
                    role: "user".to_string(),
                    content: vec![ContentItem::InputText {
                        text: "materialized".to_string(),
                    }],
                    end_turn: None,
                    phase: None,
                })]),
                AuthManager::from_auth_for_testing(CodexAuth::from_api_key("dummy")),
                false,
            )
            .await
            .expect("start thread");
        let agent_id = thread.thread_id;
        let _ = manager
            .agent_control()
            .shutdown_agent(agent_id)
            .await
            .expect("shutdown agent");
        assert_eq!(
            manager.agent_control().get_status(agent_id).await,
            AgentStatus::NotFound
        );
        let session = Arc::new(session);
        let turn = Arc::new(turn);

        let resume_invocation = invocation(
            session.clone(),
            turn.clone(),
            "resume_agent",
            function_payload(json!({"id": agent_id.to_string()})),
        );
        let output = MultiAgentHandler
            .handle(resume_invocation)
            .await
            .expect("resume_agent should succeed");
        let ToolOutput::Function {
            body: FunctionCallOutputBody::Text(content),
            success,
            ..
        } = output
        else {
            panic!("expected function output");
        };
        let result: resume_agent::ResumeAgentResult =
            serde_json::from_str(&content).expect("resume_agent result should be json");
        assert_ne!(result.status, AgentStatus::NotFound);
        assert_eq!(success, Some(true));

        let send_invocation = invocation(
            session,
            turn,
            "send_input",
            function_payload(json!({"id": agent_id.to_string(), "message": "hello"})),
        );
        let output = MultiAgentHandler
            .handle(send_invocation)
            .await
            .expect("send_input should succeed after resume");
        let ToolOutput::Function {
            body: FunctionCallOutputBody::Text(content),
            success,
            ..
        } = output
        else {
            panic!("expected function output");
        };
        let result: serde_json::Value =
            serde_json::from_str(&content).expect("send_input result should be json");
        let submission_id = result
            .get("submission_id")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        assert!(!submission_id.is_empty());
        assert_eq!(success, Some(true));

        let _ = manager
            .agent_control()
            .shutdown_agent(agent_id)
            .await
            .expect("shutdown resumed agent");
    }

    #[tokio::test]
    async fn resume_agent_rejects_when_depth_limit_exceeded() {
        let (mut session, mut turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();

        let max_depth = turn.config.agent_max_depth;
        turn.session_source = SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id: session.conversation_id,
            depth: max_depth,
            agent_nickname: None,
            agent_role: None,
        });

        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "resume_agent",
            function_payload(json!({"id": ThreadId::new().to_string()})),
        );
        let Err(err) = MultiAgentHandler.handle(invocation).await else {
            panic!("resume should fail when depth limit exceeded");
        };
        assert_eq!(
            err,
            FunctionCallError::RespondToModel(
                "Agent depth limit reached. Solve the task yourself.".to_string()
            )
        );
    }

    #[tokio::test]
    async fn wait_rejects_non_positive_timeout() {
        let (session, turn) = make_session_and_context().await;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "wait",
            function_payload(json!({
                "ids": [ThreadId::new().to_string()],
                "timeout_ms": 0
            })),
        );
        let Err(err) = MultiAgentHandler.handle(invocation).await else {
            panic!("non-positive timeout should be rejected");
        };
        assert_eq!(
            err,
            FunctionCallError::RespondToModel("timeout_ms must be greater than zero".to_string())
        );
    }

    #[tokio::test]
    async fn wait_rejects_disable_timeout_with_timeout_ms() {
        let (session, turn) = make_session_and_context().await;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "wait",
            function_payload(json!({
                "ids": [ThreadId::new().to_string()],
                "disable_timeout": true,
                "timeout_ms": 1000
            })),
        );
        let Err(err) = MultiAgentHandler.handle(invocation).await else {
            panic!("disable_timeout + timeout_ms should be rejected");
        };
        assert_eq!(
            err,
            FunctionCallError::RespondToModel(
                "disable_timeout cannot be combined with timeout_ms".to_string()
            )
        );
    }

    #[tokio::test]
    async fn wait_rejects_invalid_id() {
        let (session, turn) = make_session_and_context().await;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "wait",
            function_payload(json!({"ids": ["invalid"]})),
        );
        let Err(err) = MultiAgentHandler.handle(invocation).await else {
            panic!("invalid id should be rejected");
        };
        let FunctionCallError::RespondToModel(msg) = err else {
            panic!("expected respond-to-model error");
        };
        assert!(msg.starts_with("invalid agent id invalid:"));
    }

    #[tokio::test]
    async fn wait_rejects_empty_ids() {
        let (session, turn) = make_session_and_context().await;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "wait",
            function_payload(json!({"ids": []})),
        );
        let Err(err) = MultiAgentHandler.handle(invocation).await else {
            panic!("empty ids should be rejected");
        };
        assert_eq!(
            err,
            FunctionCallError::RespondToModel("ids must be non-empty".to_string())
        );
    }

    #[tokio::test]
    async fn wait_rejects_duplicate_ids() {
        let (session, turn) = make_session_and_context().await;
        let duplicate_id = ThreadId::new();
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "wait",
            function_payload(json!({
                "ids": [duplicate_id.to_string(), duplicate_id.to_string()]
            })),
        );
        let Err(err) = MultiAgentHandler.handle(invocation).await else {
            panic!("duplicate ids should be rejected");
        };
        assert_eq!(
            err,
            FunctionCallError::RespondToModel("ids must not contain duplicates".to_string())
        );
    }

    #[tokio::test]
    async fn wait_returns_not_found_for_missing_agents() {
        let (mut session, turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();
        let id_a = ThreadId::new();
        let id_b = ThreadId::new();
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "wait",
            function_payload(json!({
                "ids": [id_a.to_string(), id_b.to_string()],
                "timeout_ms": 1000
            })),
        );
        let output = MultiAgentHandler
            .handle(invocation)
            .await
            .expect("wait should succeed");
        let ToolOutput::Function {
            body: FunctionCallOutputBody::Text(content),
            success,
            ..
        } = output
        else {
            panic!("expected function output");
        };
        let result: wait::WaitResult =
            serde_json::from_str(&content).expect("wait result should be json");
        assert_eq!(
            result,
            wait::WaitResult {
                agents: HashMap::from([
                    (
                        id_a,
                        AgentRuntimeState {
                            status: AgentStatus::NotFound,
                            last_activity: None,
                        },
                    ),
                    (
                        id_b,
                        AgentRuntimeState {
                            status: AgentStatus::NotFound,
                            last_activity: None,
                        },
                    ),
                ]),
                timed_out: false
            }
        );
        assert_eq!(success, None);
    }

    #[tokio::test]
    async fn wait_with_all_final_treats_not_found_as_final() {
        let (mut session, turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();
        let config = turn.config.as_ref().clone();
        let thread = manager.start_thread(config).await.expect("start thread");
        let existing_id = thread.thread_id;
        let missing_id = ThreadId::new();

        let _ = thread
            .thread
            .submit(Op::Shutdown {})
            .await
            .expect("shutdown should submit");

        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "wait",
            function_payload(json!({
                "ids": [missing_id.to_string(), existing_id.to_string()],
                "return_when": "all_final"
            })),
        );
        let output = MultiAgentHandler
            .handle(invocation)
            .await
            .expect("wait should succeed");
        let ToolOutput::Function {
            body: FunctionCallOutputBody::Text(content),
            success,
            ..
        } = output
        else {
            panic!("expected function output");
        };
        let result: wait::WaitResult =
            serde_json::from_str(&content).expect("wait result should be json");
        assert_eq!(
            result,
            wait::WaitResult {
                agents: HashMap::from([
                    (
                        missing_id,
                        AgentRuntimeState {
                            status: AgentStatus::NotFound,
                            last_activity: None,
                        },
                    ),
                    (
                        existing_id,
                        AgentRuntimeState {
                            status: AgentStatus::Shutdown,
                            last_activity: None,
                        },
                    ),
                ]),
                timed_out: false,
            }
        );
        assert_eq!(success, None);
    }

    #[tokio::test]
    async fn wait_times_out_when_status_is_not_final() {
        let (mut session, turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();
        let config = turn.config.as_ref().clone();
        let thread = manager.start_thread(config).await.expect("start thread");
        let agent_id = thread.thread_id;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "wait",
            function_payload(json!({
                "ids": [agent_id.to_string()],
                "timeout_ms": MIN_WAIT_TIMEOUT_MS
            })),
        );
        let output = MultiAgentHandler
            .handle(invocation)
            .await
            .expect("wait should succeed");
        let ToolOutput::Function {
            body: FunctionCallOutputBody::Text(content),
            success,
            ..
        } = output
        else {
            panic!("expected function output");
        };
        let result: wait::WaitResult =
            serde_json::from_str(&content).expect("wait result should be json");
        assert_eq!(
            result,
            wait::WaitResult {
                agents: HashMap::from([(
                    agent_id,
                    AgentRuntimeState {
                        status: AgentStatus::PendingInit,
                        last_activity: None,
                    },
                )]),
                timed_out: true
            }
        );
        assert_eq!(success, None);

        let _ = thread
            .thread
            .submit(Op::Shutdown {})
            .await
            .expect("shutdown should submit");
    }

    #[tokio::test]
    async fn wait_clamps_short_timeouts_to_minimum() {
        let (mut session, turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();
        let config = turn.config.as_ref().clone();
        let thread = manager.start_thread(config).await.expect("start thread");
        let agent_id = thread.thread_id;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "wait",
            function_payload(json!({
                "ids": [agent_id.to_string()],
                "timeout_ms": 10
            })),
        );

        let early = timeout(
            Duration::from_millis(50),
            MultiAgentHandler.handle(invocation),
        )
        .await;
        assert!(
            early.is_err(),
            "wait should not return before the minimum timeout clamp"
        );

        let _ = thread
            .thread
            .submit(Op::Shutdown {})
            .await
            .expect("shutdown should submit");
    }

    #[tokio::test]
    async fn wait_returns_immediately_when_agent_is_already_final() {
        let (mut session, turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();
        let config = turn.config.as_ref().clone();
        let thread = manager.start_thread(config).await.expect("start thread");
        let agent_id = thread.thread_id;
        let mut status_rx = manager
            .agent_control()
            .subscribe_status(agent_id)
            .await
            .expect("subscribe should succeed");

        let _ = thread
            .thread
            .submit(Op::Shutdown {})
            .await
            .expect("shutdown should submit");
        let _ = timeout(Duration::from_secs(1), status_rx.changed())
            .await
            .expect("shutdown status should arrive");

        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "wait",
            function_payload(json!({
                "ids": [agent_id.to_string()],
                "timeout_ms": 1000
            })),
        );
        let output = MultiAgentHandler
            .handle(invocation)
            .await
            .expect("wait should succeed");
        let ToolOutput::Function {
            body: FunctionCallOutputBody::Text(content),
            success,
            ..
        } = output
        else {
            panic!("expected function output");
        };
        let result: wait::WaitResult =
            serde_json::from_str(&content).expect("wait result should be json");
        assert_eq!(
            result,
            wait::WaitResult {
                agents: HashMap::from([(
                    agent_id,
                    AgentRuntimeState {
                        status: AgentStatus::Shutdown,
                        last_activity: None,
                    },
                )]),
                timed_out: false
            }
        );
        assert_eq!(success, None);
    }

    #[tokio::test]
    async fn wait_with_disable_timeout_returns_after_in_flight_any_final_transition() {
        let (mut session, turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();
        let config = turn.config.as_ref().clone();
        let thread = manager.start_thread(config).await.expect("start thread");
        let agent_id = thread.thread_id;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "wait",
            function_payload(json!({
                "ids": [agent_id.to_string()],
                "disable_timeout": true
            })),
        );

        let mut wait_task = tokio::spawn(async move { MultiAgentHandler.handle(invocation).await });
        let early = timeout(Duration::from_millis(50), &mut wait_task).await;
        assert!(
            early.is_err(),
            "disable_timeout wait should remain pending until a final status arrives"
        );

        let _ = thread
            .thread
            .submit(Op::Shutdown {})
            .await
            .expect("shutdown should submit");

        let output = timeout(Duration::from_secs(1), &mut wait_task)
            .await
            .expect("wait should complete after final status")
            .expect("wait task should join")
            .expect("wait should succeed");
        let ToolOutput::Function {
            body: FunctionCallOutputBody::Text(content),
            success,
            ..
        } = output
        else {
            panic!("expected function output");
        };
        let result: wait::WaitResult =
            serde_json::from_str(&content).expect("wait result should be json");
        assert_eq!(
            result,
            wait::WaitResult {
                agents: HashMap::from([(
                    agent_id,
                    AgentRuntimeState {
                        status: AgentStatus::Shutdown,
                        last_activity: None,
                    },
                )]),
                timed_out: false,
            }
        );
        assert_eq!(success, None);
    }

    #[tokio::test]
    async fn wait_with_all_final_times_out_while_any_requested_agent_is_non_final() {
        let (mut session, turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();
        let config = turn.config.as_ref().clone();
        let completed_thread = manager
            .start_thread(config.clone())
            .await
            .expect("start completed thread");
        let running_thread = manager
            .start_thread(config)
            .await
            .expect("start running thread");
        let completed_id = completed_thread.thread_id;
        let running_id = running_thread.thread_id;
        let mut status_rx = manager
            .agent_control()
            .subscribe_status(completed_id)
            .await
            .expect("subscribe should succeed");

        let _ = completed_thread
            .thread
            .submit(Op::Shutdown {})
            .await
            .expect("shutdown should submit");
        let _ = timeout(Duration::from_secs(1), status_rx.changed())
            .await
            .expect("shutdown status should arrive");

        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "wait",
            function_payload(json!({
                "ids": [completed_id.to_string(), running_id.to_string()],
                "return_when": "all_final",
                "timeout_ms": MIN_WAIT_TIMEOUT_MS
            })),
        );
        let output = MultiAgentHandler
            .handle(invocation)
            .await
            .expect("wait should succeed");
        let ToolOutput::Function {
            body: FunctionCallOutputBody::Text(content),
            success,
            ..
        } = output
        else {
            panic!("expected function output");
        };
        let result: wait::WaitResult =
            serde_json::from_str(&content).expect("wait result should be json");
        assert_eq!(
            result,
            wait::WaitResult {
                agents: HashMap::from([
                    (
                        completed_id,
                        AgentRuntimeState {
                            status: AgentStatus::Shutdown,
                            last_activity: None,
                        },
                    ),
                    (
                        running_id,
                        AgentRuntimeState {
                            status: AgentStatus::PendingInit,
                            last_activity: None,
                        },
                    ),
                ]),
                timed_out: true,
            }
        );
        assert_eq!(success, None);

        let _ = running_thread
            .thread
            .submit(Op::Shutdown {})
            .await
            .expect("shutdown should submit");
    }

    #[tokio::test]
    async fn wait_with_disable_timeout_and_all_final_returns_after_every_agent_is_final() {
        let (mut session, turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();
        let config = turn.config.as_ref().clone();
        let first_thread = manager
            .start_thread(config.clone())
            .await
            .expect("start first thread");
        let second_thread = manager
            .start_thread(config)
            .await
            .expect("start second thread");
        let first_id = first_thread.thread_id;
        let second_id = second_thread.thread_id;
        let mut first_status_rx = manager
            .agent_control()
            .subscribe_status(first_id)
            .await
            .expect("subscribe first should succeed");

        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "wait",
            function_payload(json!({
                "ids": [first_id.to_string(), second_id.to_string()],
                "disable_timeout": true,
                "return_when": "all_final"
            })),
        );
        let mut wait_task = tokio::spawn(async move { MultiAgentHandler.handle(invocation).await });

        let _ = first_thread
            .thread
            .submit(Op::Shutdown {})
            .await
            .expect("first shutdown should submit");
        let _ = timeout(Duration::from_secs(1), first_status_rx.changed())
            .await
            .expect("first shutdown status should arrive");

        let early = timeout(Duration::from_millis(50), &mut wait_task).await;
        assert!(
            early.is_err(),
            "all_final should remain pending until every requested agent reaches a final status"
        );

        let _ = second_thread
            .thread
            .submit(Op::Shutdown {})
            .await
            .expect("second shutdown should submit");

        let output = timeout(Duration::from_secs(1), &mut wait_task)
            .await
            .expect("wait should complete after every final status")
            .expect("wait task should join")
            .expect("wait should succeed");
        let ToolOutput::Function {
            body: FunctionCallOutputBody::Text(content),
            success,
            ..
        } = output
        else {
            panic!("expected function output");
        };
        let result: wait::WaitResult =
            serde_json::from_str(&content).expect("wait result should be json");
        assert_eq!(
            result,
            wait::WaitResult {
                agents: HashMap::from([
                    (
                        first_id,
                        AgentRuntimeState {
                            status: AgentStatus::Shutdown,
                            last_activity: None,
                        },
                    ),
                    (
                        second_id,
                        AgentRuntimeState {
                            status: AgentStatus::Shutdown,
                            last_activity: None,
                        },
                    ),
                ]),
                timed_out: false,
            }
        );
        assert_eq!(success, None);
    }

    #[tokio::test]
    async fn wait_returns_current_state_for_all_requested_agents_after_a_final_status() {
        let (mut session, turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();
        let config = turn.config.as_ref().clone();
        let completed_thread = manager
            .start_thread(config.clone())
            .await
            .expect("start completed thread");
        let running_thread = manager
            .start_thread(config)
            .await
            .expect("start running thread");
        let completed_id = completed_thread.thread_id;
        let running_id = running_thread.thread_id;
        let mut status_rx = manager
            .agent_control()
            .subscribe_status(completed_id)
            .await
            .expect("subscribe should succeed");

        let _ = completed_thread
            .thread
            .submit(Op::Shutdown {})
            .await
            .expect("shutdown should submit");
        let _ = timeout(Duration::from_secs(1), status_rx.changed())
            .await
            .expect("shutdown status should arrive");

        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "wait",
            function_payload(json!({
                "ids": [completed_id.to_string(), running_id.to_string()],
                "timeout_ms": 1000
            })),
        );
        let output = MultiAgentHandler
            .handle(invocation)
            .await
            .expect("wait should succeed");
        let ToolOutput::Function {
            body: FunctionCallOutputBody::Text(content),
            success,
            ..
        } = output
        else {
            panic!("expected function output");
        };
        let result: wait::WaitResult =
            serde_json::from_str(&content).expect("wait result should be json");
        assert_eq!(
            result,
            wait::WaitResult {
                agents: HashMap::from([
                    (
                        completed_id,
                        AgentRuntimeState {
                            status: AgentStatus::Shutdown,
                            last_activity: None,
                        },
                    ),
                    (
                        running_id,
                        AgentRuntimeState {
                            status: AgentStatus::PendingInit,
                            last_activity: None,
                        },
                    ),
                ]),
                timed_out: false,
            }
        );
        assert_eq!(success, None);

        let _ = running_thread
            .thread
            .submit(Op::Shutdown {})
            .await
            .expect("shutdown should submit");
    }

    #[tokio::test]
    async fn close_agent_submits_shutdown_and_returns_status() {
        let (mut session, turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();
        let config = turn.config.as_ref().clone();
        let thread = manager.start_thread(config).await.expect("start thread");
        let agent_id = thread.thread_id;
        let status_before = manager.agent_control().get_status(agent_id).await;

        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "close_agent",
            function_payload(json!({"id": agent_id.to_string()})),
        );
        let output = MultiAgentHandler
            .handle(invocation)
            .await
            .expect("close_agent should succeed");
        let ToolOutput::Function {
            body: FunctionCallOutputBody::Text(content),
            success,
            ..
        } = output
        else {
            panic!("expected function output");
        };
        let result: close_agent::CloseAgentResult =
            serde_json::from_str(&content).expect("close_agent result should be json");
        assert_eq!(result.status, status_before);
        assert_eq!(success, Some(true));

        let ops = manager.captured_ops();
        let submitted_shutdown = ops
            .iter()
            .any(|(id, op)| *id == agent_id && matches!(op, Op::Shutdown));
        assert_eq!(submitted_shutdown, true);

        let status_after = manager.agent_control().get_status(agent_id).await;
        assert_eq!(status_after, AgentStatus::NotFound);
    }

    #[tokio::test]
    async fn close_agent_rejects_active_agent_when_preemption_disabled() {
        let (mut session, turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();
        let config = turn.config.as_ref().clone();
        let thread = manager.start_thread(config).await.expect("start thread");
        let agent_id = thread.thread_id;
        let turn = with_running_subagent_preemption(turn, false);
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "close_agent",
            function_payload(json!({"id": agent_id.to_string()})),
        );

        let Err(err) = MultiAgentHandler.handle(invocation).await else {
            panic!("close_agent should be rejected for active agent");
        };
        let FunctionCallError::RespondToModel(message) = err else {
            panic!("expected respond-to-model error");
        };
        assert!(message.contains("agents.allow_running_subagent_preemption"));
        assert!(message.contains("close"));
        assert!(message.contains(&agent_id.to_string()));
        assert!(message.contains("status"));

        let submitted_shutdown = manager
            .captured_ops()
            .iter()
            .any(|(id, op)| *id == agent_id && matches!(op, Op::Shutdown));
        assert!(!submitted_shutdown);

        let _ = thread
            .thread
            .submit(Op::Shutdown {})
            .await
            .expect("shutdown should submit");
    }

    #[tokio::test]
    async fn close_agent_allows_terminal_agent_when_preemption_disabled() {
        let (mut session, turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();
        let config = turn.config.as_ref().clone();
        let thread = manager.start_thread(config).await.expect("start thread");
        let agent_id = thread.thread_id;
        let mut status_rx = manager
            .agent_control()
            .subscribe_status(agent_id)
            .await
            .expect("subscribe should succeed");

        let _ = thread
            .thread
            .submit(Op::Shutdown {})
            .await
            .expect("shutdown should submit");
        let _ = timeout(Duration::from_secs(1), status_rx.changed())
            .await
            .expect("shutdown status should arrive");

        let turn = with_running_subagent_preemption(turn, false);
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "close_agent",
            function_payload(json!({"id": agent_id.to_string()})),
        );
        let output = MultiAgentHandler
            .handle(invocation)
            .await
            .expect("close_agent should allow terminal agent");
        let ToolOutput::Function {
            body: FunctionCallOutputBody::Text(content),
            success,
            ..
        } = output
        else {
            panic!("expected function output");
        };
        let result: close_agent::CloseAgentResult =
            serde_json::from_str(&content).expect("close_agent result should be json");
        assert_eq!(result.status, AgentStatus::Shutdown);
        assert_eq!(success, Some(true));
    }

    #[tokio::test]
    async fn build_agent_spawn_config_uses_turn_context_values() {
        fn pick_allowed_sandbox_policy(
            constraint: &crate::config::Constrained<SandboxPolicy>,
            base: SandboxPolicy,
        ) -> SandboxPolicy {
            let candidates = [
                SandboxPolicy::new_read_only_policy(),
                SandboxPolicy::new_workspace_write_policy(),
                SandboxPolicy::DangerFullAccess,
            ];
            candidates
                .into_iter()
                .find(|candidate| *candidate != base && constraint.can_set(candidate).is_ok())
                .unwrap_or(base)
        }

        let (_session, mut turn) = make_session_and_context().await;
        turn.developer_instructions = Some("dev".to_string());
        turn.compact_prompt = Some("compact".to_string());
        turn.shell_environment_policy = ShellEnvironmentPolicy {
            use_profile: true,
            ..ShellEnvironmentPolicy::default()
        };
        let temp_dir = tempfile::tempdir().expect("temp dir");
        turn.cwd = temp_dir.path().to_path_buf();
        turn.codex_linux_sandbox_exe = Some(PathBuf::from("/bin/echo"));
        let sandbox_policy = pick_allowed_sandbox_policy(
            &turn.config.permissions.sandbox_policy,
            turn.config.permissions.sandbox_policy.get().clone(),
        );
        turn.sandbox_policy
            .set(sandbox_policy)
            .expect("sandbox policy set");
        turn.approval_policy
            .set(AskForApproval::OnRequest)
            .expect("approval policy set");
        turn.windows_sandbox_level = WindowsSandboxLevel::Elevated;

        let config = build_agent_spawn_config(&turn).expect("spawn config");
        let mut expected = (*turn.config).clone();
        expected.base_instructions = Some(turn.model_info.get_model_instructions(turn.personality));
        expected.model = Some(turn.model_info.slug.clone());
        expected.model_provider = turn.provider.clone();
        expected.model_reasoning_effort = turn.reasoning_effort;
        expected.model_reasoning_summary = Some(turn.reasoning_summary);
        expected.developer_instructions = None;
        expected.user_instructions = None;
        expected.project_doc_max_bytes = 0;
        let _ = expected.features.disable(Feature::ChildAgentsMd);
        expected.compact_prompt = turn.compact_prompt.clone();
        expected.permissions.shell_environment_policy = turn.shell_environment_policy.clone();
        expected.codex_linux_sandbox_exe = turn.codex_linux_sandbox_exe.clone();
        expected.cwd = turn.cwd.clone();
        expected
            .permissions
            .approval_policy
            .set(AskForApproval::OnRequest)
            .expect("approval policy set");
        expected
            .permissions
            .sandbox_policy
            .set(turn.sandbox_policy.get().clone())
            .expect("sandbox policy set");
        expected.permissions.file_system_sandbox_policy =
            crate::protocol::FileSystemSandboxPolicy::from(turn.sandbox_policy.get());
        expected.permissions.network_sandbox_policy =
            crate::protocol::NetworkSandboxPolicy::from(turn.sandbox_policy.get());
        expected.permissions.windows_sandbox_mode = Some(WindowsSandboxModeToml::Elevated);
        assert_eq!(config, expected);
    }

    #[tokio::test]
    async fn build_agent_spawn_config_clears_base_user_instructions() {
        let (_session, mut turn) = make_session_and_context().await;
        let mut base_config = (*turn.config).clone();
        base_config.user_instructions = Some("base-user".to_string());
        turn.user_instructions = Some("resolved-user".to_string());
        turn.config = Arc::new(base_config.clone());

        let config = build_agent_spawn_config(&turn).expect("spawn config");

        assert_eq!(config.user_instructions, None);
    }

    #[tokio::test]
    async fn spawn_config_uses_child_model_default_instructions_without_subagent_override() {
        let (_session, turn) = make_session_and_context().await;

        let spawn_config = build_agent_spawn_config(&turn).expect("spawn config");

        assert_eq!(
            spawn_config.base_instructions,
            Some(turn.model_info.get_model_instructions(turn.personality))
        );
    }

    #[tokio::test]
    async fn finalize_spawn_config_reapplies_isolation_after_role_reload() {
        let (session, mut turn) = make_session_and_context().await;
        let codex_home = tempfile::tempdir().expect("codex home");
        let workspace = tempfile::tempdir().expect("workspace");
        tokio::fs::write(
            workspace.path().join("AGENTS.md"),
            "# AGENTS.md instructions for test\n\nrole reload should not leak this",
        )
        .await
        .expect("write AGENTS");
        let role_path = workspace.path().join("custom-role.toml");
        tokio::fs::write(&role_path, "model = \"o3\"")
            .await
            .expect("write role");

        let cfg = ConfigToml {
            developer_instructions: Some("lead dev".to_string()),
            project_doc_max_bytes: Some(4096),
            ..Default::default()
        };
        let mut loaded_config = load_test_config(cfg, &codex_home, &workspace);
        loaded_config.agent_roles.insert(
            "custom".to_string(),
            AgentRoleConfig {
                description: None,
                config_file: Some(role_path),
                nickname_candidates: None,
            },
        );
        turn.cwd = workspace.path().to_path_buf();
        turn.config = Arc::new(loaded_config);

        let mut spawn_config = build_agent_spawn_config(&turn).expect("spawn config");
        apply_role_to_config(&mut spawn_config, Some("custom"))
            .await
            .expect("apply role");
        apply_spawn_agent_runtime_overrides(&mut spawn_config, &turn).expect("runtime overrides");
        finalize_spawn_agent_prompt_config(
            &mut spawn_config,
            &turn,
            session.services.models_manager.as_ref(),
        )
        .await;

        let expected_model_info = session
            .services
            .models_manager
            .get_model_info(spawn_config.model.as_deref().expect("model"), &spawn_config)
            .await;
        assert_eq!(spawn_config.developer_instructions, None);
        assert_eq!(spawn_config.user_instructions, None);
        assert_eq!(spawn_config.project_doc_max_bytes, 0);
        assert_eq!(
            spawn_config.base_instructions,
            Some(expected_model_info.get_model_instructions(spawn_config.personality))
        );
    }

    #[tokio::test]
    async fn finalize_spawn_config_uses_subagent_file_after_profile_reload() {
        let (session, mut turn) = make_session_and_context().await;
        let codex_home = tempfile::tempdir().expect("codex home");
        let workspace = tempfile::tempdir().expect("workspace");
        tokio::fs::write(
            workspace.path().join("AGENTS.md"),
            "# AGENTS.md instructions for test\n\nprofile reload should not leak this",
        )
        .await
        .expect("write AGENTS");
        let subagent_path = workspace.path().join("subagent.md");
        tokio::fs::write(&subagent_path, "child-only profile instructions")
            .await
            .expect("write subagent instructions");

        let cfg = ConfigToml {
            developer_instructions: Some("lead dev".to_string()),
            project_doc_max_bytes: Some(4096),
            profiles: HashMap::from([(
                "child".to_string(),
                ConfigProfile {
                    model: Some("o3".to_string()),
                    subagent_instructions_file: Some(
                        AbsolutePathBuf::try_from(subagent_path).expect("absolute subagent path"),
                    ),
                    ..Default::default()
                },
            )]),
            ..Default::default()
        };
        let loaded_config = load_test_config(cfg, &codex_home, &workspace);
        turn.cwd = workspace.path().to_path_buf();
        turn.config = Arc::new(loaded_config);

        let mut spawn_config = build_agent_spawn_config(&turn).expect("spawn config");
        apply_spawn_agent_profile_override(&mut spawn_config, Some("child"))
            .expect("apply profile");
        apply_spawn_agent_runtime_overrides(&mut spawn_config, &turn).expect("runtime overrides");
        finalize_spawn_agent_prompt_config(
            &mut spawn_config,
            &turn,
            session.services.models_manager.as_ref(),
        )
        .await;

        assert_eq!(spawn_config.developer_instructions, None);
        assert_eq!(spawn_config.user_instructions, None);
        assert_eq!(spawn_config.project_doc_max_bytes, 0);
        assert_eq!(
            spawn_config.base_instructions,
            Some("child-only profile instructions".to_string())
        );
    }

    #[tokio::test]
    async fn build_agent_resume_config_clears_base_instructions() {
        let (_session, mut turn) = make_session_and_context().await;
        let mut base_config = (*turn.config).clone();
        base_config.base_instructions = Some("caller-base".to_string());
        turn.config = Arc::new(base_config);
        turn.approval_policy
            .set(AskForApproval::OnRequest)
            .expect("approval policy set");
        turn.windows_sandbox_level = WindowsSandboxLevel::RestrictedToken;

        let config = build_agent_resume_config(&turn, 0).expect("resume config");

        let mut expected = (*turn.config).clone();
        expected.base_instructions = None;
        expected.model = Some(turn.model_info.slug.clone());
        expected.model_provider = turn.provider.clone();
        expected.model_reasoning_effort = turn.reasoning_effort;
        expected.model_reasoning_summary = Some(turn.reasoning_summary);
        expected.developer_instructions = None;
        expected.user_instructions = None;
        expected.project_doc_max_bytes = 0;
        let _ = expected.features.disable(Feature::ChildAgentsMd);
        expected.compact_prompt = turn.compact_prompt.clone();
        expected.permissions.shell_environment_policy = turn.shell_environment_policy.clone();
        expected.codex_linux_sandbox_exe = turn.codex_linux_sandbox_exe.clone();
        expected.cwd = turn.cwd.clone();
        expected
            .permissions
            .approval_policy
            .set(AskForApproval::OnRequest)
            .expect("approval policy set");
        expected
            .permissions
            .sandbox_policy
            .set(turn.sandbox_policy.get().clone())
            .expect("sandbox policy set");
        expected.permissions.file_system_sandbox_policy =
            crate::protocol::FileSystemSandboxPolicy::from(turn.sandbox_policy.get());
        expected.permissions.network_sandbox_policy =
            crate::protocol::NetworkSandboxPolicy::from(turn.sandbox_policy.get());
        expected.permissions.windows_sandbox_mode = Some(WindowsSandboxModeToml::Unelevated);
        assert_eq!(config, expected);
    }

    #[tokio::test]
    async fn build_agent_spawn_config_keeps_disabled_windows_override() {
        let (_session, mut turn) = make_session_and_context().await;
        let mut base_config = (*turn.config).clone();
        base_config
            .features
            .enable(Feature::WindowsSandbox)
            .expect("enable windows sandbox feature");
        turn.config = Arc::new(base_config);
        turn.windows_sandbox_level = WindowsSandboxLevel::Disabled;

        let config = build_agent_spawn_config(&turn).expect("spawn config");

        assert_eq!(config.permissions.windows_sandbox_mode, None);
        assert!(!config.features.enabled(Feature::WindowsSandbox));
        assert!(!config.features.enabled(Feature::WindowsSandboxElevated));
    }

    #[tokio::test]
    async fn build_agent_resume_config_keeps_disabled_windows_override() {
        let (_session, mut turn) = make_session_and_context().await;
        let mut base_config = (*turn.config).clone();
        base_config
            .features
            .enable(Feature::WindowsSandboxElevated)
            .expect("enable elevated windows sandbox feature");
        turn.config = Arc::new(base_config);
        turn.windows_sandbox_level = WindowsSandboxLevel::Disabled;

        let config = build_agent_resume_config(&turn, 0).expect("resume config");

        assert_eq!(config.permissions.windows_sandbox_mode, None);
        assert!(!config.features.enabled(Feature::WindowsSandbox));
        assert!(!config.features.enabled(Feature::WindowsSandboxElevated));
    }
}
