use std::collections::HashSet;
use std::ops::Range;

use codex_protocol::models::ResponseItem;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum PairKind {
    Function,
    Custom,
    ToolSearch,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ToolIdentity {
    kind: PairKind,
    call_id: String,
}

pub(crate) enum ToolItemKind {
    Call,
    Output,
    UnsupportedCall,
    UnsupportedOutput,
    NonTool,
}

pub(crate) struct CompleteToolBatch {
    pub(crate) range: Range<usize>,
}

pub(crate) struct IncompleteToolBatch {
    pub(crate) end: usize,
    pub(crate) reason: &'static str,
}

pub(crate) enum ToolBatchMatch {
    Complete(CompleteToolBatch),
    Incomplete(IncompleteToolBatch),
}

pub(crate) fn complete_trailing_tool_batch(
    items: &[ResponseItem],
) -> Result<CompleteToolBatch, &'static str> {
    if !matches!(
        items.last().map(classified_identity),
        Some(ClassifiedToolItem::Output(_))
    ) {
        return Err("no_trailing_tool_outputs");
    }

    let mut call_start = None;
    for index in (0..items.len()).rev() {
        match classified_identity(&items[index]) {
            ClassifiedToolItem::Call(_) | ClassifiedToolItem::UnsupportedCall => {
                call_start = Some(index);
            }
            ClassifiedToolItem::Output(_) | ClassifiedToolItem::UnsupportedOutput
                if call_start.is_some() =>
            {
                break;
            }
            ClassifiedToolItem::Output(_)
            | ClassifiedToolItem::UnsupportedOutput
            | ClassifiedToolItem::NonTool => {}
        }
    }
    let Some(call_start) = call_start else {
        return Err("no_matching_tool_calls");
    };

    match tool_batch_at(items, call_start)? {
        Some(ToolBatchMatch::Complete(batch)) if batch.range.end == items.len() => Ok(batch),
        Some(ToolBatchMatch::Complete(_)) => Err("non_trailing_tool_batch"),
        Some(ToolBatchMatch::Incomplete(batch)) => Err(batch.reason),
        None => Err("no_matching_tool_calls"),
    }
}

pub(crate) fn tool_batch_at(
    items: &[ResponseItem],
    start: usize,
) -> Result<Option<ToolBatchMatch>, &'static str> {
    let Some(start_item) = items.get(start).map(classified_identity) else {
        return Ok(None);
    };
    if !matches!(
        start_item,
        ClassifiedToolItem::Call(_) | ClassifiedToolItem::UnsupportedCall
    ) {
        return Ok(None);
    }

    let mut call_identities = HashSet::new();
    let mut call_ids = HashSet::new();
    let mut output_identities = HashSet::new();
    let mut output_call_ids = HashSet::new();
    let mut incomplete_reason = None;
    let mut saw_output = false;
    let mut end = start;
    while end < items.len() {
        match classified_identity(&items[end]) {
            ClassifiedToolItem::Call(identity) => {
                if saw_output {
                    return Err("tool_call_follows_output");
                }
                if !call_ids.insert(identity.call_id.clone()) {
                    return Err("duplicate_call_id");
                }
                if !call_identities.insert(identity) {
                    return Err("duplicate_call_identity");
                }
                end += 1;
            }
            ClassifiedToolItem::UnsupportedCall => {
                if saw_output {
                    return Err("tool_call_follows_output");
                }
                incomplete_reason.get_or_insert("unsupported_call_item");
                end += 1;
            }
            ClassifiedToolItem::Output(identity) => {
                saw_output = true;
                if !output_call_ids.insert(identity.call_id.clone()) {
                    return Err("duplicate_output_call_id");
                }
                if !output_identities.insert(identity) {
                    return Err("duplicate_output_identity");
                }
                if !output_identities.is_subset(&call_identities) {
                    return Err("output_without_matching_call");
                }
                end += 1;
                if incomplete_reason.is_none() && output_identities == call_identities {
                    return Ok(Some(ToolBatchMatch::Complete(CompleteToolBatch {
                        range: start..end,
                    })));
                }
            }
            ClassifiedToolItem::UnsupportedOutput => {
                return Err("unsupported_output_item");
            }
            ClassifiedToolItem::NonTool => end += 1,
        }
    }

    if !saw_output {
        return Ok(Some(ToolBatchMatch::Incomplete(IncompleteToolBatch {
            end,
            reason: "no_matching_tool_outputs",
        })));
    }
    if let Some(reason) = incomplete_reason {
        return Ok(Some(ToolBatchMatch::Incomplete(IncompleteToolBatch {
            end,
            reason,
        })));
    }
    Ok(Some(ToolBatchMatch::Incomplete(IncompleteToolBatch {
        end,
        reason: "incomplete_or_asymmetric_tool_batch",
    })))
}

pub(crate) fn classify_tool_item(item: &ResponseItem) -> ToolItemKind {
    match classified_identity(item) {
        ClassifiedToolItem::Call(_) => ToolItemKind::Call,
        ClassifiedToolItem::Output(_) => ToolItemKind::Output,
        ClassifiedToolItem::UnsupportedCall => ToolItemKind::UnsupportedCall,
        ClassifiedToolItem::UnsupportedOutput => ToolItemKind::UnsupportedOutput,
        ClassifiedToolItem::NonTool => ToolItemKind::NonTool,
    }
}

enum ClassifiedToolItem {
    Call(ToolIdentity),
    Output(ToolIdentity),
    UnsupportedCall,
    UnsupportedOutput,
    NonTool,
}

fn classified_identity(item: &ResponseItem) -> ClassifiedToolItem {
    match item {
        ResponseItem::FunctionCall { call_id, .. } => identity(call_id, PairKind::Function).map_or(
            ClassifiedToolItem::UnsupportedCall,
            ClassifiedToolItem::Call,
        ),
        ResponseItem::LocalShellCall { call_id, .. } => call_id
            .as_deref()
            .and_then(|call_id| identity(call_id, PairKind::Function))
            .map_or(
                ClassifiedToolItem::UnsupportedCall,
                ClassifiedToolItem::Call,
            ),
        ResponseItem::CustomToolCall { call_id, .. } => identity(call_id, PairKind::Custom).map_or(
            ClassifiedToolItem::UnsupportedCall,
            ClassifiedToolItem::Call,
        ),
        ResponseItem::ToolSearchCall {
            execution: server_execution,
            ..
        } if server_execution == "server" => ClassifiedToolItem::NonTool,
        ResponseItem::ToolSearchCall { call_id, .. } => call_id
            .as_deref()
            .and_then(|call_id| identity(call_id, PairKind::ToolSearch))
            .map_or(
                ClassifiedToolItem::UnsupportedCall,
                ClassifiedToolItem::Call,
            ),
        ResponseItem::FunctionCallOutput {
            call_id: Some(call_id),
            ..
        } => identity(call_id, PairKind::Function).map_or(
            ClassifiedToolItem::UnsupportedOutput,
            ClassifiedToolItem::Output,
        ),
        ResponseItem::FunctionCallOutput {
            call_id: None,
            name: Some(name),
            ..
        } if !name.trim().is_empty() => ClassifiedToolItem::NonTool,
        ResponseItem::FunctionCallOutput { call_id: None, .. } => {
            ClassifiedToolItem::UnsupportedOutput
        }
        ResponseItem::CustomToolCallOutput { call_id, .. } => identity(call_id, PairKind::Custom)
            .map_or(
                ClassifiedToolItem::UnsupportedOutput,
                ClassifiedToolItem::Output,
            ),
        ResponseItem::ToolSearchOutput {
            execution: server_execution,
            ..
        } if server_execution == "server" => ClassifiedToolItem::NonTool,
        ResponseItem::ToolSearchOutput { call_id, .. } => call_id
            .as_deref()
            .and_then(|call_id| identity(call_id, PairKind::ToolSearch))
            .map_or(
                ClassifiedToolItem::UnsupportedOutput,
                ClassifiedToolItem::Output,
            ),
        // Merge-safety anchor: configuration controls remain singleton non-tool boundaries.
        ResponseItem::AdditionalTools { .. }
        | ResponseItem::ConfigurationUpdate { .. }
        | ResponseItem::Message { .. }
        | ResponseItem::AgentMessage { .. }
        | ResponseItem::Reasoning { .. }
        | ResponseItem::WebSearchCall { .. }
        | ResponseItem::ImageGenerationCall { .. }
        | ResponseItem::Compaction { .. }
        | ResponseItem::CompactionTrigger { .. }
        | ResponseItem::ContextCompaction { .. }
        | ResponseItem::Other => ClassifiedToolItem::NonTool,
    }
}

fn identity(call_id: &str, kind: PairKind) -> Option<ToolIdentity> {
    let call_id = call_id.trim();
    if call_id.is_empty() {
        return None;
    }
    Some(ToolIdentity {
        kind,
        call_id: call_id.to_string(),
    })
}
