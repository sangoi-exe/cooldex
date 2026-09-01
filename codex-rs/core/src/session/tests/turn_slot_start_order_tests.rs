use super::*;
use crate::tasks::SessionTaskContext;
use pretty_assertions::assert_eq;

struct BlockingTurnStartedTask {
    start_event_entered_tx: async_channel::Sender<()>,
    start_event_release_rx: async_channel::Receiver<()>,
}

impl SessionTask for BlockingTurnStartedTask {
    fn kind(&self) -> TaskKind {
        TaskKind::Regular
    }

    fn span_name(&self) -> &'static str {
        "session_task.blocking_turn_started"
    }

    fn emit_turn_started(
        &self,
        session: Arc<SessionTaskContext>,
        ctx: Arc<TurnContext>,
    ) -> impl std::future::Future<Output = ()> + Send {
        let start_event_entered_tx = self.start_event_entered_tx.clone();
        let start_event_release_rx = self.start_event_release_rx.clone();
        async move {
            start_event_entered_tx
                .send(())
                .await
                .expect("turn-start observer should remain open");
            start_event_release_rx
                .recv()
                .await
                .expect("turn-start event should be released");
            session
                .clone_session()
                .send_event(
                    ctx.as_ref(),
                    EventMsg::TurnStarted(TurnStartedEvent {
                        turn_id: ctx.sub_id.clone(),
                        trace_id: ctx.trace_id.clone(),
                        started_at: ctx.turn_timing_state.started_at_unix_secs().await,
                        model_context_window: ctx.model_context_window(),
                        collaboration_mode_kind: ctx.mode(),
                    }),
                )
                .await;
        }
    }

    async fn run(
        self: Arc<Self>,
        _session: Arc<Session>,
        _ctx: Arc<TurnContext>,
        _input: Vec<TurnInput>,
        cancellation_token: CancellationToken,
    ) -> SessionTaskResult {
        cancellation_token.cancelled().await;
        Ok(SessionTaskOutput::default())
    }
}

fn user_input(text: &str) -> Vec<UserInput> {
    vec![UserInput::Text {
        text: text.to_string(),
        text_elements: Vec::new(),
    }]
}

fn user_input_request(text: &str) -> codex_protocol::turn_input::TurnInputRequest {
    codex_protocol::turn_input::TurnInputRequest::user_input(user_input(text))
}

async fn recv_turn_started(rx: &async_channel::Receiver<Event>, expected_turn_id: &str) -> Event {
    timeout(Duration::from_secs(2), async {
        loop {
            let event = rx.recv().await.expect("event channel should remain open");
            if matches!(
                event.msg,
                EventMsg::TurnStarted(TurnStartedEvent { ref turn_id, .. })
                    if turn_id == expected_turn_id
            ) {
                return event;
            }
        }
    })
    .await
    .expect("expected TurnStarted")
}

async fn recv_turn_aborted(
    rx: &async_channel::Receiver<Event>,
    expected_turn_id: &str,
    expected_reason: TurnAbortReason,
) -> Event {
    timeout(Duration::from_secs(2), async {
        loop {
            let event = rx.recv().await.expect("event channel should remain open");
            if matches!(
                event.msg,
                EventMsg::TurnAborted(TurnAbortedEvent {
                    ref turn_id,
                    ref reason,
                    ..
                }) if turn_id.as_deref() == Some(expected_turn_id)
                    && reason == &expected_reason
            ) {
                return event;
            }
        }
    })
    .await
    .expect("expected TurnAborted")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replacement_waiters_release_only_after_successor_turn_started() {
    let (session, old_turn_context, rx) = make_session_and_context_with_rx().await;
    let (sealed_tx, sealed_rx) = async_channel::bounded(1);
    let (abort_started_tx, abort_started_rx) = async_channel::bounded(1);
    let (abort_release_tx, abort_release_rx) = async_channel::bounded(1);
    session
        .spawn_task(
            Arc::clone(&old_turn_context),
            Vec::new(),
            SealedAbortBarrierTask {
                mode: SealedAbortMode::Cooperative,
                sealed_tx,
                abort_started_tx,
                abort_release_rx,
            },
        )
        .await;
    timeout(Duration::from_secs(2), sealed_rx.recv())
        .await
        .expect("old task should seal steer admission")
        .expect("sealed-task observer should remain open");

    let replacement_turn_id = "replacement-with-blocked-start-event".to_string();
    let replacement_context = session
        .new_turn_with_default_settings(replacement_turn_id.clone(), Default::default())
        .await;
    let (start_event_entered_tx, start_event_entered_rx) = async_channel::bounded(1);
    let (start_event_release_tx, start_event_release_rx) = async_channel::bounded(1);
    let replacement = tokio::spawn({
        let session = Arc::clone(&session);
        async move {
            session
                .spawn_task(
                    replacement_context,
                    Vec::new(),
                    BlockingTurnStartedTask {
                        start_event_entered_tx,
                        start_event_release_rx,
                    },
                )
                .await;
        }
    });
    timeout(Duration::from_secs(2), abort_started_rx.recv())
        .await
        .expect("replacement should enter the old abort hook")
        .expect("abort observer should remain open");

    let fresh_text = "fresh input blocked behind successor TurnStarted";
    let fresh_handler = tokio::spawn({
        let session = Arc::clone(&session);
        async move {
            handlers::user_input_or_turn(
                &session,
                "fresh-waiter".to_string(),
                user_input_request(fresh_text),
                /*client_user_message_id*/ None,
                /*parent_turn_id*/ None,
            )
            .await;
        }
    });
    tokio::task::yield_now().await;
    assert!(!fresh_handler.is_finished());

    abort_release_tx
        .send(())
        .await
        .expect("old abort hook should still be waiting");
    recv_turn_aborted(&rx, &old_turn_context.sub_id, TurnAbortReason::Replaced).await;
    timeout(Duration::from_secs(2), start_event_entered_rx.recv())
        .await
        .expect("successor should reach its TurnStarted barrier")
        .expect("turn-start observer should remain open");
    assert!(
        !fresh_handler.is_finished(),
        "generation waiters must remain blocked until successor TurnStarted is emitted"
    );
    {
        let slot = session.active_turn.lock().await;
        let task = slot
            .running_task()
            .expect("successor should be installed behind the start barrier");
        assert_eq!(task.turn_context.sub_id, replacement_turn_id);
        assert_eq!(task.steer_admission, SteerAdmission::Starting);
    }

    start_event_release_tx
        .send(())
        .await
        .expect("successor start event should still be waiting");
    recv_turn_started(&rx, &replacement_turn_id).await;
    timeout(Duration::from_secs(2), fresh_handler)
        .await
        .expect("fresh waiter should finish after TurnStarted")
        .expect("fresh waiter task should not panic");
    timeout(Duration::from_secs(2), replacement)
        .await
        .expect("replacement should finish startup")
        .expect("replacement task should not panic");
    assert_eq!(
        session
            .input_queue
            .get_pending_input(&session.active_turn)
            .await
            .0,
        vec![TurnInput::UserInput {
            content: user_input(fresh_text),
            client_id: None,
        }]
    );

    session.abort_all_tasks(TurnAbortReason::Interrupted).await;
}
