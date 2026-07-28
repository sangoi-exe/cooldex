use super::*;
use crate::tasks::RegularTask;
use pretty_assertions::assert_eq;

struct BlockingTurnStop {
    entered_tx: async_channel::Sender<()>,
    release_rx: async_channel::Receiver<()>,
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

fn user_input_op(text: &str) -> Op {
    Op::UserInput {
        items: user_input(text),
        final_output_json_schema: None,
        responsesapi_client_metadata: None,
        additional_context: Default::default(),
        thread_settings: Default::default(),
    }
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

fn count_user_message_text(items: &[ResponseItem], expected_text: &str) -> usize {
    items
        .iter()
        .filter(|item| {
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
            handlers::user_input_or_turn(
                &session,
                fresh_turn_id,
                user_input_op(fresh_text),
                /*client_user_message_id*/ None,
            )
            .await;
        }
    });
    let expected_old_input = user_input("expected old completion turn");
    let expected_old_steer = tokio::spawn({
        let session = Arc::clone(&session);
        let old_turn_id = old_turn_context.sub_id.clone();
        let expected_old_input = expected_old_input.clone();
        async move {
            session
                .steer_input(
                    expected_old_input,
                    /*additional_context*/ Default::default(),
                    Some(&old_turn_id),
                    /*client_user_message_id*/ None,
                    /*responsesapi_client_metadata*/ None,
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
        .expect("fresh handler task should not panic");

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
        .new_default_turn_with_sub_id(replacement_turn_id.clone())
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
        let fresh_request_id = fresh_request_id.clone();
        async move {
            handlers::user_input_or_turn(
                &session,
                fresh_request_id,
                user_input_op(fresh_text),
                /*client_user_message_id*/ None,
            )
            .await;
        }
    });
    let expected_old_input = user_input("expected old replacement turn");
    let expected_old_steer = tokio::spawn({
        let session = Arc::clone(&session);
        let old_turn_id = old_turn_context.sub_id.clone();
        let expected_old_input = expected_old_input.clone();
        async move {
            session
                .steer_input(
                    expected_old_input,
                    /*additional_context*/ Default::default(),
                    Some(&old_turn_id),
                    /*client_user_message_id*/ None,
                    /*responsesapi_client_metadata*/ None,
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
        .expect("fresh handler task should not panic");

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
            .await,
        vec![TurnInput::UserInput {
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
    reason = "the held state lock is the explicit startup barrier under test"
)]
async fn cancelling_starting_caller_keeps_internal_owner_and_one_successor() {
    let (session, first_context, rx) = make_session_and_context_with_rx().await;
    let second_context = session
        .new_default_turn_with_sub_id("second-no-id-input".to_string())
        .await;
    let third_context = session
        .new_default_turn_with_sub_id("third-no-id-input".to_string())
        .await;
    let _startup_prewarm_release = install_blocked_startup_prewarm(session.as_ref()).await;
    let state_guard = session.state.lock().await;

    let first_turn_id = first_context.sub_id.clone();
    let starter = tokio::spawn({
        let session = Arc::clone(&session);
        let first_context = Arc::clone(&first_context);
        async move {
            session
                .route_user_input(
                    first_context,
                    user_input("first starting input"),
                    /*additional_context*/ Default::default(),
                    /*client_user_message_id*/ None,
                    /*responsesapi_client_metadata*/ None,
                )
                .await
        }
    });
    wait_for_starting_turn(session.as_ref(), &first_turn_id).await;

    let stale_error = session
        .steer_input(
            user_input("stale expected id"),
            /*additional_context*/ Default::default(),
            Some("stale-turn"),
            /*client_user_message_id*/ None,
            /*responsesapi_client_metadata*/ None,
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
            session
                .route_user_input(
                    second_context,
                    user_input("second waiting input"),
                    /*additional_context*/ Default::default(),
                    /*client_user_message_id*/ None,
                    /*responsesapi_client_metadata*/ None,
                )
                .await
        }
    });
    let third = tokio::spawn({
        let session = Arc::clone(&session);
        async move {
            session
                .route_user_input(
                    third_context,
                    user_input("third waiting input"),
                    /*additional_context*/ Default::default(),
                    /*client_user_message_id*/ None,
                    /*responsesapi_client_metadata*/ None,
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
        .await;
    assert_eq!(pending.len(), 2);
    assert!(pending.contains(&TurnInput::UserInput {
        content: user_input("second waiting input"),
        client_id: None,
    }));
    assert!(pending.contains(&TurnInput::UserInput {
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
            session
                .steer_input(
                    expected_old_input,
                    /*additional_context*/ Default::default(),
                    Some(&turn_id),
                    /*client_user_message_id*/ None,
                    /*responsesapi_client_metadata*/ None,
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
