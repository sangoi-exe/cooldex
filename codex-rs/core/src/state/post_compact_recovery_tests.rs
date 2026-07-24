use super::*;
use pretty_assertions::assert_eq;

fn identity() -> PostCompactRecoveryIdentity {
    PostCompactRecoveryIdentity {
        compaction_window_id: "019b3f6e-7a10-7cc3-8b6e-1d09e2f7a001".to_string(),
        boundary_item_id: "msg_boundary".to_string(),
    }
}

#[test]
fn sampling_success_builds_application_without_clearing_pending_state() {
    let identity = identity();
    let state = PostCompactRecoveryRuntimeState::pending(identity.clone());

    let application = state
        .application_for_sampling_success(&identity, "turn_with_response")
        .expect("matching sampling success");

    assert_eq!(state.pending_identity(), Some(&identity));
    assert_eq!(
        application,
        PostCompactRecoveryAppliedItem {
            compaction_window_id: identity.compaction_window_id,
            boundary_item_id: identity.boundary_item_id,
            turn_id: "turn_with_response".to_string(),
        }
    );
}

#[test]
fn sampling_success_rejects_mismatched_identity_or_empty_turn() {
    let identity = identity();
    let state = PostCompactRecoveryRuntimeState::pending(identity.clone());
    let different_identity = PostCompactRecoveryIdentity {
        compaction_window_id: identity.compaction_window_id.clone(),
        boundary_item_id: "different-boundary".to_string(),
    };

    for result in [
        state.application_for_sampling_success(&different_identity, "turn_with_response"),
        state.application_for_sampling_success(&identity, ""),
    ] {
        assert_eq!(
            result.expect_err("invalid sampling identity must fail"),
            PostCompactRecoveryFailureClass::BoundaryMismatch
        );
    }
}

#[test]
fn matching_durable_application_clears_pending_state() {
    let identity = identity();
    let mut state = PostCompactRecoveryRuntimeState::pending(identity.clone());
    let application = state
        .application_for_sampling_success(&identity, "turn_with_response")
        .expect("matching sampling success");

    state
        .clear_after_application(&application)
        .expect("durable matching application should clear recovery");

    assert_eq!(state, PostCompactRecoveryRuntimeState::Absent);
}
