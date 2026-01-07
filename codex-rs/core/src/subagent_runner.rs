use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use codex_protocol::ConversationId;
use codex_protocol::protocol::ApplyPatchApprovalRequestEvent;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ExecApprovalRequestEvent;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::ReviewDecision;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::Mutex;
use tokio::sync::Notify;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use crate::codex::Codex;
use crate::codex::Session;
use crate::codex::TurnContext;

/// Route approval requests emitted by a sub-agent to the parent session, then
/// deliver the user's decision back to the sub-agent.
///
/// Returns `true` when the event was an approval request (and therefore was
/// consumed/handled by this function).
pub(crate) async fn maybe_route_subagent_approval(
    codex: &Codex,
    parent_session: &Session,
    parent_turn: &TurnContext,
    cancel_token: &CancellationToken,
    event: &Event,
) -> bool {
    match &event.msg {
        EventMsg::ExecApprovalRequest(ev) => {
            handle_exec_approval(
                codex,
                event.id.clone(),
                parent_session,
                parent_turn,
                ev.clone(),
                cancel_token,
            )
            .await;
            true
        }
        EventMsg::ApplyPatchApprovalRequest(ev) => {
            handle_patch_approval(
                codex,
                event.id.clone(),
                parent_session,
                parent_turn,
                ev.clone(),
                cancel_token,
            )
            .await;
            true
        }
        _ => false,
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BackgroundAgentStatus {
    Running,
    Completed,
    Errored,
    Aborted,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct BackgroundAgentSnapshot {
    pub(crate) status: BackgroundAgentStatus,
    pub(crate) result: Option<Value>,
    pub(crate) error: Option<String>,
    pub(crate) rollout_path: Option<String>,
    pub(crate) elapsed_ms: u128,
    pub(crate) last_message: Option<String>,
}

#[derive(Debug)]
struct BackgroundAgentState {
    status: BackgroundAgentStatus,
    result: Option<Value>,
    error: Option<String>,
    rollout_path: Option<PathBuf>,
    last_message: Option<String>,
    started_at: Instant,
    finished_at: Option<Instant>,
}

impl BackgroundAgentState {
    fn snapshot(&self) -> BackgroundAgentSnapshot {
        let end = self.finished_at.unwrap_or_else(Instant::now);
        BackgroundAgentSnapshot {
            status: self.status,
            result: self.result.clone(),
            error: self.error.clone(),
            rollout_path: self.rollout_path.as_ref().map(|p| p.display().to_string()),
            elapsed_ms: end.duration_since(self.started_at).as_millis(),
            last_message: self.last_message.clone(),
        }
    }
}

pub(crate) struct BackgroundAgentHandle {
    codex: Arc<Codex>,
    state: Arc<Mutex<BackgroundAgentState>>,
    done: Arc<Notify>,
    cancel_token: CancellationToken,
    max_result_bytes: usize,
}

impl BackgroundAgentHandle {
    pub(crate) fn new(codex: Arc<Codex>, max_result_bytes: usize) -> Arc<Self> {
        Arc::new(Self {
            codex,
            state: Arc::new(Mutex::new(BackgroundAgentState {
                status: BackgroundAgentStatus::Running,
                result: None,
                error: None,
                rollout_path: None,
                last_message: None,
                started_at: Instant::now(),
                finished_at: None,
            })),
            done: Arc::new(Notify::new()),
            cancel_token: CancellationToken::new(),
            max_result_bytes,
        })
    }

    pub(crate) async fn snapshot(&self) -> BackgroundAgentSnapshot {
        let state = self.state.lock().await;
        state.snapshot()
    }

    pub(crate) async fn wait_for_done(&self, timeout_duration: Duration) -> bool {
        let is_done = {
            let state = self.state.lock().await;
            !matches!(state.status, BackgroundAgentStatus::Running)
        };
        if is_done {
            return true;
        }
        timeout(timeout_duration, self.done.notified())
            .await
            .is_ok()
    }

    pub(crate) async fn cancel(&self) {
        self.cancel_token.cancel();
        shutdown_subagent(self.codex.as_ref()).await;
    }
}

#[derive(Default)]
pub(crate) struct AgentRegistry {
    agents: Arc<Mutex<HashMap<ConversationId, Arc<BackgroundAgentHandle>>>>,
}

impl AgentRegistry {
    pub(crate) async fn insert(&self, id: ConversationId, handle: Arc<BackgroundAgentHandle>) {
        let mut agents = self.agents.lock().await;
        agents.insert(id, handle);
    }

    pub(crate) async fn get(&self, id: &ConversationId) -> Option<Arc<BackgroundAgentHandle>> {
        let agents = self.agents.lock().await;
        agents.get(id).cloned()
    }

    pub(crate) async fn remove(&self, id: &ConversationId) -> Option<Arc<BackgroundAgentHandle>> {
        let mut agents = self.agents.lock().await;
        agents.remove(id)
    }
}

pub(crate) async fn spawn_background_agent_loop(
    handle: Arc<BackgroundAgentHandle>,
    registry: Arc<AgentRegistry>,
    agent_id: ConversationId,
) {
    let codex = Arc::clone(&handle.codex);
    let cancel_token = handle.cancel_token.child_token();
    let cancelled = cancel_token.cancelled();
    tokio::pin!(cancelled);

    loop {
        tokio::select! {
            _ = &mut cancelled => {
                update_agent_state(&handle, BackgroundAgentStatus::Aborted, None, Some("cancelled".to_string()), None).await;
                shutdown_subagent(codex.as_ref()).await;
                handle.done.notify_waiters();
                registry.remove(&agent_id).await;
                break;
            }
            event = codex.next_event() => {
                let event = match event {
                    Ok(event) => event,
                    Err(err) => {
                        update_agent_state(&handle, BackgroundAgentStatus::Errored, None, Some(format!("failed to receive sub-agent event: {err}")), None).await;
                        shutdown_subagent(codex.as_ref()).await;
                        handle.done.notify_waiters();
                        registry.remove(&agent_id).await;
                        break;
                    }
                };

                if let EventMsg::SessionConfigured(ev) = &event.msg {
                    let mut state = handle.state.lock().await;
                    state.rollout_path = Some(ev.rollout_path.clone());
                    continue;
                }

                if matches!(
                    event.msg,
                    EventMsg::ExecApprovalRequest(_)
                        | EventMsg::ApplyPatchApprovalRequest(_)
                        | EventMsg::ElicitationRequest(_)
                ) {
                    update_agent_state(
                        &handle,
                        BackgroundAgentStatus::Errored,
                        None,
                        Some("background agent requested approval/elicitation; approval_policy=never is required".to_string()),
                        None,
                    )
                    .await;
                    shutdown_subagent(codex.as_ref()).await;
                    handle.done.notify_waiters();
                    break;
                }

                match event.msg {
                    EventMsg::TaskComplete(ev) => {
                        let last_message = ev.last_agent_message;
                        let (result, error) = parse_agent_result(
                            last_message.clone(),
                            handle.max_result_bytes,
                        );
                        let status = if error.is_some() {
                            BackgroundAgentStatus::Errored
                        } else {
                            BackgroundAgentStatus::Completed
                        };
                        update_agent_state(&handle, status, result, error, last_message).await;
                        shutdown_subagent(codex.as_ref()).await;
                        handle.done.notify_waiters();
                        registry.remove(&agent_id).await;
                        break;
                    }
                    EventMsg::TurnAborted(ev) => {
                        update_agent_state(
                            &handle,
                            BackgroundAgentStatus::Aborted,
                            None,
                            Some(format!("{:?}", ev.reason)),
                            None,
                        )
                        .await;
                        shutdown_subagent(codex.as_ref()).await;
                        handle.done.notify_waiters();
                        registry.remove(&agent_id).await;
                        break;
                    }
                    EventMsg::Error(ev) => {
                        update_agent_state(
                            &handle,
                            BackgroundAgentStatus::Errored,
                            None,
                            Some(ev.message),
                            None,
                        )
                        .await;
                        shutdown_subagent(codex.as_ref()).await;
                        handle.done.notify_waiters();
                        registry.remove(&agent_id).await;
                        break;
                    }
                    _ => {}
                }
            }
        }
    }
}

async fn update_agent_state(
    handle: &BackgroundAgentHandle,
    status: BackgroundAgentStatus,
    result: Option<Value>,
    error: Option<String>,
    last_message: Option<String>,
) {
    let mut state = handle.state.lock().await;
    state.status = status;
    state.result = result;
    state.error = error;
    state.last_message = last_message;
    state.finished_at = Some(Instant::now());
}

fn parse_agent_result(
    last_message: Option<String>,
    max_result_bytes: usize,
) -> (Option<Value>, Option<String>) {
    let Some(text) = last_message else {
        return (
            None,
            Some("sub-agent completed without a final message".to_string()),
        );
    };
    let value = match serde_json::from_str::<Value>(&text) {
        Ok(value) => value,
        Err(err) => {
            let truncated = text.chars().take(2048).collect::<String>();
            return (
                None,
                Some(format!(
                    "sub-agent returned non-JSON output: {err}; output (truncated): {truncated}"
                )),
            );
        }
    };
    let serialized = serde_json::to_string(&value).unwrap_or_default();
    if serialized.len() > max_result_bytes {
        return (
            None,
            Some(format!(
                "sub-agent result too large: {} bytes (limit: {max_result_bytes}); re-run with a more compact summary",
                serialized.len()
            )),
        );
    }
    (Some(value), None)
}

/// Ask the sub-agent to stop and drain its events so background sends do not hit a closed channel.
pub(crate) async fn shutdown_subagent(codex: &Codex) {
    let _ = codex.submit(Op::Interrupt).await;
    let _ = codex.submit(Op::Shutdown {}).await;

    let _ = timeout(Duration::from_millis(500), async {
        while let Ok(event) = codex.next_event().await {
            if matches!(event.msg, EventMsg::ShutdownComplete) {
                break;
            }
        }
    })
    .await;
}

async fn handle_exec_approval(
    codex: &Codex,
    subagent_turn_id: String,
    parent_session: &Session,
    parent_turn: &TurnContext,
    event: ExecApprovalRequestEvent,
    cancel_token: &CancellationToken,
) {
    // Race approval with cancellation to avoid hangs.
    let approval_fut = parent_session.request_command_approval(
        parent_turn,
        event.call_id.clone(),
        event.command,
        event.cwd,
        event.reason,
        event.proposed_execpolicy_amendment,
    );
    let decision = await_approval_with_cancel(
        approval_fut,
        parent_session,
        &parent_turn.sub_id,
        cancel_token,
    )
    .await;

    let _ = codex
        .submit(Op::ExecApproval {
            id: subagent_turn_id,
            decision,
        })
        .await;
}

async fn handle_patch_approval(
    codex: &Codex,
    subagent_turn_id: String,
    parent_session: &Session,
    parent_turn: &TurnContext,
    event: ApplyPatchApprovalRequestEvent,
    cancel_token: &CancellationToken,
) {
    let decision_rx = parent_session
        .request_patch_approval(
            parent_turn,
            event.call_id.clone(),
            event.changes,
            event.reason,
            event.grant_root,
        )
        .await;
    let decision = await_approval_with_cancel(
        async move { decision_rx.await.unwrap_or_default() },
        parent_session,
        &parent_turn.sub_id,
        cancel_token,
    )
    .await;

    let _ = codex
        .submit(Op::PatchApproval {
            id: subagent_turn_id,
            decision,
        })
        .await;
}

/// Await an approval decision, aborting on cancellation.
async fn await_approval_with_cancel<F>(
    fut: F,
    parent_session: &Session,
    parent_turn_id: &str,
    cancel_token: &CancellationToken,
) -> ReviewDecision
where
    F: core::future::Future<Output = ReviewDecision>,
{
    tokio::select! {
        biased;
        _ = cancel_token.cancelled() => {
            parent_session
                .notify_approval(parent_turn_id, ReviewDecision::Abort)
                .await;
            ReviewDecision::Abort
        }
        decision = fut => decision,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_channel::bounded;
    use codex_protocol::protocol::TurnAbortReason;
    use codex_protocol::protocol::TurnAbortedEvent;
    use pretty_assertions::assert_eq;
    use std::sync::atomic::AtomicU64;

    #[tokio::test]
    async fn shutdown_subagent_sends_interrupt_and_shutdown() {
        let (tx_sub, rx_sub) = bounded(crate::codex::SUBMISSION_CHANNEL_CAPACITY);
        let (tx_events, rx_events) = bounded(1);

        let codex = Codex {
            next_id: AtomicU64::new(0),
            tx_sub,
            rx_event: rx_events,
            agent_status: Default::default(),
        };

        tx_events
            .send(Event {
                id: "done".to_string(),
                msg: EventMsg::ShutdownComplete,
            })
            .await
            .unwrap();

        shutdown_subagent(&codex).await;

        let mut ops = Vec::new();
        while let Ok(sub) = rx_sub.try_recv() {
            ops.push(sub.op);
        }

        assert!(
            ops.iter().any(|op| matches!(op, Op::Interrupt)),
            "expected Interrupt op"
        );
        assert!(
            ops.iter().any(|op| matches!(op, Op::Shutdown)),
            "expected Shutdown op"
        );
    }

    #[tokio::test]
    async fn maybe_route_subagent_approval_aborts_on_cancelled_parent() {
        let (tx_sub, rx_sub) = bounded(crate::codex::SUBMISSION_CHANNEL_CAPACITY);
        let (_tx_events, rx_events) = bounded(1);

        let codex = Codex {
            next_id: AtomicU64::new(0),
            tx_sub,
            rx_event: rx_events,
            agent_status: Default::default(),
        };

        let (parent_session, parent_turn, _rx_evt) =
            crate::codex::make_session_and_context_with_rx().await;

        let cancel = CancellationToken::new();
        cancel.cancel();

        let event = Event {
            id: "sub-turn".to_string(),
            msg: EventMsg::ExecApprovalRequest(ExecApprovalRequestEvent {
                call_id: "call-1".to_string(),
                turn_id: "sub-turn".to_string(),
                command: vec!["echo".to_string(), "hi".to_string()],
                cwd: std::path::PathBuf::from("."),
                reason: None,
                proposed_execpolicy_amendment: None,
                parsed_cmd: Vec::new(),
            }),
        };

        assert!(
            maybe_route_subagent_approval(
                &codex,
                parent_session.as_ref(),
                parent_turn.as_ref(),
                &cancel,
                &event,
            )
            .await
        );

        let mut approval = None;
        while let Ok(sub) = rx_sub.try_recv() {
            if let Op::ExecApproval { id, decision } = sub.op {
                approval = Some((id, decision));
                break;
            }
        }

        let (id, decision) = approval.expect("expected sub-agent ExecApproval submission");
        assert_eq!(id, "sub-turn".to_string());
        assert_eq!(decision, ReviewDecision::Abort);
    }

    #[tokio::test]
    async fn maybe_route_subagent_approval_ignores_non_approval_events() {
        let (tx_sub, _rx_sub) = bounded(crate::codex::SUBMISSION_CHANNEL_CAPACITY);
        let (_tx_events, rx_events) = bounded(1);

        let codex = Codex {
            next_id: AtomicU64::new(0),
            tx_sub,
            rx_event: rx_events,
            agent_status: Default::default(),
        };

        let (parent_session, parent_turn, _rx_evt) =
            crate::codex::make_session_and_context_with_rx().await;

        let cancel = CancellationToken::new();

        let event = Event {
            id: "turn".to_string(),
            msg: EventMsg::TurnAborted(TurnAbortedEvent {
                reason: TurnAbortReason::Interrupted,
            }),
        };

        assert!(
            !maybe_route_subagent_approval(
                &codex,
                parent_session.as_ref(),
                parent_turn.as_ref(),
                &cancel,
                &event,
            )
            .await
        );
    }
}
