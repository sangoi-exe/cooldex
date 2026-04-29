use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::protocol::CompactedItem;
use codex_protocol::protocol::PostCompactRecoveryCompactionAnchor;
use codex_protocol::protocol::PostCompactRecoveryItem;
use codex_protocol::protocol::PostCompactRecoveryStatus;
use codex_protocol::protocol::RolloutItem;
use sha2::Digest as _;
use sha2::Sha256;

// Merge-safety anchor: recovery replay is the rollback-aware authority for
// restoring post-compact runtime packets; raw scans of `PostCompactRecovery`
// rollout items must not decide active pending recovery.
#[derive(Debug)]
pub(crate) struct PostCompactRecoveryReplay {
    pub(crate) next_sequence: u64,
    pub(crate) lifecycle: RecoveryLifecycle,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RecoveryLifecycle {
    Ready(PostCompactRecoveryItem),
    Cleared,
    Failed(PostCompactRecoveryItem),
    Started(PostCompactRecoveryItem),
    MissingForSurvivingCompaction(PostCompactRecoveryCompactionAnchor),
    NoSurvivingCompaction,
}

pub(crate) fn replay_post_compact_recovery(
    rollout_items: &[RolloutItem],
    surviving_compaction_indices: &[usize],
    surviving_post_compact_recovery_indices: &[usize],
) -> CodexResult<PostCompactRecoveryReplay> {
    let next_sequence = next_post_compact_recovery_sequence(rollout_items);
    let latest_compaction_anchor =
        latest_compaction_anchor_from_indices(rollout_items, surviving_compaction_indices)?;
    let Some(latest_compaction_anchor) = latest_compaction_anchor else {
        return Ok(PostCompactRecoveryReplay {
            next_sequence,
            lifecycle: RecoveryLifecycle::NoSurvivingCompaction,
        });
    };

    let mut lifecycle = None;
    for index in surviving_post_compact_recovery_indices {
        let Some(RolloutItem::PostCompactRecovery(item)) = rollout_items.get(*index) else {
            return Err(CodexErr::Fatal(format!(
                "post-compact recovery replay index {index} is not a recovery rollout item"
            )));
        };
        if item.compaction_anchor.as_ref() != Some(&latest_compaction_anchor) {
            continue;
        }

        lifecycle = match item.status {
            PostCompactRecoveryStatus::Ready if item.packet.is_some() => {
                Some(RecoveryLifecycle::Ready(item.clone()))
            }
            PostCompactRecoveryStatus::Ready => Some(
                RecoveryLifecycle::MissingForSurvivingCompaction(latest_compaction_anchor.clone()),
            ),
            PostCompactRecoveryStatus::Started => Some(RecoveryLifecycle::Started(item.clone())),
            PostCompactRecoveryStatus::Failed => Some(RecoveryLifecycle::Failed(item.clone())),
            PostCompactRecoveryStatus::Cleared => Some(RecoveryLifecycle::Cleared),
        };
    }

    Ok(PostCompactRecoveryReplay {
        next_sequence,
        lifecycle: lifecycle.unwrap_or(RecoveryLifecycle::MissingForSurvivingCompaction(
            latest_compaction_anchor,
        )),
    })
}

fn latest_compaction_anchor_from_indices(
    rollout_items: &[RolloutItem],
    surviving_compaction_indices: &[usize],
) -> CodexResult<Option<PostCompactRecoveryCompactionAnchor>> {
    for index in surviving_compaction_indices.iter().rev() {
        if let Some(RolloutItem::Compacted(compacted)) = rollout_items.get(*index) {
            return Ok(Some(compaction_anchor(*index, compacted)?));
        }
    }
    Ok(None)
}

pub(crate) fn compaction_anchor_at_index(
    rollout_items: &[RolloutItem],
    index: usize,
) -> CodexResult<PostCompactRecoveryCompactionAnchor> {
    let Some(RolloutItem::Compacted(compacted)) = rollout_items.get(index) else {
        return Err(CodexErr::Fatal(format!(
            "post-compact recovery boundary index {index} is not a compacted rollout item"
        )));
    };
    compaction_anchor(index, compacted)
}

pub(crate) fn compaction_anchor(
    rollout_index: usize,
    compacted: &CompactedItem,
) -> CodexResult<PostCompactRecoveryCompactionAnchor> {
    let serialized = serde_json::to_vec(compacted)?;
    let digest = Sha256::digest(serialized);
    Ok(PostCompactRecoveryCompactionAnchor {
        rollout_index,
        digest: format!("sha256:{digest:x}"),
    })
}

fn next_post_compact_recovery_sequence(rollout_items: &[RolloutItem]) -> u64 {
    let mut next_sequence = 0_u64;
    for item in rollout_items {
        let RolloutItem::PostCompactRecovery(item) = item else {
            continue;
        };
        if let Some(sequence) = recovery_sequence_from_id(&item.recovery_id) {
            next_sequence = next_sequence.max(sequence.saturating_add(1));
        }
    }
    next_sequence
}

fn recovery_sequence_from_id(recovery_id: &str) -> Option<u64> {
    let (_prefix, sequence) = recovery_id.rsplit_once(":recovery-")?;
    sequence.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compacted_item(message: &str) -> CompactedItem {
        CompactedItem {
            message: message.to_string(),
            replacement_history: None,
            prompt_gc: None,
        }
    }

    fn recovery_item(
        status: PostCompactRecoveryStatus,
        compaction_anchor: Option<PostCompactRecoveryCompactionAnchor>,
        packet: Option<&str>,
    ) -> PostCompactRecoveryItem {
        PostCompactRecoveryItem {
            recovery_id: "turn:mid_turn:recovery-0".to_string(),
            turn_id: "turn".to_string(),
            status,
            phase: "mid_turn".to_string(),
            reason: "context_limit".to_string(),
            implementation: "test".to_string(),
            compaction_anchor,
            latest_compacted_index: Some(0),
            last_boundary_kind: Some("replacement_history_compacted".to_string()),
            created_at_unix_secs: Some(1),
            packet: packet.map(ToString::to_string),
            failure: None,
        }
    }

    #[test]
    fn replay_restores_ready_only_when_anchor_survives() {
        let compacted = compacted_item("summary");
        let anchor = compaction_anchor(/*rollout_index*/ 0, &compacted).expect("anchor");
        let item = recovery_item(
            PostCompactRecoveryStatus::Ready,
            Some(anchor),
            Some("packet"),
        );
        let rollout_items = vec![
            RolloutItem::Compacted(compacted),
            RolloutItem::PostCompactRecovery(item.clone()),
        ];

        let replay =
            replay_post_compact_recovery(&rollout_items, &[0], &[1]).expect("recovery replay");

        assert_eq!(replay.lifecycle, RecoveryLifecycle::Ready(item));
    }

    #[test]
    fn replay_treats_unanchored_ready_as_missing_for_surviving_compaction() {
        let compacted = compacted_item("summary");
        let anchor = compaction_anchor(/*rollout_index*/ 0, &compacted).expect("anchor");
        let rollout_items = vec![
            RolloutItem::Compacted(compacted),
            RolloutItem::PostCompactRecovery(recovery_item(
                PostCompactRecoveryStatus::Ready,
                None,
                Some("stale packet"),
            )),
        ];

        let replay =
            replay_post_compact_recovery(&rollout_items, &[0], &[1]).expect("recovery replay");

        assert_eq!(
            replay.lifecycle,
            RecoveryLifecycle::MissingForSurvivingCompaction(anchor)
        );
    }

    #[test]
    fn replay_drops_recovery_when_compaction_does_not_survive() {
        let compacted = compacted_item("summary");
        let anchor = compaction_anchor(/*rollout_index*/ 0, &compacted).expect("anchor");
        let rollout_items = vec![
            RolloutItem::Compacted(compacted),
            RolloutItem::PostCompactRecovery(recovery_item(
                PostCompactRecoveryStatus::Ready,
                Some(anchor),
                Some("stale packet"),
            )),
        ];

        let replay =
            replay_post_compact_recovery(&rollout_items, &[], &[1]).expect("recovery replay");

        assert_eq!(replay.lifecycle, RecoveryLifecycle::NoSurvivingCompaction);
    }

    #[test]
    fn replay_cleared_suppresses_surviving_compaction_recovery() {
        let compacted = compacted_item("summary");
        let anchor = compaction_anchor(/*rollout_index*/ 0, &compacted).expect("anchor");
        let rollout_items = vec![
            RolloutItem::Compacted(compacted),
            RolloutItem::PostCompactRecovery(recovery_item(
                PostCompactRecoveryStatus::Ready,
                Some(anchor.clone()),
                Some("packet"),
            )),
            RolloutItem::PostCompactRecovery(recovery_item(
                PostCompactRecoveryStatus::Cleared,
                Some(anchor),
                None,
            )),
        ];

        let replay =
            replay_post_compact_recovery(&rollout_items, &[0], &[1, 2]).expect("recovery replay");

        assert_eq!(replay.lifecycle, RecoveryLifecycle::Cleared);
    }

    #[test]
    fn replay_started_and_failed_are_explicit_lifecycle_states() {
        let compacted = compacted_item("summary");
        let anchor = compaction_anchor(/*rollout_index*/ 0, &compacted).expect("anchor");
        let started = recovery_item(
            PostCompactRecoveryStatus::Started,
            Some(anchor.clone()),
            None,
        );
        let mut failed = recovery_item(PostCompactRecoveryStatus::Failed, Some(anchor), None);
        failed.failure = Some("boom".to_string());
        let rollout_items = vec![
            RolloutItem::Compacted(compacted),
            RolloutItem::PostCompactRecovery(started),
            RolloutItem::PostCompactRecovery(failed.clone()),
        ];

        let replay =
            replay_post_compact_recovery(&rollout_items, &[0], &[1, 2]).expect("recovery replay");

        assert_eq!(replay.lifecycle, RecoveryLifecycle::Failed(failed));
    }

    #[test]
    fn replay_ignores_rolled_back_lifecycle_entries() {
        for rolled_back_status in [
            PostCompactRecoveryStatus::Ready,
            PostCompactRecoveryStatus::Started,
            PostCompactRecoveryStatus::Failed,
            PostCompactRecoveryStatus::Cleared,
        ] {
            let compacted = compacted_item("summary");
            let anchor = compaction_anchor(/*rollout_index*/ 0, &compacted).expect("anchor");
            let ready = recovery_item(
                PostCompactRecoveryStatus::Ready,
                Some(anchor.clone()),
                Some("packet"),
            );
            let stale_item = recovery_item(rolled_back_status, Some(anchor), Some("stale packet"));
            let rollout_items = vec![
                RolloutItem::Compacted(compacted),
                RolloutItem::PostCompactRecovery(ready.clone()),
                RolloutItem::PostCompactRecovery(stale_item),
            ];

            let replay =
                replay_post_compact_recovery(&rollout_items, &[0], &[1]).expect("recovery replay");

            assert_eq!(replay.lifecycle, RecoveryLifecycle::Ready(ready));
        }
    }
}
