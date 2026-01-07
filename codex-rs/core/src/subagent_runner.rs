use std::time::Duration;

use codex_protocol::protocol::ApplyPatchApprovalRequestEvent;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ExecApprovalRequestEvent;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::ReviewDecision;
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
