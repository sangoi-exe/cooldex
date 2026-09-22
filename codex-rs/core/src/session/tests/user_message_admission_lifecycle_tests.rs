use super::*;
use crate::user_message_admission::PendingUserMessageAdmissionState;
use crate::user_message_admission::UserMessageAdmission;
use crate::user_message_admission::UserMessageAdmissionError;

#[tokio::test]
async fn completing_turn_rejects_pending_persisted_user_message_admission() {
    let (session, turn_context, _rx) = make_session_and_context_with_rx().await;
    let (_guard, admission_rx) = session.pending_user_message_admissions.register(
        turn_context.sub_id.clone(),
        Some("client-message-1".to_string()),
        PendingUserMessageAdmissionState::Admitted(UserMessageAdmission::Started {
            turn_id: turn_context.sub_id.clone(),
        }),
    );

    session
        .spawn_task(Arc::clone(&turn_context), Vec::new(), CompletingTask)
        .await;

    let error = timeout(Duration::from_secs(2), admission_rx)
        .await
        .expect("task completion should resolve the pending admission")
        .expect("admission waiter should not drop unexpectedly")
        .expect_err("completed turn should reject unpersisted admission");
    assert!(matches!(
        error,
        UserMessageAdmissionError::TaskEndedBeforePersistence
    ));
}

#[tokio::test]
async fn aborting_turn_rejects_pending_persisted_user_message_admission() {
    let (session, turn_context, rx) = make_session_and_context_with_rx().await;
    let client_id = "client-message-1";
    let (_guard, admission_rx) = session.pending_user_message_admissions.register(
        turn_context.sub_id.clone(),
        Some(client_id.to_string()),
        PendingUserMessageAdmissionState::Admitted(UserMessageAdmission::Started {
            turn_id: turn_context.sub_id.clone(),
        }),
    );

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
    session.abort_all_tasks(TurnAbortReason::Interrupted).await;

    let error = timeout(Duration::from_secs(2), admission_rx)
        .await
        .expect("task abort should resolve the pending admission")
        .expect("admission waiter should not drop unexpectedly")
        .expect_err("aborted turn should reject unpersisted admission");
    assert!(matches!(
        error,
        UserMessageAdmissionError::TaskEndedBeforePersistence
    ));
    assert!(
        !session
            .pending_user_message_admissions
            .contains_client_id(client_id),
        "aborted admission should be removed after settling its waiter"
    );

    let successor_turn_id = "successor-after-aborted-admission".to_string();
    let successor_context = session
        .new_turn_with_default_settings(successor_turn_id.clone(), Default::default())
        .await;
    session
        .spawn_task(successor_context, Vec::new(), CompletingTask)
        .await;
    timeout(Duration::from_secs(2), async {
        loop {
            let event = rx.recv().await.expect("event channel should remain open");
            if matches!(
                event.msg,
                EventMsg::TurnComplete(TurnCompleteEvent { ref turn_id, .. })
                    if turn_id == &successor_turn_id
            ) {
                return;
            }
        }
    })
    .await
    .expect("session should accept and complete later work");
}
