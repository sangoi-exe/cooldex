use std::collections::HashSet;

use codex_protocol::protocol::CompactedItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::RolloutItem;

// Merge-safety anchor: prompt_gc rollout classification must keep prompt-gc
// markers distinguishable from normal compaction, and prompt-gc
// replacement_history is hydratable only when its persisted Compacted item is
// paired with the surviving turn segment rather than a rolled-back turn.
pub(crate) fn is_prompt_gc_compaction_marker(compacted: &CompactedItem) -> bool {
    compacted.prompt_gc.is_some()
        || compacted.message == crate::prompt_gc_sidecar::PROMPT_GC_COMPACTION_MARKER
}

pub(crate) fn compaction_replacement_history_is_hydratable(
    rollout_items: &[RolloutItem],
    compacted_index: usize,
    compacted: &CompactedItem,
) -> bool {
    compacted.replacement_history.is_some()
        && (!is_prompt_gc_compaction_marker(compacted)
            || matches!(
                rollout_items.get(compacted_index.saturating_add(1)),
                Some(RolloutItem::TurnContext(_))
            ))
}

#[derive(Default)]
struct ActiveRollbackSegment {
    turn_id: Option<String>,
    counts_as_user_turn: bool,
    indices: Vec<usize>,
}

fn turn_ids_are_compatible(active_turn_id: Option<&str>, item_turn_id: Option<&str>) -> bool {
    active_turn_id
        .is_none_or(|turn_id| item_turn_id.is_none_or(|item_turn_id| item_turn_id == turn_id))
}

fn finalize_active_rollback_segment(
    active_segment: ActiveRollbackSegment,
    discarded_indices: &mut HashSet<usize>,
    pending_rollback_turns: &mut usize,
) {
    if *pending_rollback_turns == 0 {
        return;
    }
    if active_segment.counts_as_user_turn {
        *pending_rollback_turns -= 1;
    }
    discarded_indices.extend(active_segment.indices);
}

pub(crate) fn discarded_rollout_indices_for_rolled_back_turns(
    rollout_items: &[RolloutItem],
) -> HashSet<usize> {
    let mut discarded_indices = HashSet::new();
    let mut pending_rollback_turns = 0usize;
    let mut active_segment: Option<ActiveRollbackSegment> = None;

    for (index, item) in rollout_items.iter().enumerate().rev() {
        match item {
            RolloutItem::Compacted(_) | RolloutItem::ResponseItem(_) => {
                let active_segment =
                    active_segment.get_or_insert_with(ActiveRollbackSegment::default);
                active_segment.indices.push(index);
            }
            RolloutItem::EventMsg(EventMsg::ThreadRolledBack(rollback)) => {
                pending_rollback_turns = pending_rollback_turns
                    .saturating_add(usize::try_from(rollback.num_turns).unwrap_or(usize::MAX));
            }
            RolloutItem::EventMsg(EventMsg::ContextCompacted(_)) => {
                let active_segment =
                    active_segment.get_or_insert_with(ActiveRollbackSegment::default);
                active_segment.indices.push(index);
            }
            RolloutItem::EventMsg(EventMsg::TurnComplete(event)) => {
                let active_segment =
                    active_segment.get_or_insert_with(ActiveRollbackSegment::default);
                active_segment.indices.push(index);
                if active_segment.turn_id.is_none() {
                    active_segment.turn_id = Some(event.turn_id.clone());
                }
            }
            RolloutItem::EventMsg(EventMsg::TurnAborted(event)) => {
                let active_segment =
                    active_segment.get_or_insert_with(ActiveRollbackSegment::default);
                active_segment.indices.push(index);
                if active_segment.turn_id.is_none() {
                    active_segment.turn_id = event.turn_id.clone();
                }
            }
            RolloutItem::EventMsg(EventMsg::UserMessage(_)) => {
                let active_segment =
                    active_segment.get_or_insert_with(ActiveRollbackSegment::default);
                active_segment.indices.push(index);
                active_segment.counts_as_user_turn = true;
            }
            RolloutItem::TurnContext(ctx) => {
                let active_segment =
                    active_segment.get_or_insert_with(ActiveRollbackSegment::default);
                active_segment.indices.push(index);
                if active_segment.turn_id.is_none() {
                    active_segment.turn_id = ctx.turn_id.clone();
                }
            }
            RolloutItem::EventMsg(EventMsg::TurnStarted(event)) => {
                if active_segment.as_ref().is_some_and(|active_segment| {
                    turn_ids_are_compatible(
                        active_segment.turn_id.as_deref(),
                        Some(event.turn_id.as_str()),
                    )
                }) && let Some(active_segment) = active_segment.take()
                {
                    finalize_active_rollback_segment(
                        active_segment,
                        &mut discarded_indices,
                        &mut pending_rollback_turns,
                    );
                }
            }
            RolloutItem::EventMsg(_) | RolloutItem::SessionMeta(_) => {}
        }
    }

    if let Some(active_segment) = active_segment.take() {
        finalize_active_rollback_segment(
            active_segment,
            &mut discarded_indices,
            &mut pending_rollback_turns,
        );
    }

    discarded_indices
}
