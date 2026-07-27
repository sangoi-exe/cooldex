use std::collections::HashSet;

use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::CompactedItem;
use codex_protocol::protocol::RolloutItem;
use codex_thread_store::LoadRolloutTailParams;
use codex_thread_store::RecallRolloutSourceIssueKind;
use codex_thread_store::StoredRecallRolloutTail;
use codex_utils_output_truncation::approx_token_count;
use serde::Serialize;

use super::Session;
use super::TurnContext;
use crate::context::RecallContext;
use crate::tool_batch::ToolBatchMatch;
use crate::tool_batch::ToolItemKind;
use crate::tool_batch::classify_tool_item;
use crate::tool_batch::tool_batch_at;

pub(crate) const RECALL_SOURCE_MAX_BYTES: u64 = 16 * 1024 * 1024;
pub(crate) const RECALL_SOURCE_MAX_RECORDS: usize = 8_192;
const RECALL_RESULT_MAX_BYTES: usize = 32 * 1024;
const RECALL_RESULT_MAX_GROUPS: usize = 64;
const RECALL_RESULT_MAX_TOKENS: usize = 8_000;
const RECALL_DIAGNOSTIC_MAX_BYTES: usize = 1_024;
const RECALL_METADATA_OMITTED_MESSAGE: &str =
    "recall source metadata exceeded its result limits and was omitted";

#[derive(Debug, thiserror::Error)]
pub(crate) enum RecallContextError {
    #[error("bounded rollout tail belongs to {actual}, not {expected}")]
    ThreadMismatch {
        expected: codex_protocol::ThreadId,
        actual: codex_protocol::ThreadId,
    },
    #[error(transparent)]
    Build(#[from] anyhow::Error),
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum RecallLoadError {
    #[error("failed to read bounded current-thread rollout: {0:#}")]
    Source(anyhow::Error),
    #[error(transparent)]
    Context(#[from] RecallContextError),
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum RecallAvailability {
    Available,
    WorkLimit,
    NoCompaction,
    UnsupportedLegacy,
    SourceError,
    UnsupportedSchema,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum RecallDiagnosticClass {
    HistoricalSchemaDrift,
    SourceReadError,
    ReconstructionError,
    ThreadMismatch,
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

#[derive(Clone, Serialize)]
struct RecallSourceRead {
    reached_start: bool,
    reached_recall_origin: bool,
    bytes_read: u64,
    records_read: usize,
    segments_read: usize,
    path: Option<String>,
    line: Option<u64>,
    byte_offset: Option<u64>,
    ordinal: Option<u64>,
    record_type: Option<String>,
    event_type: Option<String>,
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
    diagnostic_class: Option<RecallDiagnosticClass>,
    diagnostic_message: Option<&'a str>,
    truncated: bool,
    omitted_groups: usize,
    excluded_native_continuity_pairs: usize,
    groups: &'a [RecallGroup],
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum PairKey {
    Function(String),
    Custom(String),
    ToolSearch(String),
}

impl Session {
    pub(crate) async fn load_current_thread_recall_context(
        &self,
        turn_context: &TurnContext,
    ) -> Result<RecallContext, RecallLoadError> {
        if let Some(live_thread) = self.live_thread() {
            live_thread
                .flush_history()
                .await
                .map_err(|err| RecallLoadError::Source(anyhow::Error::new(err)))?;
        }
        let tail = self
            .services
            .thread_store
            .load_recall_rollout_tail(LoadRolloutTailParams {
                thread_id: self.thread_id,
                include_archived: true,
                max_bytes: RECALL_SOURCE_MAX_BYTES,
                max_records: RECALL_SOURCE_MAX_RECORDS,
            })
            .await
            .map_err(|err| RecallLoadError::Source(anyhow::Error::new(err)))?;
        self.build_recall_context(turn_context, tail)
            .await
            .map_err(RecallLoadError::from)
    }

    pub(crate) async fn build_recall_context(
        &self,
        turn_context: &TurnContext,
        tail: StoredRecallRolloutTail,
    ) -> Result<RecallContext, RecallContextError> {
        if tail.thread_id != self.thread_id {
            return Err(RecallContextError::ThreadMismatch {
                expected: self.thread_id,
                actual: tail.thread_id,
            });
        }
        let thread_id = self.thread_id.to_string();
        let mut source = RecallSourceRead {
            reached_start: tail.reached_start,
            reached_recall_origin: false,
            bytes_read: tail.bytes_read,
            records_read: tail.records_read,
            segments_read: tail.segments_read,
            path: None,
            line: None,
            byte_offset: None,
            ordinal: None,
            record_type: None,
            event_type: None,
        };
        if let Some(issue) = tail.source_issue {
            source.path = issue
                .path
                .as_deref()
                .map(|path| path.to_string_lossy().into_owned());
            source.line = issue.line;
            source.byte_offset = issue.byte_offset;
            source.ordinal = issue.ordinal;
            source.record_type = issue.record_type;
            source.event_type = issue.event_type;
            let (availability, diagnostic_class) = match issue.kind {
                RecallRolloutSourceIssueKind::SourceError => (
                    RecallAvailability::SourceError,
                    RecallDiagnosticClass::SourceReadError,
                ),
                RecallRolloutSourceIssueKind::UnsupportedSchema => (
                    RecallAvailability::UnsupportedSchema,
                    RecallDiagnosticClass::HistoricalSchemaDrift,
                ),
            };
            let diagnostic_message = bounded_diagnostic_message(issue.message.as_str());
            return unavailable_context(
                &thread_id,
                availability,
                source,
                Some(diagnostic_class),
                Some(diagnostic_message.as_str()),
            )
            .map_err(RecallContextError::from);
        }

        let reconstruction = self
            .reconstruct_history_from_rollout(turn_context, tail.items.as_slice())
            .await;
        let Some(boundary_index) = reconstruction.latest_surviving_compaction_index else {
            let availability = if tail.reached_start {
                RecallAvailability::NoCompaction
            } else {
                RecallAvailability::WorkLimit
            };
            return unavailable_context(&thread_id, availability, source, None, None)
                .map_err(RecallContextError::from);
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
            })
            .map_err(RecallContextError::from)?;

        let prefix = self
            .reconstruct_history_from_rollout(turn_context, &tail.items[..boundary_index])
            .await;
        let recall_origin_index = match compacted.previous_window_id.as_deref() {
            None if tail.reached_start => 0,
            None => {
                return unavailable_context(
                    &thread_id,
                    RecallAvailability::WorkLimit,
                    source,
                    None,
                    None,
                )
                .map_err(RecallContextError::from);
            }
            Some(expected_previous_window_id) => {
                let previous_boundary_index = match prefix.latest_surviving_compaction_index {
                    Some(previous_boundary_index) => previous_boundary_index,
                    None if !tail.reached_start => {
                        return unavailable_context(
                            &thread_id,
                            RecallAvailability::WorkLimit,
                            source,
                            None,
                            None,
                        )
                        .map_err(RecallContextError::from);
                    }
                    None => {
                        return Err(RecallContextError::Build(anyhow::anyhow!(
                            "compaction window {} names missing predecessor {expected_previous_window_id}",
                            compacted.window_id.as_deref().unwrap_or("<unknown>")
                        )));
                    }
                };
                let previous = tail.items[..boundary_index]
                    .get(previous_boundary_index)
                    .and_then(|item| match item {
                        RolloutItem::Compacted(previous) => Some(previous),
                        _ => None,
                    })
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "rollout reconstruction selected non-compaction predecessor index {previous_boundary_index}"
                        )
                    })
                    .map_err(RecallContextError::from)?;
                let actual_previous_window_id = previous.window_id.as_deref().ok_or_else(|| {
                    RecallContextError::Build(anyhow::anyhow!(
                        "compaction predecessor at index {previous_boundary_index} has no window id"
                    ))
                })?;
                if actual_previous_window_id != expected_previous_window_id {
                    return Err(RecallContextError::Build(anyhow::anyhow!(
                        "compaction predecessor mismatch: expected {expected_previous_window_id}, found {actual_previous_window_id}"
                    )));
                }
                if previous.replacement_history.is_none() {
                    if !tail.reached_start {
                        return unavailable_context(
                            &thread_id,
                            RecallAvailability::WorkLimit,
                            source,
                            None,
                            None,
                        )
                        .map_err(RecallContextError::from);
                    }
                    if tail.segments_read != 1 {
                        return unavailable_context(
                            &thread_id,
                            RecallAvailability::UnsupportedLegacy,
                            source,
                            None,
                            None,
                        )
                        .map_err(RecallContextError::from);
                    }
                }
                previous_boundary_index + 1
            }
        };
        source.reached_recall_origin = true;

        if compacted.replacement_history.is_none() && tail.segments_read != 1 {
            return unavailable_context(
                &thread_id,
                RecallAvailability::UnsupportedLegacy,
                source,
                None,
                None,
            )
            .map_err(RecallContextError::from);
        }

        let native_continuity_pairs = compacted
            .replacement_history
            .as_deref()
            .map(complete_native_continuity_pair_keys)
            .unwrap_or_default();
        let retained_user_messages = compacted
            .replacement_history
            .as_deref()
            .into_iter()
            .flatten()
            .filter(|item| {
                matches!(
                    item,
                    ResponseItem::Message { role, .. } if role == "user"
                )
            })
            .collect::<Vec<_>>();
        let excluded_native_continuity_pairs = native_continuity_pairs.len();
        let recall_delta = self
            .reconstruct_history_from_rollout(
                turn_context,
                &tail.items[recall_origin_index..boundary_index],
            )
            .await;
        let history = recall_delta
            .history
            .into_iter()
            .filter(|item| {
                let duplicates_retained_user =
                    retained_user_messages
                        .iter()
                        .any(|retained| match (item, *retained) {
                            (
                                ResponseItem::Message {
                                    role,
                                    content,
                                    phase,
                                    ..
                                },
                                ResponseItem::Message {
                                    role: retained_role,
                                    content: retained_content,
                                    phase: retained_phase,
                                    ..
                                },
                            ) => {
                                role == "user"
                                    && retained_role == "user"
                                    && content == retained_content
                                    && phase == retained_phase
                            }
                            _ => false,
                        });
                !duplicates_retained_user
                    && call_pair_key(item)
                        .or_else(|| output_pair_key(item))
                        .is_none_or(|key| !native_continuity_pairs.contains(&key))
            })
            .collect();
        let (groups, incomplete_groups) =
            group_history(history).map_err(RecallContextError::from)?;
        let boundary = recall_boundary(boundary_index, compacted);
        available_context(
            &thread_id,
            source,
            &boundary,
            groups,
            incomplete_groups,
            excluded_native_continuity_pairs,
        )
        .map_err(RecallContextError::from)
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
    diagnostic_class: Option<RecallDiagnosticClass>,
    diagnostic_message: Option<&str>,
) -> anyhow::Result<RecallContext> {
    let document = RecallDocument {
        thread_id,
        availability,
        boundary: None,
        source: source.clone(),
        diagnostic_class,
        diagnostic_message,
        truncated: false,
        omitted_groups: 0,
        excluded_native_continuity_pairs: 0,
        groups: &[],
    };
    let serialized = serde_json::to_string(&document)?;
    if recall_result_fits(serialized.as_str()) {
        return Ok(RecallContext::unavailable(serialized));
    }

    let fallback = RecallDocument {
        thread_id,
        availability,
        boundary: None,
        source: RecallSourceRead {
            reached_start: source.reached_start,
            reached_recall_origin: source.reached_recall_origin,
            bytes_read: source.bytes_read,
            records_read: source.records_read,
            segments_read: source.segments_read,
            path: None,
            line: source.line,
            byte_offset: source.byte_offset,
            ordinal: source.ordinal,
            record_type: None,
            event_type: None,
        },
        diagnostic_class,
        diagnostic_message: Some(RECALL_METADATA_OMITTED_MESSAGE),
        truncated: true,
        omitted_groups: 0,
        excluded_native_continuity_pairs: 0,
        groups: &[],
    };
    let serialized = serde_json::to_string(&fallback)?;
    if !recall_result_fits(serialized.as_str()) {
        anyhow::bail!("fixed unavailable recall result exceeds its limits");
    }
    Ok(RecallContext::unavailable(serialized))
}

fn available_context(
    thread_id: &str,
    source: RecallSourceRead,
    boundary: &RecallBoundary,
    groups: Vec<RecallGroup>,
    incomplete_groups: usize,
    excluded_native_continuity_pairs: usize,
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
            source: source.clone(),
            diagnostic_class: None,
            diagnostic_message: None,
            truncated: omitted_groups > 0,
            omitted_groups,
            excluded_native_continuity_pairs,
            groups: &groups[proposed_start..],
        };
        let serialized = serde_json::to_string(&document)?;
        if !recall_result_fits(serialized.as_str()) {
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
        diagnostic_class: None,
        diagnostic_message: None,
        truncated: omitted_groups > 0,
        omitted_groups,
        excluded_native_continuity_pairs,
        groups: &groups[selected_start..],
    };
    let serialized = serde_json::to_string(&document)?;
    if !recall_result_fits(serialized.as_str()) {
        anyhow::bail!("recall metadata exceeds its result limits");
    }
    Ok(RecallContext::new(serialized))
}

fn recall_result_fits(serialized: &str) -> bool {
    serialized.len() <= RECALL_RESULT_MAX_BYTES
        && approx_token_count(serialized) <= RECALL_RESULT_MAX_TOKENS
}

pub(crate) fn unavailable_recall_context_for_error(
    thread_id: codex_protocol::ThreadId,
    error: &RecallLoadError,
) -> anyhow::Result<RecallContext> {
    let (diagnostic_class, diagnostic_message) = match error {
        RecallLoadError::Source(error) => {
            (RecallDiagnosticClass::SourceReadError, format!("{error:#}"))
        }
        RecallLoadError::Context(RecallContextError::ThreadMismatch { .. }) => {
            (RecallDiagnosticClass::ThreadMismatch, error.to_string())
        }
        RecallLoadError::Context(RecallContextError::Build(_)) => (
            RecallDiagnosticClass::ReconstructionError,
            error.to_string(),
        ),
    };
    let diagnostic_message = bounded_diagnostic_message(diagnostic_message.as_str());
    let thread_id = thread_id.to_string();
    unavailable_context(
        thread_id.as_str(),
        RecallAvailability::SourceError,
        RecallSourceRead {
            reached_start: false,
            reached_recall_origin: false,
            bytes_read: 0,
            records_read: 0,
            segments_read: 0,
            path: None,
            line: None,
            byte_offset: None,
            ordinal: None,
            record_type: None,
            event_type: None,
        },
        Some(diagnostic_class),
        Some(diagnostic_message.as_str()),
    )
}

fn bounded_diagnostic_message(message: &str) -> String {
    if message.len() <= RECALL_DIAGNOSTIC_MAX_BYTES {
        return message.to_string();
    }
    let mut end = RECALL_DIAGNOSTIC_MAX_BYTES.saturating_sub(3);
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &message[..end])
}

fn complete_native_continuity_pair_keys(items: &[ResponseItem]) -> HashSet<PairKey> {
    let Some(compaction_index) = items
        .iter()
        .rposition(|item| matches!(item, ResponseItem::Compaction { .. }))
    else {
        return HashSet::new();
    };
    let mut calls = HashSet::new();
    let mut outputs = HashSet::new();
    for item in &items[compaction_index + 1..] {
        if let Some(key) = call_pair_key(item) {
            calls.insert(key);
        }
        if let Some(key) = output_pair_key(item) {
            outputs.insert(key);
        }
    }
    calls.intersection(&outputs).cloned().collect()
}

fn group_history(items: Vec<ResponseItem>) -> anyhow::Result<(Vec<RecallGroup>, usize)> {
    let mut groups = Vec::new();
    let mut incomplete_groups = 0_usize;
    let mut index = 0_usize;
    while index < items.len() {
        match classify_tool_item(&items[index]) {
            ToolItemKind::NonTool => {
                groups.push(RecallGroup {
                    items: vec![items[index].clone()],
                });
                index += 1;
            }
            ToolItemKind::Call | ToolItemKind::UnsupportedCall => {
                match tool_batch_at(&items, index)
                    .map_err(|reason| anyhow::anyhow!("invalid historical tool batch: {reason}"))?
                {
                    Some(ToolBatchMatch::Complete(batch)) => {
                        let end = batch.range.end;
                        groups.push(RecallGroup {
                            items: items[batch.range].to_vec(),
                        });
                        index = end;
                    }
                    Some(ToolBatchMatch::Incomplete(batch)) => {
                        incomplete_groups += 1;
                        index = batch.end.max(index + 1);
                    }
                    None => {
                        return Err(anyhow::anyhow!(
                            "tool batch classifier disagreed with the batch grouper"
                        ));
                    }
                }
            }
            ToolItemKind::Output | ToolItemKind::UnsupportedOutput => {
                let key = output_pair_key(&items[index]);
                anyhow::bail!("historical tool output precedes its batch call: {key:?}");
            }
        }
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
