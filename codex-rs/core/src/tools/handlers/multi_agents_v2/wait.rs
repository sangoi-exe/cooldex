use super::*;
use crate::agent::status::is_final;
use crate::session::InputQueueActivity;
use crate::tools::handlers::multi_agents_spec::WaitAgentTimeoutOptions;
use crate::tools::handlers::multi_agents_spec::create_wait_agent_tool_v2;
use codex_protocol::ThreadId;
use codex_tools::ToolSpec;
use futures::StreamExt;
use futures::stream::FuturesUnordered;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::sync::watch;
use tokio::time::Instant;
use tokio::time::timeout_at;

#[derive(Default)]
pub(crate) struct Handler {
    options: WaitAgentTimeoutOptions,
}

impl Handler {
    pub(crate) fn new(options: WaitAgentTimeoutOptions) -> Self {
        Self { options }
    }
}

impl ToolExecutor<ToolInvocation> for Handler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain("wait_agent")
    }

    fn spec(&self) -> ToolSpec {
        create_wait_agent_tool_v2(self.options)
    }

    fn handle<'a>(&'a self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'a>
    where
        ToolInvocation: 'a,
    {
        Box::pin(self.handle_call(invocation))
    }
}

impl Handler {
    async fn handle_call(
        &self,
        invocation: ToolInvocation,
    ) -> Result<Box<dyn crate::tools::context::ToolOutput>, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            payload,
            call_id,
            ..
        } = invocation;
        let arguments = function_arguments(payload)?;
        let wait_mode = parse_arguments::<WaitArgs>(&arguments)?.into_mode()?;
        let turn_state = session
            .input_queue
            .turn_state_for_sub_id(&session.active_turn, &turn.sub_id)
            .await;
        let (mut activity_rx, pending_activity) = session
            .input_queue
            .subscribe_activity(turn_state.as_deref())
            .await;

        match wait_mode {
            WaitMode::Activity {
                requested_timeout_ms,
            } => {
                let min_timeout_ms = turn.config.multi_agent_v2.min_wait_timeout_ms;
                let max_timeout_ms = turn.config.multi_agent_v2.max_wait_timeout_ms;
                let default_timeout_ms = turn.config.multi_agent_v2.default_wait_timeout_ms;
                let timeout_ms = match requested_timeout_ms {
                    Some(ms) if ms > max_timeout_ms => {
                        return Err(FunctionCallError::RespondToModel(format!(
                            "timeout_ms must be at most {max_timeout_ms}"
                        )));
                    }
                    Some(ms) => ms.max(min_timeout_ms),
                    None => default_timeout_ms,
                };

                session
                    .emit_turn_item_started(
                        &turn,
                        &wait_tool_call_item(
                            call_id.clone(),
                            CollabAgentToolCallStatus::InProgress,
                            session.thread_id,
                            Vec::new(),
                            HashMap::new(),
                        ),
                    )
                    .await;

                let deadline = Instant::now() + Duration::from_millis(timeout_ms as u64);
                let outcome = wait_for_activity(&mut activity_rx, pending_activity, deadline).await;
                let result = WaitAgentResult::from_activity_outcome(
                    outcome,
                    requested_timeout_ms,
                    timeout_ms,
                );

                session
                    .emit_turn_item_completed(
                        &turn,
                        wait_tool_call_item(
                            call_id,
                            CollabAgentToolCallStatus::Completed,
                            session.thread_id,
                            Vec::new(),
                            HashMap::new(),
                        ),
                    )
                    .await;

                Ok(boxed_tool_output(result))
            }
            WaitMode::Condition {
                targets,
                return_when,
            } => {
                let mut targets = resolve_condition_targets(&session, &turn, targets).await?;
                let receiver_thread_ids = targets
                    .iter()
                    .map(|target| target.thread_id)
                    .collect::<Vec<_>>();

                session
                    .emit_turn_item_started(
                        &turn,
                        &wait_tool_call_item(
                            call_id.clone(),
                            CollabAgentToolCallStatus::InProgress,
                            session.thread_id,
                            receiver_thread_ids.clone(),
                            HashMap::new(),
                        ),
                    )
                    .await;

                let outcome = wait_for_condition(
                    session.as_ref(),
                    turn_state.as_deref(),
                    &mut targets,
                    return_when,
                    &mut activity_rx,
                )
                .await;
                let result = WaitAgentResult::from_condition(outcome, &targets);
                let lifecycle_status = match outcome {
                    WaitConditionOutcome::AnyFinal
                    | WaitConditionOutcome::AllFinal
                    | WaitConditionOutcome::MailboxActivity => {
                        CollabAgentToolCallStatus::Completed
                    }
                    WaitConditionOutcome::Errored => CollabAgentToolCallStatus::Failed,
                    WaitConditionOutcome::Steered => CollabAgentToolCallStatus::Interrupted,
                };

                session
                    .emit_turn_item_completed(
                        &turn,
                        wait_tool_call_item(
                            call_id,
                            lifecycle_status,
                            session.thread_id,
                            receiver_thread_ids,
                            condition_target_lifecycle_states(&targets),
                        ),
                    )
                    .await;

                Ok(boxed_tool_output(result))
            }
        }
    }
}

fn wait_tool_call_item(
    id: String,
    status: CollabAgentToolCallStatus,
    sender_thread_id: ThreadId,
    receiver_thread_ids: Vec<ThreadId>,
    agents_states: HashMap<ThreadId, AgentStatus>,
) -> TurnItem {
    TurnItem::CollabAgentToolCall(CollabAgentToolCallItem {
        id,
        tool: CollabAgentTool::Wait,
        status,
        sender_thread_id,
        receiver_thread_ids,
        receiver_agents: Vec::new(),
        prompt: None,
        model: None,
        reasoning_effort: None,
        // Merge-safety anchor: V2 conditional lifecycle states redact only completed-message
        // bodies while preserving canonical error status payloads for existing consumers.
        agents_states,
    })
}

impl CoreToolRuntime for Handler {
    fn matches_kind(&self, payload: &ToolPayload) -> bool {
        matches!(payload, ToolPayload::Function { .. })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitArgs {
    timeout_ms: Option<i64>,
    targets: Option<Vec<String>>,
    return_when: Option<ReturnWhen>,
    disable_timeout: Option<bool>,
}

impl WaitArgs {
    fn into_mode(self) -> Result<WaitMode, FunctionCallError> {
        if self.timeout_ms.is_some() && self.disable_timeout.is_some() {
            return Err(FunctionCallError::RespondToModel(
                "timeout_ms cannot accompany disable_timeout".to_string(),
            ));
        }

        match (self.targets, self.return_when, self.disable_timeout) {
            (None, None, None) => Ok(WaitMode::Activity {
                requested_timeout_ms: self.timeout_ms,
            }),
            (Some(targets), Some(return_when), Some(true)) => {
                validate_condition_targets(&targets)?;
                Ok(WaitMode::Condition {
                    targets,
                    return_when,
                })
            }
            (None, Some(_), Some(true)) => Err(FunctionCallError::RespondToModel(
                "targets must be non-empty for a conditional wait".to_string(),
            )),
            (_, Some(_), _) => Err(FunctionCallError::RespondToModel(
                "return_when requires disable_timeout to be true".to_string(),
            )),
            (_, None, Some(_)) => Err(FunctionCallError::RespondToModel(
                "disable_timeout requires return_when".to_string(),
            )),
            (Some(_), None, None) => Err(FunctionCallError::RespondToModel(
                "targets require return_when and disable_timeout to be true".to_string(),
            )),
        }
    }
}

fn validate_condition_targets(targets: &[String]) -> Result<(), FunctionCallError> {
    if targets.is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "targets must be non-empty for a conditional wait".to_string(),
        ));
    }

    let mut seen = HashSet::with_capacity(targets.len());
    if targets.iter().any(|target| !seen.insert(target)) {
        return Err(FunctionCallError::RespondToModel(
            "targets must not contain duplicates".to_string(),
        ));
    }

    Ok(())
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ReturnWhen {
    AnyFinal,
    AllFinal,
}

enum WaitMode {
    Activity {
        requested_timeout_ms: Option<i64>,
    },
    Condition {
        targets: Vec<String>,
        return_when: ReturnWhen,
    },
}

struct ConditionTarget {
    reference: String,
    thread_id: ThreadId,
    status_rx: Option<watch::Receiver<AgentStatus>>,
    status: AgentStatus,
    initially_final: bool,
}

async fn resolve_condition_targets(
    session: &std::sync::Arc<crate::session::session::Session>,
    turn: &std::sync::Arc<crate::session::turn_context::TurnContext>,
    target_references: Vec<String>,
) -> Result<Vec<ConditionTarget>, FunctionCallError> {
    let mut targets = Vec::with_capacity(target_references.len());
    let mut resolved_thread_ids = HashSet::with_capacity(target_references.len());
    for reference in target_references {
        let thread_id = resolve_agent_target(session, turn, &reference)
            .await
            .map_err(|err| {
                FunctionCallError::RespondToModel(format!(
                    "target `{reference}` could not be resolved: {err:?}"
                ))
            })?;
        if !resolved_thread_ids.insert(thread_id) {
            return Err(FunctionCallError::RespondToModel(
                "targets must not resolve to the same agent".to_string(),
            ));
        }
        let mut status_rx = session
            .services
            .agent_control
            .subscribe_status(thread_id)
            .await
            .map_err(|err| {
                FunctionCallError::RespondToModel(format!(
                    "target `{reference}` could not be resolved: {err}"
                ))
            })?;
        let status = status_rx.borrow_and_update().clone();
        targets.push(ConditionTarget {
            reference,
            thread_id,
            initially_final: is_final(&status),
            status_rx: Some(status_rx),
            status,
        });
    }
    Ok(targets)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WaitConditionOutcome {
    AnyFinal,
    AllFinal,
    Errored,
    Steered,
    MailboxActivity,
}

async fn wait_for_condition(
    session: &crate::session::session::Session,
    turn_state: Option<&Mutex<crate::state::TurnState>>,
    targets: &mut [ConditionTarget],
    return_when: ReturnWhen,
    activity_rx: &mut watch::Receiver<InputQueueActivity>,
) -> WaitConditionOutcome {
    if let Some(outcome) =
        reconcile_condition_wait(session, turn_state, targets, return_when).await
    {
        return outcome;
    }

    let mut activity_closed = false;
    loop {
        let mut status_changes = FuturesUnordered::new();
        for (index, target) in targets.iter().enumerate() {
            if let Some(status_rx) = &target.status_rx {
                let mut status_rx = status_rx.clone();
                status_changes.push(async move { (index, status_rx.changed().await) });
            }
        }

        tokio::select! {
            Some((_index, _changed)) = status_changes.next() => {
                if let Some(outcome) =
                    reconcile_condition_wait(session, turn_state, targets, return_when).await
                {
                    return outcome;
                }
            }
            changed = activity_rx.changed(), if !activity_closed => {
                match changed {
                    Ok(()) => {
                        drop(activity_rx.borrow_and_update());
                        if let Some(outcome) =
                            reconcile_condition_wait(session, turn_state, targets, return_when).await
                        {
                            return outcome;
                        }
                    }
                    Err(_) => activity_closed = true,
                }
            }
        }
    }
}

// Merge-safety anchor: every V2 conditional-wait reconciliation refreshes target state and
// retires closed receivers through canonical lookup before lower-priority mailbox selection.
async fn reconcile_condition_wait(
    session: &crate::session::session::Session,
    turn_state: Option<&Mutex<crate::state::TurnState>>,
    targets: &mut [ConditionTarget],
    return_when: ReturnWhen,
) -> Option<WaitConditionOutcome> {
    for target in targets.iter_mut() {
        if target
            .status_rx
            .as_ref()
            .is_some_and(|status_rx| status_rx.has_changed().is_err())
        {
            retire_condition_target_status_receiver(session, target).await;
        }
    }
    refresh_condition_target_statuses(targets);

    let (_, pending_activity) = session.input_queue.subscribe_activity(turn_state).await;

    if pending_activity == Some(InputQueueActivity::Steer) {
        return Some(WaitConditionOutcome::Steered);
    }
    if let Some(outcome) = condition_outcome(targets, return_when) {
        return Some(outcome);
    }
    if pending_activity == Some(InputQueueActivity::Mailbox) {
        return Some(WaitConditionOutcome::MailboxActivity);
    }
    None
}

fn refresh_condition_target_statuses(targets: &mut [ConditionTarget]) {
    for target in targets {
        if let Some(status_rx) = target.status_rx.as_mut() {
            target.status = status_rx.borrow_and_update().clone();
        }
    }
}

async fn retire_condition_target_status_receiver(
    session: &crate::session::session::Session,
    target: &mut ConditionTarget,
) {
    if let Some(mut status_rx) = target.status_rx.take() {
        let status = status_rx.borrow_and_update().clone();
        target.status = if is_final(&status) {
            status
        } else {
            session
                .services
                .agent_control
                .get_status(target.thread_id)
                .await
        };
    }
}

fn condition_outcome(
    targets: &[ConditionTarget],
    return_when: ReturnWhen,
) -> Option<WaitConditionOutcome> {
    if targets
        .iter()
        .any(|target| matches!(target.status, AgentStatus::Errored(_)))
    {
        return Some(WaitConditionOutcome::Errored);
    }

    match return_when {
        ReturnWhen::AllFinal if targets.iter().all(|target| is_final(&target.status)) => {
            Some(WaitConditionOutcome::AllFinal)
        }
        ReturnWhen::AnyFinal if targets.iter().all(|target| target.initially_final) => {
            Some(WaitConditionOutcome::AllFinal)
        }
        ReturnWhen::AnyFinal
            if targets
                .iter()
                .any(|target| !target.initially_final && is_final(&target.status)) =>
        {
            Some(WaitConditionOutcome::AnyFinal)
        }
        ReturnWhen::AllFinal | ReturnWhen::AnyFinal => None,
    }
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct WaitAgentResult {
    pub(crate) message: String,
    pub(crate) timed_out: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) status: Option<BTreeMap<String, AgentStatus>>,
}

impl WaitAgentResult {
    fn from_activity_outcome(
        outcome: WaitActivityOutcome,
        requested_timeout_ms: Option<i64>,
        timeout_ms: i64,
    ) -> Self {
        let message = match outcome {
            WaitActivityOutcome::MailboxActivity => "Wait completed.",
            WaitActivityOutcome::Steered => "Wait interrupted by new input.",
            WaitActivityOutcome::TimedOut => "Wait timed out.",
        };
        let message = match requested_timeout_ms {
            Some(requested_timeout_ms) if requested_timeout_ms < timeout_ms => format!(
                "{message}\n\nRequested timeout of {requested_timeout_ms}ms was clamped to the minimum of {timeout_ms}ms."
            ),
            Some(_) | None => message.to_string(),
        };
        Self {
            message,
            timed_out: outcome == WaitActivityOutcome::TimedOut,
            status: None,
        }
    }

    fn from_condition(outcome: WaitConditionOutcome, targets: &[ConditionTarget]) -> Self {
        let message = match outcome {
            WaitConditionOutcome::AnyFinal => "Wait completed: a target reached a final status.",
            WaitConditionOutcome::AllFinal => "Wait completed: all targets are final.",
            WaitConditionOutcome::Errored => "Wait ended because a target errored.",
            WaitConditionOutcome::Steered => "Wait interrupted by new input.",
            WaitConditionOutcome::MailboxActivity => "Wait completed.",
        };
        Self {
            message: message.to_string(),
            timed_out: false,
            status: Some(
                targets
                    .iter()
                    .map(|target| {
                        (
                            target.reference.clone(),
                            redacted_condition_target_status(&target.status),
                        )
                    })
                    .collect(),
            ),
        }
    }
}

fn condition_target_lifecycle_states(
    targets: &[ConditionTarget],
) -> HashMap<ThreadId, AgentStatus> {
    targets
        .iter()
        .map(|target| {
            (
                target.thread_id,
                redacted_condition_target_status(&target.status),
            )
        })
        .collect()
}

fn redacted_condition_target_status(status: &AgentStatus) -> AgentStatus {
    match status {
        AgentStatus::Completed(_) => AgentStatus::Completed(None),
        _ => status.clone(),
    }
}

impl ToolOutput for WaitAgentResult {
    fn log_output(&self) -> String {
        tool_output_json_text(self, "wait_agent")
    }

    fn success_for_logging(&self) -> bool {
        true
    }

    fn to_response_item(&self, call_id: &str, payload: &ToolPayload) -> ResponseInputItem {
        tool_output_response_item(call_id, payload, self, /*success*/ None, "wait_agent")
    }

    fn code_mode_result(&self, _payload: &ToolPayload) -> JsonValue {
        tool_output_code_mode_result(self, "wait_agent")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WaitActivityOutcome {
    MailboxActivity,
    Steered,
    TimedOut,
}

async fn wait_for_activity(
    activity_rx: &mut watch::Receiver<InputQueueActivity>,
    pending_activity: Option<InputQueueActivity>,
    deadline: Instant,
) -> WaitActivityOutcome {
    if let Some(activity) = pending_activity {
        return match activity {
            InputQueueActivity::Mailbox => WaitActivityOutcome::MailboxActivity,
            InputQueueActivity::Steer => WaitActivityOutcome::Steered,
        };
    }
    match timeout_at(deadline, activity_rx.changed()).await {
        Ok(Ok(())) => match *activity_rx.borrow_and_update() {
            InputQueueActivity::Mailbox => WaitActivityOutcome::MailboxActivity,
            InputQueueActivity::Steer => WaitActivityOutcome::Steered,
        },
        Ok(Err(_)) | Err(_) => WaitActivityOutcome::TimedOut,
    }
}
