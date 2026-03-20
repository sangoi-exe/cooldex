// Merge-safety anchor: wait(any/all) and timeout semantics must stay aligned with the
// workspace-local collab runtime contract, app-server protocol, and operator surfaces.

use super::*;
use crate::agent::status::is_final;
use codex_protocol::protocol::CollabWaitReturnWhen;
use futures::StreamExt;
use futures::stream::FuturesUnordered;
use std::collections::HashMap;
use std::collections::HashSet;
use std::time::Duration;
use tokio::sync::watch::Receiver;
use tokio::time::Instant;
use tokio::time::timeout_at;

pub(crate) struct Handler;

struct StatusSubscription {
    thread_id: ThreadId,
    status_rx: Receiver<AgentStatus>,
    was_final_at_start: bool,
}

#[async_trait]
impl ToolHandler for Handler {
    type Output = WaitResult;

    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    fn matches_kind(&self, payload: &ToolPayload) -> bool {
        matches!(payload, ToolPayload::Function { .. })
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            payload,
            call_id,
            ..
        } = invocation;
        let arguments = function_arguments(payload)?;
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
        let condition_enabled = args.return_when.is_some();
        if condition_enabled && !args.disable_timeout {
            return Err(FunctionCallError::RespondToModel(
                "return_when requires disable_timeout=true".to_owned(),
            ));
        }
        if !condition_enabled && args.disable_timeout {
            return Err(FunctionCallError::RespondToModel(
                "disable_timeout requires return_when".to_owned(),
            ));
        }
        let return_when = args.return_when.unwrap_or_default();

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
                    wait_state: collab_wait_state(
                        return_when,
                        condition_enabled,
                        args.disable_timeout,
                        None,
                    ),
                }
                .into(),
            )
            .await;

        let mut status_subscriptions = Vec::with_capacity(receiver_thread_ids.len());
        let mut final_status_count = 0;
        for id in &receiver_thread_ids {
            match session.services.agent_control.subscribe_status(*id).await {
                Ok(rx) => {
                    let status = rx.borrow().clone();
                    let was_final_at_start = is_final(&status);
                    if was_final_at_start {
                        final_status_count += 1;
                    }
                    status_subscriptions.push(StatusSubscription {
                        thread_id: *id,
                        status_rx: rx,
                        was_final_at_start,
                    });
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
                                    return_when,
                                    condition_enabled,
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
            condition_enabled,
            return_when,
            final_status_count,
            receiver_thread_ids.len(),
        );
        if !condition_met {
            condition_met = wait_for_condition(
                session.clone(),
                status_subscriptions,
                return_when,
                timeout_ms.map(|ms| Instant::now() + Duration::from_millis(ms as u64)),
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

        session
            .send_event(
                &turn,
                CollabWaitingEndEvent {
                    sender_thread_id: session.conversation_id,
                    call_id,
                    agent_statuses,
                    statuses: statuses_map,
                    wait_state: collab_wait_state(
                        return_when,
                        condition_enabled,
                        args.disable_timeout,
                        Some(result.timed_out),
                    ),
                }
                .into(),
            )
            .await;

        Ok(result)
    }
}

#[derive(Debug, Deserialize)]
struct WaitArgs {
    ids: Vec<String>,
    #[serde(default)]
    disable_timeout: bool,
    return_when: Option<CollabWaitReturnWhen>,
    timeout_ms: Option<i64>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct WaitResult {
    pub(crate) agents: HashMap<ThreadId, AgentRuntimeState>,
    pub(crate) timed_out: bool,
}

impl ToolOutput for WaitResult {
    fn log_preview(&self) -> String {
        tool_output_json_text(self, "wait")
    }

    fn success_for_logging(&self) -> bool {
        true
    }

    fn to_response_item(&self, call_id: &str, payload: &ToolPayload) -> ResponseInputItem {
        tool_output_response_item(call_id, payload, self, None, "wait")
    }

    fn code_mode_result(&self, _payload: &ToolPayload) -> JsonValue {
        tool_output_code_mode_result(self, "wait")
    }
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
    condition_enabled: bool,
    return_when: CollabWaitReturnWhen,
    final_status_count: usize,
    total_status_count: usize,
) -> bool {
    if !condition_enabled {
        return final_status_count > 0;
    }
    match return_when {
        CollabWaitReturnWhen::AnyFinal => final_status_count == total_status_count,
        CollabWaitReturnWhen::AllFinal => final_status_count == total_status_count,
    }
}

async fn wait_for_condition(
    session: Arc<Session>,
    status_subscriptions: Vec<StatusSubscription>,
    return_when: CollabWaitReturnWhen,
    deadline: Option<Instant>,
) -> Result<bool, FunctionCallError> {
    match return_when {
        CollabWaitReturnWhen::AnyFinal => {
            wait_for_any_final(session, status_subscriptions, deadline).await
        }
        CollabWaitReturnWhen::AllFinal => {
            wait_for_all_final(session, status_subscriptions, deadline).await
        }
    }
}

async fn wait_for_any_final(
    session: Arc<Session>,
    status_subscriptions: Vec<StatusSubscription>,
    deadline: Option<Instant>,
) -> Result<bool, FunctionCallError> {
    let mut futures = FuturesUnordered::new();
    for subscription in status_subscriptions {
        if subscription.was_final_at_start {
            continue;
        }
        if is_final(&subscription.status_rx.borrow().clone()) {
            return Ok(true);
        }
        let session = session.clone();
        futures.push(wait_for_final_status(
            session,
            subscription.thread_id,
            subscription.status_rx,
        ));
    }
    if futures.is_empty() {
        return Ok(true);
    }
    wait_for_any_final_from_futures(&mut futures, deadline).await
}

async fn wait_for_any_final_from_futures<F>(
    futures: &mut FuturesUnordered<F>,
    deadline: Option<Instant>,
) -> Result<bool, FunctionCallError>
where
    F: std::future::Future<Output = Option<(ThreadId, AgentStatus)>>,
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
    status_subscriptions: Vec<StatusSubscription>,
    deadline: Option<Instant>,
) -> Result<bool, FunctionCallError> {
    let mut futures = FuturesUnordered::new();
    for subscription in status_subscriptions {
        if subscription.was_final_at_start || is_final(&subscription.status_rx.borrow().clone()) {
            continue;
        }
        let session = session.clone();
        futures.push(wait_for_final_status(
            session,
            subscription.thread_id,
            subscription.status_rx,
        ));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codex::make_session_and_context;
    use tokio::sync::watch;

    #[tokio::test]
    async fn wait_for_any_final_returns_success_when_filtered_set_is_empty() {
        let (session, _) = make_session_and_context().await;
        let (_status_tx, status_rx) = watch::channel(AgentStatus::Shutdown);

        let result = wait_for_any_final(
            Arc::new(session),
            vec![StatusSubscription {
                thread_id: ThreadId::new(),
                status_rx,
                was_final_at_start: true,
            }],
            None,
        )
        .await
        .expect("empty filtered any_final set should succeed");

        assert!(result);
    }

    #[tokio::test]
    async fn wait_for_any_final_returns_success_when_receiver_became_final_after_start() {
        let (session, _) = make_session_and_context().await;
        let (_finished_tx, finished_rx) = watch::channel(AgentStatus::Shutdown);
        let (_running_tx, running_rx) = watch::channel(AgentStatus::Running);

        let result = wait_for_any_final(
            Arc::new(session),
            vec![
                StatusSubscription {
                    thread_id: ThreadId::new(),
                    status_rx: finished_rx,
                    was_final_at_start: false,
                },
                StatusSubscription {
                    thread_id: ThreadId::new(),
                    status_rx: running_rx,
                    was_final_at_start: false,
                },
            ],
            None,
        )
        .await
        .expect("a receiver that became final after wait start should satisfy any_final");

        assert!(result);
    }
}
