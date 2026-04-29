use super::*;
use crate::context_manager::is_user_turn_boundary;
use crate::prompt_gc_rollout::compaction_replacement_history_is_hydratable;
use crate::prompt_gc_rollout::is_private_prompt_gc_compaction_marker;
use std::collections::HashSet;

// Return value of `Session::reconstruct_history_from_rollout`, bundling the rebuilt history with
// the resume/fork hydration metadata derived from the same replay.
#[derive(Debug)]
pub(super) struct RolloutReconstruction {
    pub(super) history: Vec<ResponseItem>,
    pub(super) previous_turn_settings: Option<PreviousTurnSettings>,
    pub(super) reference_context_item: Option<TurnContextItem>,
    pub(super) surviving_compaction_indices: Vec<usize>,
    pub(super) surviving_post_compact_recovery_indices: Vec<usize>,
}

#[derive(Debug, Default)]
enum TurnReferenceContextItem {
    /// No `TurnContextItem` has been seen for this replay span yet.
    ///
    /// This differs from `Cleared`: `NeverSet` means there is no evidence this turn ever
    /// established a baseline, while `Cleared` means a baseline existed and a later compaction
    /// invalidated it. Only the latter must emit an explicit clearing segment for resume/fork
    /// hydration.
    #[default]
    NeverSet,
    /// A previously established baseline was invalidated by later compaction.
    Cleared,
    /// The latest baseline established by this replay span.
    Latest(Box<TurnContextItem>),
}

#[derive(Debug, Default)]
struct ActiveReplaySegment<'a> {
    turn_id: Option<String>,
    counts_as_user_turn: bool,
    previous_turn_settings: Option<PreviousTurnSettings>,
    reference_context_item: TurnReferenceContextItem,
    base_replacement_history: Option<&'a [ResponseItem]>,
    rollout_suffix_start_index: Option<usize>,
    compaction_indices: Vec<usize>,
    post_compact_recovery_indices: Vec<usize>,
}

#[derive(Debug, Default)]
struct ReverseReplayState<'a> {
    base_replacement_history: Option<&'a [ResponseItem]>,
    rollout_suffix_start_index: usize,
    discarded_compaction_indices: HashSet<usize>,
    surviving_post_compact_recovery_indices: Vec<usize>,
    previous_turn_settings: Option<PreviousTurnSettings>,
    reference_context_item: TurnReferenceContextItem,
    pending_rollback_turns: usize,
}

fn turn_ids_are_compatible(active_turn_id: Option<&str>, item_turn_id: Option<&str>) -> bool {
    active_turn_id
        .is_none_or(|turn_id| item_turn_id.is_none_or(|item_turn_id| item_turn_id == turn_id))
}

fn finalize_active_segment<'a>(
    active_segment: ActiveReplaySegment<'a>,
    replay_state: &mut ReverseReplayState<'a>,
) {
    // Thread rollback drops the newest surviving real user-message boundaries. In replay, that
    // means skipping the next finalized segments that contain a non-contextual
    // `EventMsg::UserMessage`.
    if replay_state.pending_rollback_turns > 0 {
        if active_segment.counts_as_user_turn {
            replay_state.pending_rollback_turns -= 1;
        }
        replay_state
            .discarded_compaction_indices
            .extend(active_segment.compaction_indices);
        return;
    }

    replay_state
        .surviving_post_compact_recovery_indices
        .extend(active_segment.post_compact_recovery_indices);

    // A surviving replacement-history checkpoint is a complete history base. Once we
    // know the newest surviving one, older rollout items do not affect rebuilt history.
    if replay_state.base_replacement_history.is_none()
        && let Some(segment_base_replacement_history) = active_segment.base_replacement_history
    {
        replay_state.base_replacement_history = Some(segment_base_replacement_history);
        if let Some(segment_rollout_suffix_start_index) = active_segment.rollout_suffix_start_index
        {
            replay_state.rollout_suffix_start_index = segment_rollout_suffix_start_index;
        }
    }

    // `previous_turn_settings` come from the newest surviving user turn that established them.
    if replay_state.previous_turn_settings.is_none() && active_segment.counts_as_user_turn {
        replay_state.previous_turn_settings = active_segment.previous_turn_settings;
    }

    // `reference_context_item` comes from the newest surviving user turn baseline, or
    // from a surviving compaction that explicitly cleared that baseline.
    if matches!(
        replay_state.reference_context_item,
        TurnReferenceContextItem::NeverSet
    ) && (active_segment.counts_as_user_turn
        || matches!(
            active_segment.reference_context_item,
            TurnReferenceContextItem::Cleared
        ))
    {
        replay_state.reference_context_item = active_segment.reference_context_item;
    }
}

impl Session {
    pub(super) async fn reconstruct_history_from_rollout(
        &self,
        turn_context: &TurnContext,
        rollout_items: &[RolloutItem],
    ) -> RolloutReconstruction {
        // Replay metadata should already match the shape of the future lazy reverse loader, even
        // while history materialization still uses an eager bridge. Scan newest-to-oldest,
        // stopping once a surviving replacement-history checkpoint and the required resume metadata
        // are both known; then replay only the buffered surviving tail forward to preserve exact
        // history semantics.
        // Rollback is "drop the newest N user turns". While scanning in reverse, that becomes
        // "skip the next N user-turn segments we finalize".
        let mut replay_state = ReverseReplayState::default();
        // Borrowed suffix of rollout items newer than the newest surviving replacement-history
        // checkpoint. If no such checkpoint exists, this remains the full rollout.
        // Reverse replay accumulates rollout items into the newest in-progress turn segment until
        // we hit its matching `TurnStarted`, at which point the segment can be finalized.
        let mut active_segment: Option<ActiveReplaySegment<'_>> = None;

        for (index, item) in rollout_items.iter().enumerate().rev() {
            match item {
                RolloutItem::Compacted(compacted) => {
                    if is_private_prompt_gc_compaction_marker(compacted)
                        && compacted.replacement_history.is_none()
                    {
                        continue;
                    }
                    let active_segment =
                        active_segment.get_or_insert_with(ActiveReplaySegment::default);
                    active_segment.compaction_indices.push(index);
                    // Looking backward, compaction clears any older baseline unless a newer
                    // `TurnContextItem` in this same segment has already re-established it.
                    if matches!(
                        active_segment.reference_context_item,
                        TurnReferenceContextItem::NeverSet
                    ) {
                        active_segment.reference_context_item = TurnReferenceContextItem::Cleared;
                    }
                    if active_segment.base_replacement_history.is_none()
                        && let Some(replacement_history) = &compacted.replacement_history
                        && compaction_replacement_history_is_hydratable(
                            rollout_items,
                            index,
                            compacted,
                        )
                    {
                        // Merge-safety anchor: prompt_gc writes a Compacted item
                        // before its matching TurnContext. Reverse replay must
                        // only accept that replacement_history after the
                        // persisted TurnContext pair exists; otherwise a
                        // crash-truncated prompt_gc pair would rewrite history
                        // without the resume metadata that anchors it.
                        active_segment.base_replacement_history = Some(replacement_history);
                        active_segment.rollout_suffix_start_index = Some(index.saturating_add(1));
                    }
                }
                RolloutItem::EventMsg(EventMsg::ThreadRolledBack(rollback)) => {
                    if let Some(active_segment) = active_segment.take() {
                        finalize_active_segment(active_segment, &mut replay_state);
                    }
                    replay_state.pending_rollback_turns = replay_state
                        .pending_rollback_turns
                        .saturating_add(usize::try_from(rollback.num_turns).unwrap_or(usize::MAX));
                }
                RolloutItem::EventMsg(EventMsg::TurnComplete(event)) => {
                    let active_segment =
                        active_segment.get_or_insert_with(ActiveReplaySegment::default);
                    // Reverse replay often sees `TurnComplete` before any turn-scoped metadata.
                    // Capture the turn id early so later `TurnContext` / abort items can match it.
                    if active_segment.turn_id.is_none() {
                        active_segment.turn_id = Some(event.turn_id.clone());
                    }
                }
                RolloutItem::EventMsg(EventMsg::TurnAborted(event)) => {
                    if let Some(active_segment) = active_segment.as_mut() {
                        if active_segment.turn_id.is_none()
                            && let Some(turn_id) = &event.turn_id
                        {
                            active_segment.turn_id = Some(turn_id.clone());
                        }
                    } else if let Some(turn_id) = &event.turn_id {
                        active_segment = Some(ActiveReplaySegment {
                            turn_id: Some(turn_id.clone()),
                            ..Default::default()
                        });
                    }
                }
                RolloutItem::EventMsg(EventMsg::UserMessage(_)) => {
                    let active_segment =
                        active_segment.get_or_insert_with(ActiveReplaySegment::default);
                    active_segment.counts_as_user_turn = true;
                }
                RolloutItem::TurnContext(ctx) => {
                    let active_segment =
                        active_segment.get_or_insert_with(ActiveReplaySegment::default);
                    // `TurnContextItem` can attach metadata to an existing segment, but only a
                    // real `UserMessage` event should make the segment count as a user turn.
                    if active_segment.turn_id.is_none() {
                        active_segment.turn_id = ctx.turn_id.clone();
                    }
                    if turn_ids_are_compatible(
                        active_segment.turn_id.as_deref(),
                        ctx.turn_id.as_deref(),
                    ) {
                        active_segment.previous_turn_settings = Some(PreviousTurnSettings {
                            model: ctx.model.clone(),
                            realtime_active: ctx.realtime_active,
                        });
                        if matches!(
                            active_segment.reference_context_item,
                            TurnReferenceContextItem::NeverSet
                        ) {
                            active_segment.reference_context_item =
                                TurnReferenceContextItem::Latest(Box::new(ctx.clone()));
                        }
                    }
                }
                RolloutItem::EventMsg(EventMsg::TurnStarted(event)) => {
                    // `TurnStarted` is the oldest boundary of the active reverse segment.
                    if active_segment.as_ref().is_some_and(|active_segment| {
                        turn_ids_are_compatible(
                            active_segment.turn_id.as_deref(),
                            Some(event.turn_id.as_str()),
                        )
                    }) && let Some(active_segment) = active_segment.take()
                    {
                        finalize_active_segment(active_segment, &mut replay_state);
                    }
                }
                RolloutItem::ResponseItem(response_item) => {
                    let active_segment =
                        active_segment.get_or_insert_with(ActiveReplaySegment::default);
                    active_segment.counts_as_user_turn |= is_user_turn_boundary(response_item);
                }
                RolloutItem::PostCompactRecovery(_) => {
                    let active_segment =
                        active_segment.get_or_insert_with(ActiveReplaySegment::default);
                    active_segment.post_compact_recovery_indices.push(index);
                }
                RolloutItem::EventMsg(_)
                | RolloutItem::SessionMeta(_)
                | RolloutItem::SessionState(_) => {}
            }

            if replay_state.base_replacement_history.is_some()
                && replay_state.previous_turn_settings.is_some()
                && !matches!(
                    replay_state.reference_context_item,
                    TurnReferenceContextItem::NeverSet
                )
            {
                // At this point we have both eager resume metadata values and the replacement-
                // history base for the surviving tail, so older rollout items cannot affect this
                // result.
                break;
            }
        }

        if let Some(active_segment) = active_segment.take() {
            finalize_active_segment(active_segment, &mut replay_state);
        }
        replay_state
            .surviving_post_compact_recovery_indices
            .sort_unstable();

        let mut history = ContextManager::new();
        let mut saw_legacy_compaction_without_replacement_history = false;
        let mut surviving_compaction_indices = Vec::new();
        if let Some(base_replacement_history) = replay_state.base_replacement_history {
            history.replace(base_replacement_history.to_vec());
            let base_compaction_index = replay_state.rollout_suffix_start_index.saturating_sub(1);
            if let Some(RolloutItem::Compacted(compacted)) =
                rollout_items.get(base_compaction_index)
                && !replay_state
                    .discarded_compaction_indices
                    .contains(&base_compaction_index)
                && !is_private_prompt_gc_compaction_marker(compacted)
            {
                surviving_compaction_indices.push(base_compaction_index);
            }
        }
        // Materialize exact history semantics from the replay-derived suffix. The eventual lazy
        // design should keep this same replay shape, but drive it from a resumable reverse source
        // instead of an eagerly loaded `&[RolloutItem]`.
        for (index, item) in rollout_items
            .iter()
            .enumerate()
            .skip(replay_state.rollout_suffix_start_index)
        {
            match item {
                RolloutItem::ResponseItem(response_item) => {
                    history.record_items(
                        std::iter::once(response_item),
                        turn_context.truncation_policy,
                    );
                }
                RolloutItem::Compacted(compacted) => {
                    if replay_state.discarded_compaction_indices.contains(&index) {
                        continue;
                    }
                    if is_private_prompt_gc_compaction_marker(compacted) {
                        continue;
                    }
                    surviving_compaction_indices.push(index);
                    if let Some(replacement_history) = &compacted.replacement_history {
                        // This should actually never happen, because the reverse loop above (to build rollout_suffix)
                        // should stop before any compaction that has Some replacement_history
                        history.replace(replacement_history.clone());
                    } else {
                        saw_legacy_compaction_without_replacement_history = true;
                        // Legacy rollouts without `replacement_history` should rebuild the
                        // historical TurnContext at the correct insertion point from persisted
                        // `TurnContextItem`s. These are rare enough that we currently just clear
                        // `reference_context_item`, reinject canonical context at the end of the
                        // resumed conversation, and accept the temporary out-of-distribution
                        // prompt shape.
                        // TODO(ccunningham): if we drop support for None replacement_history compaction items,
                        // we can get rid of this second loop entirely and just build `history` directly in the first loop.
                        let user_messages = collect_user_messages(history.raw_items());
                        let rebuilt = compact::build_compacted_history(
                            Vec::new(),
                            &user_messages,
                            &compacted.message,
                        );
                        history.replace(rebuilt);
                    }
                }
                RolloutItem::EventMsg(EventMsg::ThreadRolledBack(rollback)) => {
                    history.drop_last_n_user_turns(rollback.num_turns);
                }
                RolloutItem::EventMsg(_)
                | RolloutItem::PostCompactRecovery(_)
                | RolloutItem::SessionState(_)
                | RolloutItem::TurnContext(_)
                | RolloutItem::SessionMeta(_) => {}
            }
        }

        let reference_context_item = match replay_state.reference_context_item {
            TurnReferenceContextItem::NeverSet | TurnReferenceContextItem::Cleared => None,
            TurnReferenceContextItem::Latest(turn_reference_context_item) => {
                Some(*turn_reference_context_item)
            }
        };
        let reference_context_item = if saw_legacy_compaction_without_replacement_history {
            None
        } else {
            reference_context_item
        };

        RolloutReconstruction {
            history: history.raw_items().to_vec(),
            previous_turn_settings: replay_state.previous_turn_settings,
            reference_context_item,
            surviving_compaction_indices,
            surviving_post_compact_recovery_indices: replay_state
                .surviving_post_compact_recovery_indices,
        }
    }
}
