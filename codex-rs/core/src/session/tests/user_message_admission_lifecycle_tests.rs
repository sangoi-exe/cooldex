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
