use codex_protocol::protocol::CompactedItem;
use codex_protocol::protocol::RolloutItem;

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
