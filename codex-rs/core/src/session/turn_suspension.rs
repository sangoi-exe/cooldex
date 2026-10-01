use super::handlers;
use super::session::Session;
use crate::state::TaskKind;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::turn_input::SuspendTurnOutcome;
use std::sync::Arc;
use std::time::Duration;
use tracing::warn;

// Merge-safety anchor: suspension flushes durable history, waits for compaction/recovery live
// publication before retirement, and only takes a still-running slot; terminal finalization
// retains event-delivery ownership of a finishing turn. Forced shutdown stays outside the permit.
pub(super) async fn suspend_turn_and_shutdown(
    session: &Arc<Session>,
    submission_id: String,
) -> CodexResult<SuspendTurnOutcome> {
    {
        let _persistence_guard = session.acquire_thread_settings_persistence().await;
        let active = session.active_turn.lock().await;
        let Some(active_turn) = active.as_ref() else {
            return Ok(SuspendTurnOutcome::NotActive);
        };
        if active_turn.finishing {
            return Ok(SuspendTurnOutcome::NotActive);
        }
        let Some(task) = active_turn.task.as_ref() else {
            return Ok(SuspendTurnOutcome::NotActive);
        };
        if task.kind != TaskKind::Regular {
            return Ok(SuspendTurnOutcome::UnsupportedTask);
        }
    }

    // This is a snapshot of currently loaded descendants, not a spawn-admission seal.
    // Previously closed descendants and concurrent future spawns remain best effort.
    if session
        .services
        .local_agent_runtime
        .list_live_agent_subtree_thread_ids(session.thread_id)
        .await?
        .len()
        > 1
    {
        return Ok(SuspendTurnOutcome::HasLiveDescendants);
    }

    let live_thread = session
        .live_thread_for_persistence("suspend an unfinished root turn")
        .map_err(|error| CodexErr::Fatal(error.to_string()))?;
    // Flush before canceling execution so a persistence failure leaves the original turn running.
    live_thread.flush().await.map_err(|error| {
        CodexErr::Fatal(format!("flush before root turn suspension failed: {error}"))
    })?;

    // The flush can yield while the active turn completes or changes. Recheck its kind while
    // taking the exact current ActiveTurn. Keep the persistence-publication permit through the
    // take and cancellation, so no late compaction or recovery publisher can cross retirement.
    let (turn_state, input_persisted, task) = {
        let _persistence_guard = session.acquire_thread_settings_persistence().await;
        let mut active = session.active_turn.lock().await;
        let Some(active_turn) = active.as_ref() else {
            return Ok(SuspendTurnOutcome::NotActive);
        };
        if active_turn.finishing {
            return Ok(SuspendTurnOutcome::NotActive);
        }
        let Some(task) = active_turn.task.as_ref() else {
            return Ok(SuspendTurnOutcome::NotActive);
        };
        if task.kind != TaskKind::Regular {
            return Ok(SuspendTurnOutcome::UnsupportedTask);
        }
        let mut active_turn = active.take().ok_or_else(|| {
            CodexErr::Fatal("accepted root turn suspension had no running turn".to_string())
        })?;
        let task = active_turn.task.take().ok_or_else(|| {
            CodexErr::Fatal("accepted root turn suspension had no running task".to_string())
        })?;
        task.cancellation_token.cancel();
        (
            active_turn.turn_state,
            active_turn.input_persisted.take(),
            task,
        )
    };
    let turn_id = task.turn_context.sub_id.clone();
    if let Some(sender) = input_persisted {
        let _ = sender.send(Err(
            crate::codex_thread::TryStartTurnIfIdleRejectionReason::TaskEndedBeforePersistence,
        ));
    }
    // Normal shutdown records a terminal turn event, preventing another worker from
    // recovering this turn under its original ID. Cancel the task without that event.
    task.turn_context
        .turn_metadata_state
        .cancel_git_enrichment_task();
    let mut task_handle = task.handle.detach();
    match tokio::time::timeout(
        Duration::from_millis(crate::tasks::GRACEFULL_INTERRUPTION_TIMEOUT_MS),
        &mut task_handle,
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            warn!(thread_id = %session.thread_id, %error, "suspended turn task exited abnormally");
        }
        Err(_) => {
            warn!(
                thread_id = %session.thread_id,
                "suspended turn task did not stop gracefully; aborting it"
            );
            task_handle.abort();
            let _ = task_handle.await;
        }
    }
    // Pending accepted input and interactive waiters live only in this process. Handoff
    // intentionally drops that state; persisting or replaying it needs a separate protocol.
    session
        .pending_user_message_admissions
        .complete_task_end(&turn_id);
    session
        .input_queue
        .clear_pending_for_turn_state(turn_state.as_ref())
        .await;

    // Stop all producers before flushing their final history and closing its writer.
    // If either persistence step fails, do not report success: the current worker
    // retains ownership until worker-failure recovery can take responsibility.
    handlers::shutdown_session_runtime(session, handlers::ActiveTurnShutdown::AlreadyRetired).await;
    live_thread.flush().await.map_err(|error| {
        CodexErr::Fatal(format!("flush after root turn suspension failed: {error}"))
    })?;
    live_thread.shutdown().await.map_err(|error| {
        CodexErr::Fatal(format!("close suspended root turn writer failed: {error}"))
    })?;
    // Announce completion only after extension cleanup and writer closure so a replacement
    // worker cannot write the same thread concurrently.
    session
        .deliver_event_raw(Event {
            id: submission_id,
            msg: EventMsg::ShutdownComplete,
        })
        .await;
    Ok(SuspendTurnOutcome::Suspended { turn_id })
}
