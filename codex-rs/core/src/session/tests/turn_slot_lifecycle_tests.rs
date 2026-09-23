use super::*;
use crate::tasks::MailboxParentProvenance;
use crate::tasks::RegularTask;
use codex_protocol::turn_input::SuspendTurnOutcome;
use codex_protocol::turn_input::TurnInput as SubmittedTurnInput;
use codex_protocol::turn_input::TurnInputMode;
use codex_protocol::turn_input::TurnInputRequest;
use pretty_assertions::assert_eq;

struct BlockingTurnStop {
    entered_tx: async_channel::Sender<()>,
    release_rx: async_channel::Receiver<()>,
}

struct ThreadIdleSignal(async_channel::Sender<()>);

impl codex_extension_api::ThreadLifecycleContributor<crate::config::Config> for ThreadIdleSignal {
    fn on_thread_idle<'a>(
        &'a self,
        _input: codex_extension_api::ThreadIdleInput<'a>,
    ) -> codex_extension_api::ExtensionFuture<'a, ()> {
        Box::pin(async move {
            self.0
                .send(())
                .await
                .expect("thread-idle observer should remain open");
        })
    }
}

impl codex_extension_api::TurnLifecycleContributor for BlockingTurnStop {
    fn on_turn_stop<'a>(
        &'a self,
        _input: codex_extension_api::TurnStopInput<'a>,
    ) -> codex_extension_api::ExtensionFuture<'a, ()> {
        Box::pin(async move {
            self.entered_tx
                .send(())
                .await
                .expect("turn-stop observer should remain open");
            self.release_rx
                .recv()
                .await
                .expect("turn-stop hook should be released");
        })
    }
}

fn user_input(text: &str) -> Vec<UserInput> {
    vec![UserInput::Text {
        text: text.to_string(),
        text_elements: Vec::new(),
    }]
}

async fn install_blocked_startup_prewarm(session: &Session) -> tokio::sync::oneshot::Sender<()> {
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        let _ = release_rx.await;
        Ok(test_model_client_session())
    });
    session
        .set_session_startup_prewarm(
            crate::session_startup_prewarm::SessionStartupPrewarmHandle::new(
                handle,
                std::time::Instant::now(),
                crate::client::WEBSOCKET_CONNECT_TIMEOUT,
            ),
        )
        .await;
    release_tx
}

async fn wait_for_starting_turn(session: &Session, turn_id: &str) {
    timeout(Duration::from_secs(2), async {
        loop {
            if session.active_turn.lock().await.starting_turn_id() == Some(turn_id) {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("turn slot should publish Starting");
}

async fn wait_for_running_turn(session: &Session, turn_id: &str) {
    timeout(Duration::from_secs(2), async {
        loop {
            if session.active_turn.lock().await.running_turn_id() == Some(turn_id) {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("turn slot should publish Running");
}

async fn recv_turn_complete(rx: &async_channel::Receiver<Event>, expected_turn_id: &str) -> Event {
    timeout(Duration::from_secs(2), async {
        loop {
            let event = rx.recv().await.expect("event channel should remain open");
            if matches!(
                event.msg,
                EventMsg::TurnComplete(TurnCompleteEvent { ref turn_id, .. })
                    if turn_id == expected_turn_id
            ) {
                return event;
            }
        }
    })
    .await
    .expect("expected TurnComplete")
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

fn count_user_message_text(
    items: impl IntoIterator<Item = impl std::borrow::Borrow<ResponseItem>>,
    expected_text: &str,
) -> usize {
    items
        .into_iter()
        .filter(|item| {
            let item = item.borrow();
            matches!(
                item,
                ResponseItem::Message {
                    role,
                    content,
                    ..
                } if role == "user"
                    && content == &vec![ContentItem::InputText {
                        text: expected_text.to_string(),
                    }]
            )
        })
        .count()
}

async fn start_gated_compaction_publication(
    session: &Arc<Session>,
    store: &GatedInMemoryThreadStore,
) -> (
    PostFlushGate,
    tokio::task::JoinHandle<codex_protocol::error::Result<()>>,
) {
    let (window_number, window_ids) = session.prepare_auto_compact_window().await;
    let gate = store.gate_next_flush();
    let compaction = tokio::spawn({
        let session = Arc::clone(session);
        async move {
            session
                .replace_compacted_history(
                    vec![ResponseItemEnvelope::new(user_message(
                        "gated compaction publication",
                    ))],
                    /*reference_context_item*/ None,
                    /*world_state_baseline*/ None,
                    CompactedHistoryMetadata {
                        message: "gated compaction publication".to_string(),
                        window_number,
                        window_ids,
                        compaction_response_id: None,
                        compaction_model_hash: None,
                        reviewer_compaction_hash: None,
                    },
                )
                .await
        }
    });
    timeout(Duration::from_secs(2), gate.wait_until_flushed())
        .await
        .expect("compaction should reach the post-flush test gate");
    (gate, compaction)
}

async fn finish_gated_compaction_publication(
    gate: &PostFlushGate,
    compaction: tokio::task::JoinHandle<codex_protocol::error::Result<()>>,
) {
    gate.release().await;
    timeout(Duration::from_secs(2), compaction)
        .await
        .expect("compaction should resume after publication gate release")
        .expect("compaction task should not panic")
        .expect("compaction should publish its durable checkpoint");
}

async fn start_regular_never_ending_task(session: &Arc<Session>, turn_context: &Arc<TurnContext>) {
    session
        .spawn_task(
            Arc::clone(turn_context),
            Vec::new(),
            NeverEndingTask {
                kind: TaskKind::Regular,
                listen_to_cancellation_token: true,
            },
        )
        .await;
    wait_for_running_turn(session.as_ref(), &turn_context.sub_id).await;
}

async fn wait_for_lifecycle_competitor_start(started: tokio::sync::oneshot::Receiver<()>) {
    timeout(Duration::from_secs(2), started)
        .await
        .expect("lifecycle competitor should start")
        .expect("lifecycle competitor start observer should remain open");
}

async fn wait_for_lifecycle_retirement(session: &Session) {
    timeout(Duration::from_secs(2), async {
        loop {
            if session.active_turn.lock().await.is_transitioning() {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("lifecycle should retire the active task before its deferred publication starts");
}

struct DeferredCompactionPublicationTask {
    cancelled_tx: async_channel::Sender<()>,
    start_rx: async_channel::Receiver<()>,
    rejected_tx: async_channel::Sender<bool>,
}

impl SessionTask for DeferredCompactionPublicationTask {
    fn kind(&self) -> TaskKind {
        TaskKind::Compact
    }

    fn span_name(&self) -> &'static str {
        "session_task.deferred_compaction_publication"
    }

    async fn run(
        self: Arc<Self>,
        session: Arc<Session>,
        _ctx: Arc<TurnContext>,
        _input: Vec<TurnInput>,
        cancellation_token: CancellationToken,
    ) -> SessionTaskResult {
        cancellation_token.cancelled().await;
        self.cancelled_tx
            .send(())
            .await
            .expect("test should observe deferred compaction task cancellation");
        self.start_rx
            .recv()
            .await
            .expect("test should release deferred compaction publication");
        let (window_number, window_ids) = session.prepare_auto_compact_window().await;
        let result = session
            .replace_compacted_history_for_task(
                &cancellation_token,
                vec![ResponseItemEnvelope::new(user_message(
                    "cancelled task compaction publication",
                ))],
                /*reference_context_item*/ None,
                /*world_state_baseline*/ None,
                CompactedHistoryMetadata {
                    message: "cancelled task compaction publication".to_string(),
                    window_number,
                    window_ids,
                    compaction_response_id: None,
                    compaction_model_hash: None,
                    reviewer_compaction_hash: None,
                },
            )
            .await;
        self.rejected_tx
            .send(result.is_err())
            .await
            .expect("test should observe deferred compaction publication admission");
        result?;
        Ok(Default::default())
    }
}

struct DeferredRecoveryPublicationTask {
    cancelled_tx: async_channel::Sender<()>,
    start_rx: async_channel::Receiver<()>,
    rejected_tx: async_channel::Sender<bool>,
    identity: PostCompactRecoveryIdentity,
}

impl SessionTask for DeferredRecoveryPublicationTask {
    fn kind(&self) -> TaskKind {
        TaskKind::Regular
    }

    fn span_name(&self) -> &'static str {
        "session_task.deferred_recovery_publication"
    }

    async fn run(
        self: Arc<Self>,
        session: Arc<Session>,
        ctx: Arc<TurnContext>,
        _input: Vec<TurnInput>,
        cancellation_token: CancellationToken,
    ) -> SessionTaskResult {
        cancellation_token.cancelled().await;
        self.cancelled_tx
            .send(())
            .await
            .expect("test should observe deferred recovery task cancellation");
        self.start_rx
            .recv()
            .await
            .expect("test should release deferred recovery publication");
        let result = session
            .record_post_compact_recovery_sampling_success_for_task(
                &self.identity,
                &ctx.sub_id,
                &cancellation_token,
            )
            .await;
        self.rejected_tx
            .send(result.is_err())
            .await
            .expect("test should observe deferred recovery publication admission");
        result?;
        Ok(Default::default())
    }
}

// Merge-safety anchor: lifecycle tests exercise direct TurnSlot admission so exact rejection
// payloads, post-flush publication boundaries, and independently owned transitions stay covered
// without retired adapters.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retirement_rejects_cancelled_task_compaction_publication_before_durable_append() {
    let (mut session, turn_context, rx) = make_session_and_context_with_rx().await;
    attach_in_memory_thread_store(Arc::get_mut(&mut session).expect("session should be unique"))
        .await;
    let (cancelled_tx, cancelled_rx) = async_channel::bounded(1);
    let (start_tx, start_rx) = async_channel::bounded(1);
    let (rejected_tx, rejected_rx) = async_channel::bounded(1);
    session
        .spawn_task(
            Arc::clone(&turn_context),
            Vec::new(),
            DeferredCompactionPublicationTask {
                cancelled_tx,
                start_rx,
                rejected_tx,
            },
        )
        .await;
    wait_for_running_turn(session.as_ref(), &turn_context.sub_id).await;

    let interrupt = tokio::spawn({
        let session = Arc::clone(&session);
        async move {
            session.abort_all_tasks(TurnAbortReason::Interrupted).await;
        }
    });
    wait_for_lifecycle_retirement(session.as_ref()).await;
    timeout(Duration::from_secs(2), cancelled_rx.recv())
        .await
        .expect("retired compaction task should observe cancellation")
        .expect("retired compaction task cancellation observer should remain open");
    start_tx
        .send(())
        .await
        .expect("deferred task should remain available during lifecycle retirement");
    assert!(
        timeout(Duration::from_secs(2), rejected_rx.recv())
            .await
            .expect("deferred compaction publication should report its admission result")
            .expect("deferred compaction publication observer should remain open"),
        "a retired task must reject compaction publication before its durable append"
    );
    timeout(Duration::from_secs(2), interrupt)
        .await
        .expect("interruption should finish after rejected publication")
        .expect("interruption task should not panic");
    recv_turn_aborted(&rx, &turn_context.sub_id, TurnAbortReason::Interrupted).await;

    let durable_items = session
        .live_thread()
        .expect("test live thread")
        .load_history(/*include_archived*/ false)
        .await
        .expect("durable thread history should be readable")
        .items;
    assert!(
        !durable_items
            .iter()
            .any(|item| matches!(item, RolloutItem::Compacted(_))),
        "a cancelled task must not append a compacted checkpoint after lifecycle retirement"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retirement_rejects_cancelled_task_recovery_publication_before_durable_proof() {
    let (mut session, turn_context, rx) = make_session_and_context_with_rx().await;
    attach_in_memory_thread_store(Arc::get_mut(&mut session).expect("session should be unique"))
        .await;
    let identity = install_test_post_compact_recovery(session.as_ref()).await;
    let (cancelled_tx, cancelled_rx) = async_channel::bounded(1);
    let (start_tx, start_rx) = async_channel::bounded(1);
    let (rejected_tx, rejected_rx) = async_channel::bounded(1);
    session
        .spawn_task(
            Arc::clone(&turn_context),
            Vec::new(),
            DeferredRecoveryPublicationTask {
                cancelled_tx,
                start_rx,
                rejected_tx,
                identity: identity.clone(),
            },
        )
        .await;
    wait_for_running_turn(session.as_ref(), &turn_context.sub_id).await;

    let interrupt = tokio::spawn({
        let session = Arc::clone(&session);
        async move {
            session.abort_all_tasks(TurnAbortReason::Interrupted).await;
        }
    });
    wait_for_lifecycle_retirement(session.as_ref()).await;
    timeout(Duration::from_secs(2), cancelled_rx.recv())
        .await
        .expect("retired recovery task should observe cancellation")
        .expect("retired recovery task cancellation observer should remain open");
    start_tx
        .send(())
        .await
        .expect("deferred task should remain available during lifecycle retirement");
    assert!(
        timeout(Duration::from_secs(2), rejected_rx.recv())
            .await
            .expect("deferred recovery publication should report its admission result")
            .expect("deferred recovery publication observer should remain open"),
        "a retired task must reject recovery publication before its durable proof"
    );
    timeout(Duration::from_secs(2), interrupt)
        .await
        .expect("interruption should finish after rejected publication")
        .expect("interruption task should not panic");
    recv_turn_aborted(&rx, &turn_context.sub_id, TurnAbortReason::Interrupted).await;

    let durable_items = session
        .live_thread()
        .expect("test live thread")
        .load_history(/*include_archived*/ false)
        .await
        .expect("durable thread history should be readable")
        .items;
    assert!(
        !durable_items.iter().any(|item| {
            matches!(
                item,
                RolloutItem::PostCompactRecoveryApplied(applied)
                    if applied.compaction_window_id == identity.compaction_window_id
                        && applied.boundary_item_id == identity.boundary_item_id
                        && applied.turn_id == turn_context.sub_id
            )
        }),
        "a cancelled task must not append a recovery proof after lifecycle retirement"
    );
    assert_eq!(
        session
            .state
            .lock()
            .await
            .post_compact_recovery
            .pending_identity(),
        Some(&identity),
        "a rejected recovery publication must not clear live recovery state"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interruption_waits_for_compaction_live_publication() {
    let (mut session, turn_context, rx) = make_session_and_context_with_rx().await;
    let store = attach_gated_in_memory_thread_store(
        Arc::get_mut(&mut session).expect("session should be uniquely owned"),
    )
    .await;
    start_regular_never_ending_task(&session, &turn_context).await;

    let (gate, compaction) = start_gated_compaction_publication(&session, store.as_ref()).await;
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let interrupt = tokio::spawn({
        let session = Arc::clone(&session);
        async move {
            started_tx
                .send(())
                .expect("interruption start observer should remain open");
            session.abort_all_tasks(TurnAbortReason::Interrupted).await;
        }
    });
    wait_for_lifecycle_competitor_start(started_rx).await;
    assert!(
        !interrupt.is_finished(),
        "interruption must wait for durable compaction publication"
    );
    assert_eq!(
        session.active_turn.lock().await.running_turn_id(),
        Some(turn_context.sub_id.as_str())
    );

    finish_gated_compaction_publication(&gate, compaction).await;
    timeout(Duration::from_secs(2), interrupt)
        .await
        .expect("interruption should finish after compaction publication")
        .expect("interruption task should not panic");
    recv_turn_aborted(&rx, &turn_context.sub_id, TurnAbortReason::Interrupted).await;
    assert!(session.active_turn.lock().await.is_idle());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn targeted_abort_waits_for_compaction_live_publication() {
    let (mut session, turn_context, rx) = make_session_and_context_with_rx().await;
    let store = attach_gated_in_memory_thread_store(
        Arc::get_mut(&mut session).expect("session should be uniquely owned"),
    )
    .await;
    start_regular_never_ending_task(&session, &turn_context).await;

    let (gate, compaction) = start_gated_compaction_publication(&session, store.as_ref()).await;
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let targeted_abort = tokio::spawn({
        let session = Arc::clone(&session);
        let turn_id = turn_context.sub_id.clone();
        async move {
            started_tx
                .send(())
                .expect("targeted-abort start observer should remain open");
            session
                .abort_turn_if_active(&turn_id, TurnAbortReason::Interrupted)
                .await
        }
    });
    wait_for_lifecycle_competitor_start(started_rx).await;
    assert!(
        !targeted_abort.is_finished(),
        "targeted abort must wait for durable compaction publication"
    );
    assert_eq!(
        session.active_turn.lock().await.running_turn_id(),
        Some(turn_context.sub_id.as_str())
    );

    finish_gated_compaction_publication(&gate, compaction).await;
    assert!(
        timeout(Duration::from_secs(2), targeted_abort)
            .await
            .expect("targeted abort should finish after compaction publication")
            .expect("targeted abort task should not panic")
    );
    recv_turn_aborted(&rx, &turn_context.sub_id, TurnAbortReason::Interrupted).await;
    assert!(session.active_turn.lock().await.is_idle());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replacement_waits_for_compaction_live_publication_before_successor_admission() {
    let (mut session, turn_context, rx) = make_session_and_context_with_rx().await;
    let store = attach_gated_in_memory_thread_store(
        Arc::get_mut(&mut session).expect("session should be uniquely owned"),
    )
    .await;
    start_regular_never_ending_task(&session, &turn_context).await;
    let successor_id = "successor-after-compaction-publication".to_string();
    let successor = session
        .new_turn_with_default_settings(successor_id.clone(), Default::default())
        .await;

    let (gate, compaction) = start_gated_compaction_publication(&session, store.as_ref()).await;
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let replacement = tokio::spawn({
        let session = Arc::clone(&session);
        async move {
            started_tx
                .send(())
                .expect("replacement start observer should remain open");
            session
                .spawn_task(
                    successor,
                    Vec::new(),
                    NeverEndingTask {
                        kind: TaskKind::Regular,
                        listen_to_cancellation_token: true,
                    },
                )
                .await;
        }
    });
    wait_for_lifecycle_competitor_start(started_rx).await;
    assert!(
        !replacement.is_finished(),
        "replacement must wait for durable compaction publication"
    );
    assert_eq!(
        session.active_turn.lock().await.running_turn_id(),
        Some(turn_context.sub_id.as_str())
    );

    finish_gated_compaction_publication(&gate, compaction).await;
    timeout(Duration::from_secs(2), replacement)
        .await
        .expect("replacement should finish after compaction publication")
        .expect("replacement task should not panic");
    recv_turn_aborted(&rx, &turn_context.sub_id, TurnAbortReason::Replaced).await;
    wait_for_running_turn(session.as_ref(), &successor_id).await;

    session.abort_all_tasks(TurnAbortReason::Interrupted).await;
    recv_turn_aborted(&rx, &successor_id, TurnAbortReason::Interrupted).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn suspension_waits_for_compaction_live_publication() {
    let thread_manager = crate::ThreadManager::with_models_provider_for_tests(
        CodexAuth::from_api_key("test"),
        built_in_model_providers(/*openai_base_url*/ None)["openai"].clone(),
    );
    let (mut session, turn_context, _rx) = make_session_and_context_with_rx().await;
    Arc::get_mut(&mut session)
        .expect("session should be uniquely owned")
        .services
        .agent_control = thread_manager.agent_control();
    let store = attach_gated_in_memory_thread_store(
        Arc::get_mut(&mut session).expect("session should be uniquely owned"),
    )
    .await;
    start_regular_never_ending_task(&session, &turn_context).await;

    let (gate, compaction) = start_gated_compaction_publication(&session, store.as_ref()).await;
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let suspension = tokio::spawn({
        let session = Arc::clone(&session);
        async move {
            started_tx
                .send(())
                .expect("suspension start observer should remain open");
            super::super::turn_suspension::suspend_turn_and_shutdown(
                &session,
                "suspend-after-compaction-publication".to_string(),
            )
            .await
        }
    });
    wait_for_lifecycle_competitor_start(started_rx).await;
    assert!(
        !suspension.is_finished(),
        "suspension must wait for durable compaction publication"
    );
    assert_eq!(
        session.active_turn.lock().await.running_turn_id(),
        Some(turn_context.sub_id.as_str())
    );

    finish_gated_compaction_publication(&gate, compaction).await;
    assert_eq!(
        timeout(Duration::from_secs(2), suspension)
            .await
            .expect("suspension should finish after compaction publication")
            .expect("suspension task should not panic")
            .expect("suspension should succeed"),
        SuspendTurnOutcome::Suspended {
            turn_id: turn_context.sub_id.clone(),
        }
    );
    assert!(session.active_turn.lock().await.is_idle());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recovery_application_retains_live_recovery_until_durable_proof_returns() {
    let (mut session, _turn_context, _rx) = make_session_and_context_with_rx().await;
    let store = attach_gated_in_memory_thread_store(
        Arc::get_mut(&mut session).expect("session should be uniquely owned"),
    )
    .await;
    let identity = install_test_post_compact_recovery(session.as_ref()).await;
    let recovery_packet = crate::context::PostCompactRecoveryContext::new(
        &identity.compaction_window_id,
        &identity.boundary_item_id,
        "test recovery instructions",
        None,
    )
    .expect("recovery packet");
    session
        .state
        .lock()
        .await
        .post_compact_recovery
        .cache_packet(&identity, recovery_packet)
        .expect("cache recovery packet");
    let gate = store.gate_next_flush();
    let application = tokio::spawn({
        let session = Arc::clone(&session);
        let identity = identity.clone();
        async move {
            session
                .record_post_compact_recovery_sampling_success(&identity, "sampling-turn")
                .await
        }
    });
    timeout(Duration::from_secs(2), gate.wait_until_flushed())
        .await
        .expect("recovery application should reach the post-flush test gate");
    let durable_items = session
        .live_thread()
        .expect("test live thread")
        .load_history(/*include_archived*/ false)
        .await
        .expect("durable recovery proof should be readable after its flush")
        .items;
    assert!(durable_items.iter().any(|item| {
        matches!(
            item,
            RolloutItem::PostCompactRecoveryApplied(applied)
                if applied.compaction_window_id == identity.compaction_window_id
                    && applied.boundary_item_id == identity.boundary_item_id
                    && applied.turn_id == "sampling-turn"
        )
    }));
    assert!(
        !application.is_finished(),
        "recovery application must not clear live state before its durable proof returns"
    );
    assert_eq!(
        session
            .state
            .lock()
            .await
            .post_compact_recovery
            .pending_identity(),
        Some(&identity)
    );
    assert!(
        session.thread_settings_persistence.try_acquire().is_err(),
        "recovery application must retain the shared publication permit through its live clear"
    );

    gate.release().await;
    timeout(Duration::from_secs(2), application)
        .await
        .expect("recovery application should finish after durable proof release")
        .expect("recovery application task should not panic")
        .expect("recovery application should clear matching live state");
    assert_eq!(
        session
            .state
            .lock()
            .await
            .post_compact_recovery
            .pending_identity(),
        None
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fresh_handler_input_waits_for_completion_terminal_flush() {
    let (mut session, old_turn_context, rx) = make_session_and_context_with_rx().await;
    let store = attach_in_memory_thread_store(
        Arc::get_mut(&mut session).expect("session should be uniquely owned"),
    )
    .await;
    let (stop_entered_tx, stop_entered_rx) = async_channel::bounded(1);
    let (stop_release_tx, stop_release_rx) = async_channel::bounded(1);
    let mut builder = codex_extension_api::ExtensionRegistryBuilder::<crate::config::Config>::new();
    builder.turn_lifecycle_contributor(Arc::new(BlockingTurnStop {
        entered_tx: stop_entered_tx,
        release_rx: stop_release_rx,
    }));
    Arc::get_mut(&mut session)
        .expect("session should still be uniquely owned")
        .services
        .extensions = Arc::new(builder.build());
    let _startup_prewarm_release = install_blocked_startup_prewarm(session.as_ref()).await;

    session
        .spawn_task(Arc::clone(&old_turn_context), Vec::new(), CompletingTask)
        .await;
    timeout(Duration::from_secs(2), stop_entered_rx.recv())
        .await
        .expect("completion should enter the turn-stop hook")
        .expect("turn-stop observer should remain open");

    let fresh_turn_id = "fresh-after-completion".to_string();
    let fresh_text = "fresh input after completion transition";
    let handler = tokio::spawn({
        let session = Arc::clone(&session);
        let fresh_turn_id = fresh_turn_id.clone();
        async move {
            super::super::turn_input::handle(
                &session,
                TurnInputRequest::user_input(user_input(fresh_text)),
                TurnInputMode::StartOrSteer,
                fresh_turn_id,
            )
            .await
        }
    });
    let expected_old_input = user_input("expected old completion turn");
    let expected_old_steer = tokio::spawn({
        let session = Arc::clone(&session);
        let old_turn_id = old_turn_context.sub_id.clone();
        let expected_old_input = expected_old_input.clone();
        async move {
            let mut submitted_input = SubmittedTurnInput::UserInput {
                content: expected_old_input,
                client_id: None,
            };
            session
                .steer_submitted_input(
                    &mut submitted_input,
                    /*additional_context*/ Default::default(),
                    Some(&old_turn_id),
                    /*required_final_output_json_schema*/ None,
                    /*responsesapi_client_metadata*/ None,
                    /*incoming_root_turn_id*/ None,
                )
                .await
        }
    });
    tokio::task::yield_now().await;
    assert!(!handler.is_finished());
    assert!(!expected_old_steer.is_finished());
    assert!(
        session
            .active_turn
            .lock()
            .await
            .is_starting_or_transitioning()
    );

    stop_release_tx
        .send(())
        .await
        .expect("turn-stop hook should still be waiting");
    recv_turn_complete(&rx, &old_turn_context.sub_id).await;
    recv_turn_started(&rx, &fresh_turn_id).await;
    timeout(Duration::from_secs(2), handler)
        .await
        .expect("fresh handler should finish")
        .expect("fresh handler task should not panic")
        .expect("fresh input should submit");

    let expected_old_error = timeout(Duration::from_secs(2), expected_old_steer)
        .await
        .expect("expected-id steer should finish after completion")
        .expect("expected-id steer task should not panic")
        .expect_err("old turn id must not attach to the successor");
    match expected_old_error {
        SteerInputError::NoActiveTurn(input) => assert_eq!(input, expected_old_input),
        SteerInputError::ExpectedTurnMismatch { expected, actual } => {
            assert_eq!(expected, old_turn_context.sub_id);
            assert_eq!(actual, fresh_turn_id);
        }
        other => panic!("unexpected old-turn steer error after completion: {other:?}"),
    }
    assert!(
        store.calls().await.flush_thread >= 2,
        "old completion and terminal event must be flushed before successor startup"
    );
    assert_eq!(
        session.active_turn.lock().await.running_turn_id(),
        Some(fresh_turn_id.as_str())
    );

    session.abort_all_tasks(TurnAbortReason::Interrupted).await;
    let history = session.clone_history().await;
    assert_eq!(
        count_user_message_text(history.raw_items(), fresh_text),
        1,
        "fresh input should be recorded exactly once"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fresh_handler_input_joins_intended_replacement_after_caller_cancellation() {
    let (mut session, old_turn_context, rx) = make_session_and_context_with_rx().await;
    let store = attach_in_memory_thread_store(
        Arc::get_mut(&mut session).expect("session should be uniquely owned"),
    )
    .await;
    let _startup_prewarm_release = install_blocked_startup_prewarm(session.as_ref()).await;
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

    let replacement_turn_id = "intended-replacement".to_string();
    let replacement_context = session
        .new_turn_with_default_settings(replacement_turn_id.clone(), Default::default())
        .await;
    let replacement_caller = tokio::spawn({
        let session = Arc::clone(&session);
        let replacement_context = Arc::clone(&replacement_context);
        async move {
            session
                .spawn_task(replacement_context, Vec::new(), RegularTask::new())
                .await;
        }
    });
    timeout(Duration::from_secs(2), abort_started_rx.recv())
        .await
        .expect("replacement should enter the old abort hook")
        .expect("abort observer should remain open");
    replacement_caller.abort();
    assert!(
        replacement_caller
            .await
            .expect_err("replacement caller should be cancelled")
            .is_cancelled()
    );

    let fresh_request_id = "fresh-during-replacement".to_string();
    let fresh_text = "fresh input during replacement transition";
    let handler = tokio::spawn({
        let session = Arc::clone(&session);
        async move {
            let mut submitted_input = SubmittedTurnInput::UserInput {
                content: user_input(fresh_text),
                client_id: None,
            };
            session
                .steer_submitted_input(
                    &mut submitted_input,
                    /*additional_context*/ Default::default(),
                    /*expected_turn_id*/ None,
                    /*required_final_output_json_schema*/ None,
                    /*responsesapi_client_metadata*/ None,
                    /*incoming_root_turn_id*/ None,
                )
                .await
        }
    });
    let expected_old_input = user_input("expected old replacement turn");
    let expected_old_steer = tokio::spawn({
        let session = Arc::clone(&session);
        let old_turn_id = old_turn_context.sub_id.clone();
        let expected_old_input = expected_old_input.clone();
        async move {
            let mut submitted_input = SubmittedTurnInput::UserInput {
                content: expected_old_input,
                client_id: None,
            };
            session
                .steer_submitted_input(
                    &mut submitted_input,
                    /*additional_context*/ Default::default(),
                    Some(&old_turn_id),
                    /*required_final_output_json_schema*/ None,
                    /*responsesapi_client_metadata*/ None,
                    /*incoming_root_turn_id*/ None,
                )
                .await
        }
    });
    tokio::task::yield_now().await;
    assert!(!handler.is_finished());
    assert!(!expected_old_steer.is_finished());

    abort_release_tx
        .send(())
        .await
        .expect("old abort hook should still be waiting");
    recv_turn_aborted(&rx, &old_turn_context.sub_id, TurnAbortReason::Replaced).await;
    recv_turn_started(&rx, &replacement_turn_id).await;
    timeout(Duration::from_secs(2), handler)
        .await
        .expect("fresh handler should finish")
        .expect("fresh handler task should not panic")
        .expect("fresh input should attach to the replacement");

    let expected_old_error = timeout(Duration::from_secs(2), expected_old_steer)
        .await
        .expect("expected-id steer should finish after replacement")
        .expect("expected-id steer task should not panic")
        .expect_err("old turn id must not attach to the replacement");
    assert_eq!(
        expected_old_error,
        SteerInputError::ExpectedTurnMismatch {
            expected: old_turn_context.sub_id.clone(),
            actual: replacement_turn_id.clone(),
        }
    );
    assert_eq!(
        session.active_turn.lock().await.running_turn_id(),
        Some(replacement_turn_id.as_str())
    );
    assert!(
        store.calls().await.flush_thread >= 1,
        "old TurnAborted must be flushed before replacement startup"
    );
    assert_eq!(
        session
            .input_queue
            .get_pending_input(&session.active_turn)
            .await
            .0,
        vec![TurnInput::UserInput {
            acceptance_order: None,
            content: user_input(fresh_text),
            client_id: None,
        }]
    );
    assert!(
        !matches!(
            rx.try_recv(),
            Ok(Event {
                msg: EventMsg::TurnStarted(TurnStartedEvent { turn_id, .. }),
                ..
            }) if turn_id == fresh_request_id
        ),
        "fresh input must not create a third task"
    );

    session.abort_all_tasks(TurnAbortReason::Interrupted).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[expect(
    clippy::await_holding_invalid_type,
    reason = "the held state lock is the explicit pre-drain startup barrier under test"
)]
async fn stale_pending_work_startup_cannot_drain_successor_mailbox_input() {
    let (session, stale_context, rx) = make_session_and_context_with_rx().await;
    let _startup_prewarm_release = install_blocked_startup_prewarm(session.as_ref()).await;
    let expected_mail = InterAgentCommunication::new(
        AgentPath::root(),
        AgentPath::root(),
        Vec::new(),
        "mail owned by the successor startup".to_string(),
        /*trigger_turn*/ true,
    );
    session
        .input_queue
        .enqueue_mailbox_communication(expected_mail.clone(), Default::default())
        .await;

    let stale_claim = {
        let mut slot = session.active_turn.lock().await;
        slot.claim_start(stale_context.sub_id.clone())
            .expect("idle slot should admit the stale startup")
    };
    let state_guard = session.state.lock().await;
    let stale_startup = tokio::spawn({
        let session = Arc::clone(&session);
        let stale_context = Arc::clone(&stale_context);
        async move {
            session
                .start_claimed_regular_task_with_options(
                    stale_claim,
                    stale_context,
                    Vec::new(),
                    /*input_persisted*/ None,
                    MailboxParentProvenance::Attribute,
                )
                .await
        }
    });
    wait_for_starting_turn(session.as_ref(), &stale_context.sub_id).await;
    assert!(
        session
            .abort_turn_if_active(&stale_context.sub_id, TurnAbortReason::Replaced)
            .await,
        "targeted abort should retire the stale startup claim"
    );

    drop(state_guard);
    assert!(
        timeout(Duration::from_secs(2), stale_startup)
            .await
            .expect("stale startup should return")
            .expect("stale startup should not panic")
            .is_err(),
        "stale startup must fail before consuming mailbox input"
    );
    assert!(
        session.input_queue.has_pending_mailbox_items().await,
        "stale startup must leave successor mailbox input intact"
    );

    let successor_turn_id = "successor-after-stale-startup".to_string();
    session
        .maybe_start_turn_for_pending_work_with_sub_id(successor_turn_id.clone())
        .await;
    recv_turn_started(&rx, &successor_turn_id).await;
    assert_eq!(
        session
            .input_queue
            .get_pending_input(&session.active_turn)
            .await
            .0,
        vec![TurnInput::InterAgentCommunication(expected_mail)]
    );
    assert_eq!(
        session
            .input_queue
            .get_pending_input(&session.active_turn)
            .await
            .0,
        Vec::<TurnInput>::new(),
        "successor must receive mailbox input exactly once"
    );
    assert!(
        !session.input_queue.has_pending_mailbox_items().await,
        "successor should drain the mailbox once it owns the claim"
    );

    session.abort_all_tasks(TurnAbortReason::Interrupted).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[expect(
    clippy::await_holding_invalid_type,
    reason = "the held state lock is the explicit startup barrier under test"
)]
async fn cancelling_starting_caller_keeps_internal_owner_and_one_successor() {
    let (session, first_context, rx) = make_session_and_context_with_rx().await;
    let _startup_prewarm_release = install_blocked_startup_prewarm(session.as_ref()).await;
    let state_guard = session.state.lock().await;

    let first_turn_id = first_context.sub_id.clone();
    let starter = tokio::spawn({
        let session = Arc::clone(&session);
        let first_context = Arc::clone(&first_context);
        async move {
            session
                .spawn_task(
                    first_context,
                    vec![TurnInput::UserInput {
                        acceptance_order: None,
                        content: user_input("first starting input"),
                        client_id: None,
                    }],
                    RegularTask::new(),
                )
                .await;
        }
    });
    wait_for_starting_turn(session.as_ref(), &first_turn_id).await;

    let mut stale_input = SubmittedTurnInput::UserInput {
        content: user_input("stale expected id"),
        client_id: None,
    };
    let stale_error = session
        .steer_submitted_input(
            &mut stale_input,
            /*additional_context*/ Default::default(),
            Some("stale-turn"),
            /*required_final_output_json_schema*/ None,
            /*responsesapi_client_metadata*/ None,
            /*incoming_root_turn_id*/ None,
        )
        .await
        .expect_err("stale expected id should reject the published starter");
    assert_eq!(
        stale_error,
        SteerInputError::ExpectedTurnMismatch {
            expected: "stale-turn".to_string(),
            actual: first_turn_id.clone(),
        }
    );

    let second = tokio::spawn({
        let session = Arc::clone(&session);
        async move {
            let mut submitted_input = SubmittedTurnInput::UserInput {
                content: user_input("second waiting input"),
                client_id: None,
            };
            session
                .steer_submitted_input(
                    &mut submitted_input,
                    /*additional_context*/ Default::default(),
                    /*expected_turn_id*/ None,
                    /*required_final_output_json_schema*/ None,
                    /*responsesapi_client_metadata*/ None,
                    /*incoming_root_turn_id*/ None,
                )
                .await
        }
    });
    let third = tokio::spawn({
        let session = Arc::clone(&session);
        async move {
            let mut submitted_input = SubmittedTurnInput::UserInput {
                content: user_input("third waiting input"),
                client_id: None,
            };
            session
                .steer_submitted_input(
                    &mut submitted_input,
                    /*additional_context*/ Default::default(),
                    /*expected_turn_id*/ None,
                    /*required_final_output_json_schema*/ None,
                    /*responsesapi_client_metadata*/ None,
                    /*incoming_root_turn_id*/ None,
                )
                .await
        }
    });
    starter.abort();
    assert!(
        starter
            .await
            .expect_err("starter caller should be cancelled")
            .is_cancelled()
    );
    assert_eq!(
        session.active_turn.lock().await.starting_turn_id(),
        Some(first_turn_id.as_str())
    );

    drop(state_guard);
    recv_turn_started(&rx, &first_turn_id).await;
    assert_eq!(
        timeout(Duration::from_secs(2), second)
            .await
            .expect("second input should be released")
            .expect("second input task should not panic")
            .expect("second input should attach"),
        first_turn_id
    );
    assert_eq!(
        timeout(Duration::from_secs(2), third)
            .await
            .expect("third input should be released")
            .expect("third input task should not panic")
            .expect("third input should attach"),
        first_context.sub_id
    );
    wait_for_running_turn(session.as_ref(), &first_context.sub_id).await;
    let pending = session
        .input_queue
        .get_pending_input(&session.active_turn)
        .await
        .0;
    assert_eq!(pending.len(), 2);
    assert!(pending.contains(&TurnInput::UserInput {
        acceptance_order: None,
        content: user_input("second waiting input"),
        client_id: None,
    }));
    assert!(pending.contains(&TurnInput::UserInput {
        acceptance_order: None,
        content: user_input("third waiting input"),
        client_id: None,
    }));
    assert!(
        !matches!(
            rx.try_recv(),
            Ok(Event {
                msg: EventMsg::TurnStarted(_),
                ..
            })
        ),
        "concurrent no-id inputs must not start another turn"
    );

    session.abort_all_tasks(TurnAbortReason::Interrupted).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_interrupt_caller_does_not_abandon_transition() {
    let (session, turn_context, rx) = make_session_and_context_with_rx().await;
    let (sealed_tx, sealed_rx) = async_channel::bounded(1);
    let (abort_started_tx, abort_started_rx) = async_channel::bounded(1);
    let (abort_release_tx, abort_release_rx) = async_channel::bounded(1);
    session
        .spawn_task(
            Arc::clone(&turn_context),
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
        .expect("task should seal steer admission")
        .expect("sealed-task observer should remain open");

    let interrupt_caller = tokio::spawn({
        let session = Arc::clone(&session);
        async move {
            session.abort_all_tasks(TurnAbortReason::Interrupted).await;
        }
    });
    timeout(Duration::from_secs(2), abort_started_rx.recv())
        .await
        .expect("interrupt should enter the abort hook")
        .expect("abort observer should remain open");
    let expected_old_input = user_input("expected interrupted turn");
    let expected_old_steer = tokio::spawn({
        let session = Arc::clone(&session);
        let turn_id = turn_context.sub_id.clone();
        let expected_old_input = expected_old_input.clone();
        async move {
            let mut submitted_input = SubmittedTurnInput::UserInput {
                content: expected_old_input,
                client_id: None,
            };
            session
                .steer_submitted_input(
                    &mut submitted_input,
                    /*additional_context*/ Default::default(),
                    Some(&turn_id),
                    /*required_final_output_json_schema*/ None,
                    /*responsesapi_client_metadata*/ None,
                    /*incoming_root_turn_id*/ None,
                )
                .await
        }
    });
    interrupt_caller.abort();
    assert!(
        interrupt_caller
            .await
            .expect_err("interrupt caller should be cancelled")
            .is_cancelled()
    );

    abort_release_tx
        .send(())
        .await
        .expect("abort hook should still be waiting");
    recv_turn_aborted(&rx, &turn_context.sub_id, TurnAbortReason::Interrupted).await;
    timeout(Duration::from_secs(2), async {
        loop {
            if session.active_turn.lock().await.is_idle() {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("internally owned interrupt should reach Idle");
    assert_eq!(
        timeout(Duration::from_secs(2), expected_old_steer)
            .await
            .expect("expected-id steer should be released")
            .expect("expected-id steer task should not panic")
            .expect_err("interrupted turn should no longer be active"),
        SteerInputError::NoActiveTurn(expected_old_input)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn user_interrupt_does_not_emit_thread_idle_lifecycle() {
    let (mut session, turn_context, rx) = make_session_and_context_with_rx().await;
    let (idle_tx, idle_rx) = async_channel::bounded(1);
    let mut builder = codex_extension_api::ExtensionRegistryBuilder::<crate::config::Config>::new();
    builder.thread_lifecycle_contributor(Arc::new(ThreadIdleSignal(idle_tx)));
    Arc::get_mut(&mut session)
        .expect("session should be uniquely owned")
        .services
        .extensions = Arc::new(builder.build());

    assert!(!session.input_queue.has_trigger_turn_mailbox_items().await);
    session
        .spawn_task(
            Arc::clone(&turn_context),
            Vec::new(),
            NeverEndingTask {
                kind: TaskKind::Regular,
                listen_to_cancellation_token: true,
            },
        )
        .await;
    wait_for_running_turn(session.as_ref(), &turn_context.sub_id).await;
    assert!(!session.input_queue.has_trigger_turn_mailbox_items().await);

    session.interrupt_task().await;

    recv_turn_aborted(&rx, &turn_context.sub_id, TurnAbortReason::Interrupted).await;
    assert!(session.active_turn.lock().await.is_idle());
    assert!(!session.input_queue.has_trigger_turn_mailbox_items().await);
    assert!(
        timeout(Duration::from_millis(100), idle_rx.recv())
            .await
            .is_err(),
        "user interrupt must not emit generic thread-idle lifecycle"
    );
    assert!(
        !matches!(
            rx.try_recv(),
            Ok(Event {
                msg: EventMsg::TurnStarted(_),
                ..
            })
        ),
        "user interrupt without pending work must not start a successor"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn matching_turn_abort_does_not_emit_thread_idle_lifecycle() {
    let (mut session, turn_context, rx) = make_session_and_context_with_rx().await;
    let (idle_tx, idle_rx) = async_channel::bounded(1);
    let mut builder = codex_extension_api::ExtensionRegistryBuilder::<crate::config::Config>::new();
    builder.thread_lifecycle_contributor(Arc::new(ThreadIdleSignal(idle_tx)));
    Arc::get_mut(&mut session)
        .expect("session should be uniquely owned")
        .services
        .extensions = Arc::new(builder.build());

    assert!(!session.input_queue.has_trigger_turn_mailbox_items().await);
    session
        .spawn_task(
            Arc::clone(&turn_context),
            Vec::new(),
            NeverEndingTask {
                kind: TaskKind::Regular,
                listen_to_cancellation_token: true,
            },
        )
        .await;
    wait_for_running_turn(session.as_ref(), &turn_context.sub_id).await;
    assert!(!session.input_queue.has_trigger_turn_mailbox_items().await);

    assert!(
        session
            .abort_turn_if_active(&turn_context.sub_id, TurnAbortReason::Interrupted)
            .await
    );

    recv_turn_aborted(&rx, &turn_context.sub_id, TurnAbortReason::Interrupted).await;
    assert!(session.active_turn.lock().await.is_idle());
    assert!(!session.input_queue.has_trigger_turn_mailbox_items().await);
    assert!(
        timeout(Duration::from_millis(100), idle_rx.recv())
            .await
            .is_err(),
        "targeted interrupt must leave generic thread-idle ownership to its caller"
    );
}
