use super::*;
use crate::context::PostCompactRecoveryContext;
use codex_history::PostCompactRecoveryPayloadKind;
use pretty_assertions::assert_eq;

fn identity() -> PostCompactRecoveryIdentity {
    PostCompactRecoveryIdentity {
        compaction_window_id: "019b3f6e-7a10-7cc3-8b6e-1d09e2f7a001".to_string(),
        boundary_item_id: "msg_boundary".to_string(),
    }
}

#[test]
fn sampling_success_builds_application_for_the_exact_cached_packet_without_clearing_pending_state()
{
    let identity = identity();
    for (handoff, payload_kind) in [
        (
            Some("operation-local handoff"),
            PostCompactRecoveryPayloadKind::HandoffAndRecovery,
        ),
        (None, PostCompactRecoveryPayloadKind::RecoveryOnly),
    ] {
        let packet = PostCompactRecoveryContext::new(
            &identity.compaction_window_id,
            &identity.boundary_item_id,
            "fixed boundary",
            handoff,
        )
        .expect("recovery packet");
        let state = PostCompactRecoveryRuntimeState::pending_with_packet(identity.clone(), packet);

        let application = state
            .application_for_sampling_success(&identity, "turn_with_response")
            .expect("matching sampling success");

        assert_eq!(state.pending_identity(), Some(&identity));
        assert_eq!(
            application,
            PostCompactRecoveryAppliedItem {
                compaction_window_id: identity.compaction_window_id.clone(),
                boundary_item_id: identity.boundary_item_id.clone(),
                turn_id: "turn_with_response".to_string(),
                payload_kind,
            }
        );
    }
}

#[test]
fn sampling_success_rejects_mismatched_identity_or_empty_turn() {
    let identity = identity();
    let packet = PostCompactRecoveryContext::new(
        &identity.compaction_window_id,
        &identity.boundary_item_id,
        "fixed boundary",
        None,
    )
    .expect("recovery packet");
    let state = PostCompactRecoveryRuntimeState::pending_with_packet(identity.clone(), packet);
    let different_identity = PostCompactRecoveryIdentity {
        compaction_window_id: identity.compaction_window_id.clone(),
        boundary_item_id: "different-boundary".to_string(),
    };

    assert_eq!(
        state
            .application_for_sampling_success(&different_identity, "turn_with_response")
            .expect_err("mismatched sampling identity must fail"),
        PostCompactRecoveryFailureClass::BoundaryMismatch
    );
    assert_eq!(
        state
            .application_for_sampling_success(&identity, "")
            .expect_err("empty sampling turn must fail"),
        PostCompactRecoveryFailureClass::BoundaryMismatch
    );
}

#[test]
fn matching_durable_application_clears_pending_state() {
    let identity = identity();
    let packet = PostCompactRecoveryContext::new(
        &identity.compaction_window_id,
        &identity.boundary_item_id,
        "fixed boundary",
        None,
    )
    .expect("recovery packet");
    let mut state = PostCompactRecoveryRuntimeState::pending_with_packet(identity.clone(), packet);
    let application = state
        .application_for_sampling_success(&identity, "turn_with_response")
        .expect("matching sampling success");

    state
        .clear_after_application(&application)
        .expect("durable matching application should clear recovery");

    assert_eq!(state, PostCompactRecoveryRuntimeState::Absent);
}

#[test]
fn pending_packet_snapshot_clones_only_an_already_cached_packet() {
    // Merge-safety anchor: snapshot reads must not materialize recovery from recall or mutate the
    // pending state while a later compaction prepares its own prompt source.
    let identity = identity();
    let mut state = PostCompactRecoveryRuntimeState::pending(identity.clone());

    assert_eq!(
        state
            .pending_packet_snapshot()
            .expect("pending state should be readable"),
        None
    );

    let packet = PostCompactRecoveryContext::new(
        &identity.compaction_window_id,
        &identity.boundary_item_id,
        "fixed boundary",
        None,
    )
    .expect("recovery packet");
    state
        .cache_packet(&identity, packet.clone())
        .expect("cache packet");

    assert_eq!(
        state
            .pending_packet_snapshot()
            .expect("cached packet snapshot"),
        Some((identity.clone(), packet.clone()))
    );
    assert_eq!(
        state.packet(&identity).expect("matching packet read"),
        Some(packet)
    );
}

#[test]
fn pending_with_packet_publishes_one_immutable_handoff_recovery_packet() {
    let identity = identity();
    let packet = PostCompactRecoveryContext::new(
        &identity.compaction_window_id,
        &identity.boundary_item_id,
        "fixed boundary",
        Some("operation-local handoff"),
    )
    .expect("recovery packet");
    let state =
        PostCompactRecoveryRuntimeState::pending_with_packet(identity.clone(), packet.clone());

    assert_eq!(state.pending_identity(), Some(&identity));
    assert_eq!(
        state
            .pending_packet_snapshot()
            .expect("prepared packet should be readable without mutation"),
        Some((identity.clone(), packet.clone()))
    );
    assert_eq!(
        state.packet(&identity).expect("matching packet read"),
        Some(packet)
    );
}
