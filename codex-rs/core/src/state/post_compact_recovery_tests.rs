use super::*;
use pretty_assertions::assert_eq;

fn identity() -> PostCompactRecoveryIdentity {
    PostCompactRecoveryIdentity {
        compaction_window_id: "019b3f6e-7a10-7cc3-8b6e-1d09e2f7a001".to_string(),
        boundary_item_id: "msg_boundary".to_string(),
    }
}

#[test]
fn non_successful_outcomes_are_not_application_candidates() {
    for outcome in [
        PostCompactRecoveryTurnOutcome::Aborted,
        PostCompactRecoveryTurnOutcome::UnexpectedTaskError,
        PostCompactRecoveryTurnOutcome::TerminalError,
    ] {
        let identity = identity();
        let mut state = PostCompactRecoveryRuntimeState::pending(identity.clone());
        state
            .record_sampling_success(&identity, "turn_with_response")
            .expect("matching response proof");

        let candidate = state
            .application_candidate("turn_with_response", outcome)
            .expect("non-successful turns should leave recovery pending");

        assert_eq!(candidate, None);
        assert_eq!(state.pending_identity(), Some(&identity));
    }
}

#[test]
fn successful_candidate_rejects_mismatched_turn_id() {
    let identity = identity();
    let mut state = PostCompactRecoveryRuntimeState::pending(identity.clone());
    state
        .record_sampling_success(&identity, "turn_with_response")
        .expect("matching response proof");

    let failure = state
        .application_candidate(
            "different_turn",
            PostCompactRecoveryTurnOutcome::Successful,
        )
        .expect_err("a different finalizing turn must not consume recovery");

    assert_eq!(failure, PostCompactRecoveryFailureClass::BoundaryMismatch);
    assert_eq!(state.pending_identity(), Some(&identity));
}

#[test]
fn post_compact_recovery_application_candidate_does_not_clear_before_durability() {
    let identity = identity();
    let mut state = PostCompactRecoveryRuntimeState::pending(identity.clone());
    state
        .record_sampling_success(&identity, "turn_with_response")
        .expect("matching response proof");

    let candidate = state
        .application_candidate(
            "turn_with_response",
            PostCompactRecoveryTurnOutcome::Successful,
        )
        .expect("matching application candidate")
        .expect("successful inference should yield a candidate");

    assert_eq!(state.pending_identity(), Some(&identity));
    state.block(PostCompactRecoveryFailureClass::CanonicalPersistenceIndeterminate);
    assert_eq!(
        state.blocked_failure(),
        Some(PostCompactRecoveryFailureClass::CanonicalPersistenceIndeterminate)
    );
    assert_eq!(candidate.compaction_window_id, identity.compaction_window_id);
    assert_eq!(candidate.boundary_item_id, identity.boundary_item_id);
}

#[test]
fn post_compact_recovery_matching_durable_application_clears_pending_state() {
    let identity = identity();
    let mut state = PostCompactRecoveryRuntimeState::pending(identity.clone());
    state
        .record_sampling_success(&identity, "turn_with_response")
        .expect("matching response proof");
    let candidate = state
        .application_candidate(
            "turn_with_response",
            PostCompactRecoveryTurnOutcome::Successful,
        )
        .expect("matching application candidate")
        .expect("successful inference should yield a candidate");

    state
        .clear_after_application(&candidate)
        .expect("durable matching application should clear recovery");

    assert_eq!(state, PostCompactRecoveryRuntimeState::Absent);
}
