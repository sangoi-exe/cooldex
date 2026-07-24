use super::*;
use codex_protocol::ResponseItemId;
use codex_protocol::models::ContentItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::PostCompactRecoveryAppliedItem;
use codex_protocol::protocol::PostCompactRecoveryMarker;
use codex_protocol::protocol::TurnCompleteEvent;
use pretty_assertions::assert_eq;

const WINDOW_ID: &str = "019b3f6e-7a10-7cc3-8b6e-1d09e2f7a001";
const OTHER_WINDOW_ID: &str = "019b3f6e-7a10-7cc3-8b6e-1d09e2f7a002";
const UUID_V4_WINDOW_ID: &str = "550e8400-e29b-41d4-a716-446655440000";
const BOUNDARY_ID: &str = "msg_boundary";
const TURN_ID: &str = "turn_consuming";

fn boundary_item(id: &str) -> ResponseItem {
    ResponseItem::Message {
        id: Some(ResponseItemId::from_server(id.to_string())),
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "retained historical context".to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

fn compaction(
    window_id: Option<&str>,
    marker_boundary_id: Option<&str>,
    replacement_history: Option<Vec<ResponseItem>>,
) -> RolloutItem {
    RolloutItem::Compacted(CompactedItem {
        message: "summary".to_string(),
        replacement_history,
        window_number: Some(1),
        first_window_id: window_id.map(ToString::to_string),
        previous_window_id: None,
        window_id: window_id.map(ToString::to_string),
        post_compact_recovery: marker_boundary_id.map(|boundary_item_id| {
            PostCompactRecoveryMarker {
                boundary_item_id: boundary_item_id.to_string(),
            }
        }),
    })
}

fn application(window_id: &str, boundary_item_id: &str, turn_id: &str) -> RolloutItem {
    RolloutItem::PostCompactRecoveryApplied(PostCompactRecoveryAppliedItem {
        compaction_window_id: window_id.to_string(),
        boundary_item_id: boundary_item_id.to_string(),
        turn_id: turn_id.to_string(),
    })
}

fn reconstruct(items_chronological: &[RolloutItem]) -> PostCompactRecoveryRuntimeState {
    reconstruct_with_application_turn(items_chronological, Some(TURN_ID))
}

fn reconstruct_with_application_turn(
    items_chronological: &[RolloutItem],
    application_turn_id: Option<&str>,
) -> PostCompactRecoveryRuntimeState {
    let replay = items_chronological
        .iter()
        .rev()
        .map(|item| {
            let turn_id = match item {
                RolloutItem::PostCompactRecoveryApplied(_) => {
                    application_turn_id.map(ToString::to_string)
                }
                _ => None,
            };
            PostCompactRecoveryReplayItem::new(item, turn_id)
        })
        .collect();
    reconstruct_post_compact_recovery(replay)
}

fn marked_compaction() -> RolloutItem {
    compaction(
        Some(WINDOW_ID),
        Some(BOUNDARY_ID),
        Some(vec![boundary_item(BOUNDARY_ID)]),
    )
}

#[test]
fn marked_compaction_without_application_is_pending() {
    let state = reconstruct(&[marked_compaction()]);

    assert_eq!(
        state
            .pending_identity()
            .expect("marker without application remains pending"),
        &PostCompactRecoveryIdentity {
            compaction_window_id: WINDOW_ID.to_string(),
            boundary_item_id: BOUNDARY_ID.to_string(),
        }
    );
}

#[test]
fn matching_application_consumes_without_generic_turn_complete() {
    let state = reconstruct(&[
        marked_compaction(),
        application(WINDOW_ID, BOUNDARY_ID, TURN_ID),
    ]);

    assert_eq!(state, PostCompactRecoveryRuntimeState::Absent);
}

#[test]
fn latest_compaction_owns_recovery_after_multiple_windows() {
    let older = marked_compaction();
    let latest_boundary_id = "msg_latest_boundary";
    let latest = compaction(
        Some(OTHER_WINDOW_ID),
        Some(latest_boundary_id),
        Some(vec![boundary_item(latest_boundary_id)]),
    );
    let state = reconstruct(&[
        older,
        application(WINDOW_ID, BOUNDARY_ID, TURN_ID),
        latest,
    ]);

    assert_eq!(
        state
            .pending_identity()
            .expect("latest compaction should supersede older recovery state"),
        &PostCompactRecoveryIdentity {
            compaction_window_id: OTHER_WINDOW_ID.to_string(),
            boundary_item_id: latest_boundary_id.to_string(),
        }
    );
}

#[test]
fn generic_turn_complete_without_application_does_not_consume() {
    let state = reconstruct(&[
        marked_compaction(),
        RolloutItem::EventMsg(EventMsg::TurnComplete(TurnCompleteEvent {
            turn_id: TURN_ID.to_string(),
            last_agent_message: None,
            error: None,
            started_at: None,
            completed_at: None,
            duration_ms: None,
            time_to_first_token_ms: None,
        })),
    ]);

    assert!(state.pending_identity().is_some());
}

#[test]
fn duplicate_or_mismatched_application_blocks() {
    let duplicate = reconstruct(&[
        marked_compaction(),
        application(WINDOW_ID, BOUNDARY_ID, TURN_ID),
        application(WINDOW_ID, BOUNDARY_ID, TURN_ID),
    ]);
    assert_eq!(
        duplicate.blocked_failure(),
        Some(PostCompactRecoveryFailureClass::MalformedApplicationProof)
    );

    let mismatched = reconstruct(&[
        marked_compaction(),
        application(OTHER_WINDOW_ID, BOUNDARY_ID, TURN_ID),
    ]);
    assert_eq!(
        mismatched.blocked_failure(),
        Some(PostCompactRecoveryFailureClass::BoundaryMismatch)
    );

    let wrong_containing_turn = reconstruct_with_application_turn(
        &[
            marked_compaction(),
            application(WINDOW_ID, BOUNDARY_ID, TURN_ID),
        ],
        Some("different_turn"),
    );
    assert_eq!(
        wrong_containing_turn.blocked_failure(),
        Some(PostCompactRecoveryFailureClass::MalformedApplicationProof)
    );

    let empty_turn_id = reconstruct_with_application_turn(
        &[
            marked_compaction(),
            application(WINDOW_ID, BOUNDARY_ID, ""),
        ],
        Some(""),
    );
    assert_eq!(
        empty_turn_id.blocked_failure(),
        Some(PostCompactRecoveryFailureClass::MalformedApplicationProof)
    );
}

#[test]
fn stable_pre_feature_compaction_is_repairable_but_ambiguous_legacy_is_not() {
    let repairable = reconstruct(&[compaction(
        Some(WINDOW_ID),
        None,
        Some(vec![boundary_item(BOUNDARY_ID)]),
    )]);
    assert_eq!(
        repairable
            .pending_identity()
            .expect("stable pre-feature identity should be repairable"),
        &PostCompactRecoveryIdentity {
            compaction_window_id: WINDOW_ID.to_string(),
            boundary_item_id: BOUNDARY_ID.to_string(),
        }
    );

    let missing_window = reconstruct(&[compaction(
        None,
        None,
        Some(vec![boundary_item(BOUNDARY_ID)]),
    )]);
    assert_eq!(
        missing_window.blocked_failure(),
        Some(PostCompactRecoveryFailureClass::UnsupportedLegacy)
    );

    let missing_boundary = reconstruct(&[compaction(Some(WINDOW_ID), None, Some(Vec::new()))]);
    assert_eq!(
        missing_boundary.blocked_failure(),
        Some(PostCompactRecoveryFailureClass::UnsupportedLegacy)
    );

    let duplicate_boundary = reconstruct(&[compaction(
        Some(WINDOW_ID),
        None,
        Some(vec![
            boundary_item(BOUNDARY_ID),
            boundary_item(BOUNDARY_ID),
        ]),
    )]);
    assert_eq!(
        duplicate_boundary.blocked_failure(),
        Some(PostCompactRecoveryFailureClass::UnsupportedLegacy)
    );

    let wrong_uuid_version = reconstruct(&[compaction(
        Some(UUID_V4_WINDOW_ID),
        None,
        Some(vec![boundary_item(BOUNDARY_ID)]),
    )]);
    assert_eq!(
        wrong_uuid_version.blocked_failure(),
        Some(PostCompactRecoveryFailureClass::UnsupportedLegacy)
    );
}

#[test]
fn malformed_marker_or_out_of_order_application_blocks() {
    let application_without_compaction =
        reconstruct(&[application(WINDOW_ID, BOUNDARY_ID, TURN_ID)]);
    assert_eq!(
        application_without_compaction.blocked_failure(),
        Some(PostCompactRecoveryFailureClass::MalformedApplicationProof)
    );

    let marker_without_window = reconstruct(&[compaction(
        None,
        Some(BOUNDARY_ID),
        Some(vec![boundary_item(BOUNDARY_ID)]),
    )]);
    assert_eq!(
        marker_without_window.blocked_failure(),
        Some(PostCompactRecoveryFailureClass::MalformedMarker)
    );

    let marker_without_replacement =
        reconstruct(&[compaction(Some(WINDOW_ID), Some(BOUNDARY_ID), None)]);
    assert_eq!(
        marker_without_replacement.blocked_failure(),
        Some(PostCompactRecoveryFailureClass::MalformedMarker)
    );

    let mismatched_marker = reconstruct(&[compaction(
        Some(WINDOW_ID),
        Some("different_boundary"),
        Some(vec![boundary_item(BOUNDARY_ID)]),
    )]);
    assert_eq!(
        mismatched_marker.blocked_failure(),
        Some(PostCompactRecoveryFailureClass::BoundaryMismatch)
    );

    let empty_marker = reconstruct(&[compaction(
        Some(WINDOW_ID),
        Some(""),
        Some(vec![boundary_item(BOUNDARY_ID)]),
    )]);
    assert_eq!(
        empty_marker.blocked_failure(),
        Some(PostCompactRecoveryFailureClass::MalformedMarker)
    );

    let out_of_order = reconstruct(&[
        application(WINDOW_ID, BOUNDARY_ID, TURN_ID),
        marked_compaction(),
    ]);
    assert_eq!(
        out_of_order.blocked_failure(),
        Some(PostCompactRecoveryFailureClass::MalformedApplicationProof)
    );
}
