use std::collections::HashMap;
use std::collections::HashSet;

use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::CompactedItem;
use codex_protocol::protocol::RolloutItem;
use codex_thread_store::StoredRolloutTail;
use codex_utils_output_truncation::approx_token_count;
use serde::Serialize;

use super::Session;
use super::TurnContext;
use crate::context::RecallContext;

pub(crate) const RECALL_SOURCE_MAX_BYTES: u64 = 16 * 1024 * 1024;
pub(crate) const RECALL_SOURCE_MAX_RECORDS: usize = 8_192;
const RECALL_RESULT_MAX_BYTES: usize = 32 * 1024;
const RECALL_RESULT_MAX_GROUPS: usize = 64;
const RECALL_RESULT_MAX_TOKENS: usize = 8_000;

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum RecallAvailability {
    Available,
    WorkLimit,
    NoCompaction,
    UnsupportedLegacy,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum RecallBoundaryKind {
    ReplacementHistory,
    Legacy,
}

#[derive(Clone, Serialize)]
struct RecallBoundary {
    rollout_item_index: usize,
    kind: RecallBoundaryKind,
    window_number: Option<u64>,
    first_window_id: Option<String>,
    previous_window_id: Option<String>,
    window_id: Option<String>,
}

#[derive(Clone, Copy, Serialize)]
struct RecallSourceRead {
    reached_start: bool,
    bytes_read: u64,
    records_read: usize,
    segments_read: usize,
}

#[derive(Serialize)]
struct RecallGroup {
    items: Vec<ResponseItem>,
}

#[derive(Serialize)]
struct RecallDocument<'a> {
    thread_id: &'a str,
    availability: RecallAvailability,
    boundary: Option<&'a RecallBoundary>,
    source: RecallSourceRead,
    truncated: bool,
    omitted_groups: usize,
    groups: &'a [RecallGroup],
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum PairKey {
    Function(String),
    Custom(String),
    ToolSearch(String),
}

impl Session {
    pub(crate) async fn build_recall_context(
        &self,
        turn_context: &TurnContext,
        tail: StoredRolloutTail,
    ) -> anyhow::Result<RecallContext> {
        if tail.thread_id != self.thread_id {
            anyhow::bail!(
                "bounded rollout tail belongs to {}, not {}",
                tail.thread_id,
                self.thread_id
            );
        }
        let thread_id = self.thread_id.to_string();
        let source = RecallSourceRead {
            reached_start: tail.reached_start,
            bytes_read: tail.bytes_read,
            records_read: tail.records_read,
            segments_read: tail.segments_read,
        };
        if !tail.reached_start {
            return unavailable_context(&thread_id, RecallAvailability::WorkLimit, source);
        }

        let reconstruction = self
            .reconstruct_history_from_rollout(turn_context, tail.items.as_slice())
            .await;
        let Some(boundary_index) = reconstruction.latest_surviving_compaction_index else {
            return unavailable_context(&thread_id, RecallAvailability::NoCompaction, source);
        };
        let compacted = tail
            .items
            .get(boundary_index)
            .and_then(|item| match item {
                RolloutItem::Compacted(compacted) => Some(compacted),
                _ => None,
            })
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "rollout reconstruction selected non-compaction index {boundary_index}"
                )
            })?;

        if compacted.replacement_history.is_none() && tail.segments_read != 1 {
            return unavailable_context(&thread_id, RecallAvailability::UnsupportedLegacy, source);
        }

        let prefix = self
            .reconstruct_history_from_rollout(turn_context, &tail.items[..boundary_index])
            .await;
        let (groups, incomplete_groups) = group_history(prefix.history)?;
        let boundary = recall_boundary(boundary_index, compacted);
        available_context(&thread_id, source, &boundary, groups, incomplete_groups)
    }
}

fn recall_boundary(index: usize, compacted: &CompactedItem) -> RecallBoundary {
    RecallBoundary {
        rollout_item_index: index,
        kind: if compacted.replacement_history.is_some() {
            RecallBoundaryKind::ReplacementHistory
        } else {
            RecallBoundaryKind::Legacy
        },
        window_number: compacted.window_number,
        first_window_id: compacted.first_window_id.clone(),
        previous_window_id: compacted.previous_window_id.clone(),
        window_id: compacted.window_id.clone(),
    }
}

fn unavailable_context(
    thread_id: &str,
    availability: RecallAvailability,
    source: RecallSourceRead,
) -> anyhow::Result<RecallContext> {
    let document = RecallDocument {
        thread_id,
        availability,
        boundary: None,
        source,
        truncated: false,
        omitted_groups: 0,
        groups: &[],
    };
    Ok(RecallContext::new(serde_json::to_string(&document)?))
}

fn available_context(
    thread_id: &str,
    source: RecallSourceRead,
    boundary: &RecallBoundary,
    groups: Vec<RecallGroup>,
    incomplete_groups: usize,
) -> anyhow::Result<RecallContext> {
    let mut selected_start = groups.len();
    let mut selected_count = 0_usize;
    while selected_start > 0 && selected_count < RECALL_RESULT_MAX_GROUPS {
        let proposed_start = selected_start - 1;
        let omitted_groups = incomplete_groups.saturating_add(proposed_start);
        let document = RecallDocument {
            thread_id,
            availability: RecallAvailability::Available,
            boundary: Some(boundary),
            source,
            truncated: omitted_groups > 0,
            omitted_groups,
            groups: &groups[proposed_start..],
        };
        let serialized = serde_json::to_string(&document)?;
        if serialized.len() > RECALL_RESULT_MAX_BYTES
            || approx_token_count(&serialized) > RECALL_RESULT_MAX_TOKENS
        {
            break;
        }
        selected_start = proposed_start;
        selected_count += 1;
    }

    let omitted_groups = incomplete_groups.saturating_add(selected_start);
    let document = RecallDocument {
        thread_id,
        availability: RecallAvailability::Available,
        boundary: Some(boundary),
        source,
        truncated: omitted_groups > 0,
        omitted_groups,
        groups: &groups[selected_start..],
    };
    let serialized = serde_json::to_string(&document)?;
    if serialized.len() > RECALL_RESULT_MAX_BYTES
        || approx_token_count(&serialized) > RECALL_RESULT_MAX_TOKENS
    {
        anyhow::bail!("recall metadata exceeds its result limits");
    }
    Ok(RecallContext::new(serialized))
}

fn group_history(items: Vec<ResponseItem>) -> anyhow::Result<(Vec<RecallGroup>, usize)> {
    let mut call_positions = HashMap::new();
    let mut output_positions = HashMap::new();
    for (index, item) in items.iter().enumerate() {
        if let Some(key) = call_pair_key(item)
            && call_positions.insert(key.clone(), index).is_some()
        {
            anyhow::bail!("duplicate historical tool call id: {key:?}");
        }
        if let Some(key) = output_pair_key(item)
            && output_positions.insert(key.clone(), index).is_some()
        {
            anyhow::bail!("duplicate historical tool output id: {key:?}");
        }
    }

    let mut grouped_indices = HashSet::new();
    let mut groups = Vec::new();
    let mut incomplete_groups = 0_usize;
    for (index, item) in items.iter().enumerate() {
        if grouped_indices.contains(&index) {
            continue;
        }
        let key = call_pair_key(item).or_else(|| output_pair_key(item));
        let Some(key) = key else {
            groups.push(RecallGroup {
                items: vec![item.clone()],
            });
            continue;
        };
        let (Some(call_index), Some(output_index)) =
            (call_positions.get(&key), output_positions.get(&key))
        else {
            grouped_indices.insert(index);
            incomplete_groups += 1;
            continue;
        };
        if output_index < call_index {
            anyhow::bail!("historical tool output precedes its call: {key:?}");
        }
        let mut pair_indices = [*call_index, *output_index];
        pair_indices.sort_unstable();
        if index != pair_indices[0] {
            continue;
        }
        grouped_indices.extend(pair_indices);
        groups.push(RecallGroup {
            items: pair_indices
                .into_iter()
                .map(|pair_index| items[pair_index].clone())
                .collect(),
        });
    }
    Ok((groups, incomplete_groups))
}

fn call_pair_key(item: &ResponseItem) -> Option<PairKey> {
    match item {
        ResponseItem::FunctionCall { call_id, .. } => Some(PairKey::Function(call_id.clone())),
        ResponseItem::LocalShellCall {
            call_id: Some(call_id),
            ..
        } => Some(PairKey::Function(call_id.clone())),
        ResponseItem::CustomToolCall { call_id, .. } => Some(PairKey::Custom(call_id.clone())),
        ResponseItem::ToolSearchCall {
            call_id: Some(call_id),
            ..
        } => Some(PairKey::ToolSearch(call_id.clone())),
        _ => None,
    }
}

fn output_pair_key(item: &ResponseItem) -> Option<PairKey> {
    match item {
        ResponseItem::FunctionCallOutput { call_id, .. } => {
            Some(PairKey::Function(call_id.clone()))
        }
        ResponseItem::CustomToolCallOutput { call_id, .. } => {
            Some(PairKey::Custom(call_id.clone()))
        }
        ResponseItem::ToolSearchOutput {
            call_id: Some(call_id),
            ..
        } => Some(PairKey::ToolSearch(call_id.clone())),
        _ => None,
    }
}

#[cfg(test)]
#[path = "recall_tests.rs"]
mod tests;
