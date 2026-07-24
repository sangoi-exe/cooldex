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
    pub(crate) output_start: usize,
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
    let mut output_start = items.len();
    while output_start > 0 {
        match classified_identity(&items[output_start - 1]) {
            ClassifiedToolItem::Output(_) => output_start -= 1,
            ClassifiedToolItem::UnsupportedOutput => {
                return Err("unsupported_trailing_tool_item");
            }
            ClassifiedToolItem::Call(_)
            | ClassifiedToolItem::UnsupportedCall
            | ClassifiedToolItem::NonTool => break,
        }
    }
    if output_start == items.len() {
        return Err("no_trailing_tool_outputs");
    }

    let mut call_start = output_start;
    while call_start > 0 {
        match classified_identity(&items[call_start - 1]) {
            ClassifiedToolItem::Call(_) => call_start -= 1,
            ClassifiedToolItem::UnsupportedCall => return Err("unsupported_call_item"),
            ClassifiedToolItem::Output(_)
            | ClassifiedToolItem::UnsupportedOutput
            | ClassifiedToolItem::NonTool => break,
        }
    }
    if call_start == output_start {
        return Err("no_matching_tool_calls");
    }

    match tool_batch_at(items, call_start)? {
        Some(ToolBatchMatch::Complete(batch))
            if batch.output_start == output_start && batch.range.end == items.len() =>
        {
            Ok(batch)
        }
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

    let mut call_end = start;
    let mut call_identities = HashSet::new();
    let mut call_ids = HashSet::new();
    let mut incomplete_reason = None;
    while call_end < items.len() {
        match classified_identity(&items[call_end]) {
            ClassifiedToolItem::Call(identity) => {
                if !call_ids.insert(identity.call_id.clone()) {
                    return Err("duplicate_call_id");
                }
                if !call_identities.insert(identity) {
                    return Err("duplicate_call_identity");
                }
                call_end += 1;
            }
            ClassifiedToolItem::UnsupportedCall => {
                incomplete_reason.get_or_insert("unsupported_call_item");
                call_end += 1;
            }
            ClassifiedToolItem::Output(_)
            | ClassifiedToolItem::UnsupportedOutput
            | ClassifiedToolItem::NonTool => break,
        }
    }

    let output_start = call_end;
    let mut output_end = output_start;
    let mut output_identities = HashSet::new();
    let mut output_call_ids = HashSet::new();
    while output_end < items.len() {
        match classified_identity(&items[output_end]) {
            ClassifiedToolItem::Output(identity) => {
                if !output_call_ids.insert(identity.call_id.clone()) {
                    return Err("duplicate_output_call_id");
                }
                if !output_identities.insert(identity) {
                    return Err("duplicate_output_identity");
                }
                output_end += 1;
            }
            ClassifiedToolItem::UnsupportedOutput => {
                incomplete_reason.get_or_insert("unsupported_output_item");
                output_end += 1;
            }
            ClassifiedToolItem::Call(_)
            | ClassifiedToolItem::UnsupportedCall
            | ClassifiedToolItem::NonTool => break,
        }
    }

    if output_start == output_end {
        return Ok(Some(ToolBatchMatch::Incomplete(IncompleteToolBatch {
            end: call_end,
            reason: "no_matching_tool_outputs",
        })));
    }
    if let Some(reason) = incomplete_reason {
        return Ok(Some(ToolBatchMatch::Incomplete(IncompleteToolBatch {
            end: output_end,
            reason,
        })));
    }
    if call_identities != output_identities {
        return Ok(Some(ToolBatchMatch::Incomplete(IncompleteToolBatch {
            end: output_end,
            reason: "incomplete_or_asymmetric_tool_batch",
        })));
    }

    Ok(Some(ToolBatchMatch::Complete(CompleteToolBatch {
        range: start..output_end,
        output_start,
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
        ResponseItem::ToolSearchCall { call_id, .. } => call_id
            .as_deref()
            .and_then(|call_id| identity(call_id, PairKind::ToolSearch))
            .map_or(
                ClassifiedToolItem::UnsupportedCall,
                ClassifiedToolItem::Call,
            ),
        ResponseItem::FunctionCallOutput { call_id, .. } => identity(call_id, PairKind::Function)
            .map_or(
                ClassifiedToolItem::UnsupportedOutput,
                ClassifiedToolItem::Output,
            ),
        ResponseItem::CustomToolCallOutput { call_id, .. } => identity(call_id, PairKind::Custom)
            .map_or(
                ClassifiedToolItem::UnsupportedOutput,
                ClassifiedToolItem::Output,
            ),
        ResponseItem::ToolSearchOutput { call_id, .. } => call_id
            .as_deref()
            .and_then(|call_id| identity(call_id, PairKind::ToolSearch))
            .map_or(
                ClassifiedToolItem::UnsupportedOutput,
                ClassifiedToolItem::Output,
            ),
        ResponseItem::AdditionalTools { .. }
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
