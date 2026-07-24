use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::CompactedItem;
use codex_protocol::protocol::RolloutItem;
use uuid::Uuid;

use crate::state::PostCompactRecoveryFailureClass;
use crate::state::PostCompactRecoveryIdentity;
use crate::state::PostCompactRecoveryRuntimeState;

#[derive(Debug)]
pub(super) struct PostCompactRecoveryReplayItem<'a> {
    item: &'a RolloutItem,
    turn_id: Option<String>,
}

impl<'a> PostCompactRecoveryReplayItem<'a> {
    pub(super) fn new(item: &'a RolloutItem, turn_id: Option<String>) -> Self {
        Self { item, turn_id }
    }
}

pub(super) fn reconstruct_post_compact_recovery(
    mut replay: Vec<PostCompactRecoveryReplayItem<'_>>,
) -> PostCompactRecoveryRuntimeState {
    replay.reverse();
    let Some((compaction_index, compacted)) =
        replay
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, replay_item)| match replay_item.item {
                RolloutItem::Compacted(compacted) => Some((index, compacted)),
                _ => None,
            })
    else {
        return if replay.iter().any(|replay_item| {
            matches!(replay_item.item, RolloutItem::PostCompactRecoveryApplied(_))
        }) {
            PostCompactRecoveryRuntimeState::Blocked(
                PostCompactRecoveryFailureClass::MalformedApplicationProof,
            )
        } else {
            PostCompactRecoveryRuntimeState::Absent
        };
    };

    let identity = match identity_from_compaction(compacted) {
        Ok(identity) => identity,
        Err(failure) => return PostCompactRecoveryRuntimeState::Blocked(failure),
    };

    if replay[..compaction_index].iter().any(|replay_item| {
        matches!(
            replay_item.item,
            RolloutItem::PostCompactRecoveryApplied(applied)
                if applied.compaction_window_id == identity.compaction_window_id
                    && applied.boundary_item_id == identity.boundary_item_id
        )
    }) {
        return PostCompactRecoveryRuntimeState::Blocked(
            PostCompactRecoveryFailureClass::MalformedApplicationProof,
        );
    }

    let applications = replay[compaction_index + 1..]
        .iter()
        .filter_map(|replay_item| match replay_item.item {
            RolloutItem::PostCompactRecoveryApplied(applied) => {
                Some((applied, replay_item.turn_id.as_deref()))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    if applications.is_empty() {
        return PostCompactRecoveryRuntimeState::pending(identity);
    }
    let (first_application, _) = applications[0];
    for (applied, containing_turn_id) in applications {
        if applied.compaction_window_id.is_empty()
            || applied.boundary_item_id.is_empty()
            || applied.turn_id.is_empty()
            || containing_turn_id.is_none()
            || containing_turn_id != Some(applied.turn_id.as_str())
        {
            return PostCompactRecoveryRuntimeState::Blocked(
                PostCompactRecoveryFailureClass::MalformedApplicationProof,
            );
        }
        if applied.compaction_window_id != identity.compaction_window_id
            || applied.boundary_item_id != identity.boundary_item_id
        {
            return PostCompactRecoveryRuntimeState::Blocked(
                PostCompactRecoveryFailureClass::BoundaryMismatch,
            );
        }
        if applied.compaction_window_id != first_application.compaction_window_id
            || applied.boundary_item_id != first_application.boundary_item_id
            || applied.turn_id != first_application.turn_id
        {
            return PostCompactRecoveryRuntimeState::Blocked(
                PostCompactRecoveryFailureClass::MalformedApplicationProof,
            );
        }
    }
    PostCompactRecoveryRuntimeState::Absent
}

fn identity_from_compaction(
    compacted: &CompactedItem,
) -> Result<PostCompactRecoveryIdentity, PostCompactRecoveryFailureClass> {
    let marker_is_present = compacted.post_compact_recovery.is_some();
    let failure_for_missing_identity = || {
        if marker_is_present {
            PostCompactRecoveryFailureClass::MalformedMarker
        } else {
            PostCompactRecoveryFailureClass::UnsupportedLegacy
        }
    };
    let compaction_window_id = compacted
        .window_id
        .as_deref()
        .filter(|window_id| is_uuid_v7(window_id))
        .ok_or_else(failure_for_missing_identity)?;
    let replacement_history = compacted
        .replacement_history
        .as_deref()
        .ok_or_else(failure_for_missing_identity)?;
    let boundary_item_id = replacement_history
        .last()
        .and_then(ResponseItem::id)
        .map(ToString::to_string)
        .filter(|boundary_item_id| !boundary_item_id.is_empty())
        .ok_or_else(failure_for_missing_identity)?;
    let boundary_occurrences = replacement_history
        .iter()
        .filter(|item| {
            item.id()
                .is_some_and(|item_id| item_id.as_str() == boundary_item_id.as_str())
        })
        .count();
    if boundary_occurrences != 1 {
        return Err(failure_for_missing_identity());
    }

    if let Some(marker) = &compacted.post_compact_recovery {
        if marker.boundary_item_id.is_empty() {
            return Err(PostCompactRecoveryFailureClass::MalformedMarker);
        }
        if marker.boundary_item_id != boundary_item_id {
            return Err(PostCompactRecoveryFailureClass::BoundaryMismatch);
        }
    }

    Ok(PostCompactRecoveryIdentity {
        compaction_window_id: compaction_window_id.to_string(),
        boundary_item_id,
    })
}

fn is_uuid_v7(value: &str) -> bool {
    Uuid::parse_str(value)
        .ok()
        .is_some_and(|uuid| uuid.get_version_num() == 7)
}

#[cfg(test)]
#[path = "post_compact_recovery_tests.rs"]
mod tests;
