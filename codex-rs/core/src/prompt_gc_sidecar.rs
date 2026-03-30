use std::collections::HashMap;
use std::collections::HashSet;

use crate::response_item_utils::function_call_output_token_qty;
use crate::response_item_utils::local_shell_call_output_id;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::LocalShellAction;
use codex_protocol::models::MessagePhase;
use codex_protocol::models::ReasoningItemContent;
use codex_protocol::models::ReasoningItemReasoningSummary;
use codex_protocol::models::ResponseItem;

pub(crate) const MAX_UNITS_PER_RETRIEVE: usize = 16;
pub(crate) const MAX_RAW_BYTES_PER_RETRIEVE: usize = 24_000;
pub(crate) const PROMPT_GC_MIN_FUNCTION_CALL_OUTPUT_TOKEN_QTY: usize = 200;
pub(crate) const PROMPT_GC_MIN_FINAL_ANSWER_SELECTABLE_RAW_BYTES: usize =
    MAX_RAW_BYTES_PER_RETRIEVE / MAX_UNITS_PER_RETRIEVE;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PromptGcObservedItem {
    Recorded { item: ResponseItem },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PromptGcCheckpoint {
    pub(crate) checkpoint_id: String,
    pub(crate) checkpoint_seq: u64,
    pub(crate) eligible_unit_count: usize,
    pub(crate) phase: MessagePhase,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PromptGcUnitKind {
    Reasoning,
    ToolPair,
    ToolResult,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PromptGcUnitResolver {
    Reasoning {
        fingerprint: String,
    },
    ToolPair {
        call_id: String,
        call_fingerprint: String,
        output_fingerprint: String,
        call_name: String,
    },
    ToolResult {
        fingerprint: String,
        call_name: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PromptGcCapturedUnit {
    pub(crate) unit_key: u64,
    pub(crate) chunk_id: String,
    pub(crate) kind: PromptGcUnitKind,
    pub(crate) payload_text: String,
    pub(crate) approx_bytes: usize,
    pub(crate) function_call_output_token_qty: Option<usize>,
    pub(crate) resolver: PromptGcUnitResolver,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PromptGcPendingCall {
    fingerprint: String,
    payload_text: String,
    call_name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PromptGcCheckpointEligibility {
    pub(crate) triggering_function_call_output_count: usize,
    pub(crate) selectable_unit_count: usize,
    pub(crate) selectable_raw_bytes: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PromptGcStatus {
    pub(crate) blocked_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PromptGcApplyOutcome {
    pub(crate) checkpoint_id: String,
    pub(crate) checkpoint_seq: u64,
    pub(crate) applied_unit_keys: Vec<u64>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct PromptGcSidecar {
    turn_id: Option<String>,
    next_unit_key: u64,
    next_checkpoint_seq: u64,
    next_chunk_seq: u64,
    units: Vec<PromptGcCapturedUnit>,
    compacted_unit_keys: HashSet<u64>,
    pending_function_calls: HashMap<String, Vec<PromptGcPendingCall>>,
    pending_custom_calls: HashMap<String, Vec<PromptGcPendingCall>>,
    pending_checkpoint: Option<PromptGcCheckpoint>,
    active_checkpoint: Option<PromptGcCheckpoint>,
    pending_apply_outcome: Option<PromptGcApplyOutcome>,
    running: bool,
    pub(crate) status: PromptGcStatus,
}

impl PromptGcSidecar {
    pub(crate) fn bind_turn(&mut self, turn_id: impl Into<String>) {
        self.turn_id = Some(turn_id.into());
        self.status.blocked_reason = None;
    }

    pub(crate) fn observe_recorded_item(&mut self, item: &ResponseItem) {
        if self.status.blocked_reason.is_some() {
            return;
        }
        match item {
            ResponseItem::Reasoning { .. } => {
                if let Some(payload_text) = reasoning_payload_text(item) {
                    self.push_reasoning_unit(item, payload_text);
                }
            }
            ResponseItem::FunctionCall {
                call_id,
                name,
                arguments,
                ..
            } => {
                let payload_text =
                    format!("tool_call\nname: {name}\ncall_id: {call_id}\narguments:\n{arguments}");
                let pending = PromptGcPendingCall {
                    fingerprint: response_item_fingerprint(item),
                    payload_text,
                    call_name: name.clone(),
                };
                self.pending_function_calls
                    .entry(call_id.clone())
                    .or_default()
                    .push(pending);
            }
            ResponseItem::CustomToolCall {
                call_id,
                name,
                input,
                ..
            } => {
                let payload_text =
                    format!("tool_call\nname: {name}\ncall_id: {call_id}\ninput:\n{input}");
                let pending = PromptGcPendingCall {
                    fingerprint: response_item_fingerprint(item),
                    payload_text,
                    call_name: name.clone(),
                };
                self.pending_custom_calls
                    .entry(call_id.clone())
                    .or_default()
                    .push(pending);
            }
            ResponseItem::LocalShellCall {
                id,
                call_id,
                action,
                ..
            } => {
                let Some(call_id) = local_shell_call_output_id(id, call_id) else {
                    return;
                };
                // Merge-safety anchor: local shell records as a call item plus
                // a later FunctionCallOutput. PromptGcSidecar must treat it as
                // function-like ownership or shell transcript bloat becomes
                // permanently ineligible for prompt GC.
                let payload_text = format!(
                    "tool_call\nname: local_shell\ncall_id: {call_id}\naction:\n{}",
                    local_shell_action_text(action)
                );
                let pending = PromptGcPendingCall {
                    fingerprint: response_item_fingerprint(item),
                    payload_text,
                    call_name: "local_shell".to_string(),
                };
                self.pending_function_calls
                    .entry(call_id)
                    .or_default()
                    .push(pending);
            }
            ResponseItem::FunctionCallOutput { call_id, output } => {
                if let Some(pending) = take_unambiguous_pending_function_call(
                    &mut self.pending_function_calls,
                    call_id,
                ) {
                    self.push_tool_pair_unit(call_id, output, item, pending);
                } else {
                    // Merge-safety anchor: if multiple function-like producers share the same
                    // logical call_id, PromptGcSidecar cannot prove which call owns this output.
                    // Drop the entire pending queue for that call_id so later same-turn reuse
                    // cannot be mispaired against stale ambiguous ownership.
                    discard_pending_calls(&mut self.pending_function_calls, call_id);
                }
            }
            ResponseItem::CustomToolCallOutput {
                call_id,
                name,
                output,
                ..
            } => {
                if let Some(pending) = take_pending_custom_call_for_output(
                    &mut self.pending_custom_calls,
                    call_id,
                    name.as_deref(),
                ) {
                    self.push_tool_pair_unit(call_id, output, item, pending);
                }
            }
            ResponseItem::ToolSearchOutput { .. } => {
                self.push_tool_result_unit("tool_search", item);
            }
            ResponseItem::WebSearchCall { .. } => {
                // Merge-safety anchor: some tool classes materialize their
                // result inline as a single response item rather than a
                // call/output pair. PromptGcSidecar must capture them directly.
                self.push_tool_result_unit("web_search", item);
            }
            ResponseItem::ImageGenerationCall { .. } => {
                self.push_tool_result_unit("image_generation", item);
            }
            ResponseItem::Message { role, phase, .. } => {
                if role == "assistant"
                    && let Some(phase) = phase.clone()
                {
                    self.observe_phase_checkpoint(phase);
                }
            }
            _ => {}
        }
    }

    pub(crate) fn observe_recorded_batch(&mut self, observed_items: &[PromptGcObservedItem]) {
        if self.status.blocked_reason.is_some() {
            return;
        }
        for observed in observed_items {
            let PromptGcObservedItem::Recorded { item } = observed;
            self.observe_recorded_item(item);
        }
    }

    pub(crate) fn take_pending_checkpoint(&mut self) -> Option<PromptGcCheckpoint> {
        if self.running || self.status.blocked_reason.is_some() {
            return None;
        }
        let checkpoint = self.pending_checkpoint.take()?;
        self.running = true;
        self.active_checkpoint = Some(checkpoint.clone());
        Some(checkpoint)
    }

    pub(crate) fn checkpoint(&self, checkpoint_id: &str) -> Option<PromptGcCheckpoint> {
        self.active_checkpoint
            .as_ref()
            .filter(|checkpoint| checkpoint.checkpoint_id == checkpoint_id)
            .cloned()
    }

    pub(crate) fn selectable_units(
        &self,
        checkpoint_id: &str,
        max_units: usize,
        max_raw_bytes: usize,
    ) -> Option<Vec<PromptGcCapturedUnit>> {
        let checkpoint = self.checkpoint(checkpoint_id)?;
        Some(
            self.collect_selectable_unit_refs(&checkpoint, max_units, max_raw_bytes)
                .into_iter()
                .cloned()
                .collect(),
        )
    }

    pub(crate) fn checkpoint_eligibility(
        &self,
        checkpoint_id: &str,
    ) -> Option<PromptGcCheckpointEligibility> {
        // Merge-safety anchor: prompt_gc activation must derive from the same
        // selectable burden that runtime plan build can actually feed into the
        // hidden summarizer. Keep these eligibility metrics aligned with the
        // final-answer fallback gate in `codex.rs`.
        let checkpoint = self.checkpoint(checkpoint_id)?;
        let selectable_units = self.collect_selectable_unit_refs(
            &checkpoint,
            MAX_UNITS_PER_RETRIEVE,
            MAX_RAW_BYTES_PER_RETRIEVE,
        );
        let mut triggering_function_call_output_count = 0usize;
        for unit in &selectable_units {
            if let Some(token_qty) = unit.function_call_output_token_qty
                && token_qty > PROMPT_GC_MIN_FUNCTION_CALL_OUTPUT_TOKEN_QTY
            {
                triggering_function_call_output_count =
                    triggering_function_call_output_count.saturating_add(1);
            }
        }
        let selectable_unit_count = selectable_units.len();
        let selectable_raw_bytes = selectable_units
            .iter()
            .fold(0usize, |acc, unit| acc.saturating_add(unit.approx_bytes));
        Some(PromptGcCheckpointEligibility {
            triggering_function_call_output_count,
            selectable_unit_count,
            selectable_raw_bytes,
        })
    }

    pub(crate) fn complete_cycle(&mut self, outcome: PromptGcApplyOutcome) {
        for unit_key in outcome.applied_unit_keys {
            self.compacted_unit_keys.insert(unit_key);
        }
        self.status.blocked_reason = None;
        self.running = false;
        if self
            .pending_apply_outcome
            .as_ref()
            .is_some_and(|pending| pending.checkpoint_id == outcome.checkpoint_id)
        {
            self.pending_apply_outcome = None;
        }
        if self
            .active_checkpoint
            .as_ref()
            .is_some_and(|checkpoint| checkpoint.checkpoint_id == outcome.checkpoint_id)
        {
            self.active_checkpoint = None;
        }
    }

    pub(crate) fn clear_pending_calls_for_rewrite(&mut self) {
        self.pending_function_calls.clear();
        self.pending_custom_calls.clear();
    }

    // Merge-safety anchor: runtime heuristics may decline a checkpoint without poisoning the
    // sidecar state; skip paths must only clear the active cycle and preserve prior status.
    pub(crate) fn skip_cycle(&mut self, checkpoint_id: &str) {
        self.running = false;
        if self
            .pending_apply_outcome
            .as_ref()
            .is_some_and(|pending| pending.checkpoint_id == checkpoint_id)
        {
            self.pending_apply_outcome = None;
        }
        if self
            .active_checkpoint
            .as_ref()
            .is_some_and(|checkpoint| checkpoint.checkpoint_id == checkpoint_id)
        {
            self.active_checkpoint = None;
        }
    }

    pub(crate) fn fail_cycle(&mut self, checkpoint_id: &str, error: impl Into<String>) {
        let _ = error.into();
        self.running = false;
        if self
            .pending_apply_outcome
            .as_ref()
            .is_some_and(|pending| pending.checkpoint_id == checkpoint_id)
        {
            self.pending_apply_outcome = None;
        }
        if self
            .active_checkpoint
            .as_ref()
            .is_some_and(|checkpoint| checkpoint.checkpoint_id == checkpoint_id)
        {
            self.active_checkpoint = None;
        }
    }

    pub(crate) fn block_remaining_turn(&mut self, checkpoint_id: &str, error: impl Into<String>) {
        let error = error.into();
        self.fail_cycle(checkpoint_id, error.clone());
        self.pending_function_calls.clear();
        self.pending_custom_calls.clear();
        self.pending_checkpoint = None;
        self.status.blocked_reason = Some(error);
    }

    pub(crate) fn note_apply_outcome(&mut self, checkpoint_id: &str, applied_unit_keys: Vec<u64>) {
        let Some(checkpoint) = self.checkpoint(checkpoint_id) else {
            return;
        };
        self.pending_apply_outcome = Some(PromptGcApplyOutcome {
            checkpoint_id: checkpoint_id.to_string(),
            checkpoint_seq: checkpoint.checkpoint_seq,
            applied_unit_keys,
        });
    }

    pub(crate) fn take_noted_apply_outcome(
        &mut self,
        checkpoint_id: &str,
    ) -> Option<PromptGcApplyOutcome> {
        let outcome = self.pending_apply_outcome.take()?;
        if outcome.checkpoint_id == checkpoint_id {
            return Some(outcome);
        }
        self.pending_apply_outcome = Some(outcome);
        None
    }

    pub(crate) fn recover_noted_apply_outcome(&mut self) -> Option<PromptGcApplyOutcome> {
        let checkpoint_id = self.active_checkpoint.as_ref()?.checkpoint_id.clone();
        let outcome = self.take_noted_apply_outcome(&checkpoint_id)?;
        self.complete_cycle(outcome.clone());
        Some(outcome)
    }

    fn observe_phase_checkpoint(&mut self, phase: MessagePhase) {
        if self.status.blocked_reason.is_some() {
            return;
        }
        let checkpoint_seq = self.next_checkpoint_seq;
        self.next_checkpoint_seq += 1;
        let turn_id = self.turn_id.as_deref().unwrap_or("active-turn");
        self.pending_checkpoint = Some(PromptGcCheckpoint {
            checkpoint_id: format!("{turn_id}:prompt_gc:{checkpoint_seq}"),
            checkpoint_seq,
            eligible_unit_count: self.units.len(),
            phase,
        });
    }

    fn push_reasoning_unit(&mut self, item: &ResponseItem, payload_text: String) {
        let unit_key = self.next_unit_key;
        self.next_unit_key += 1;
        let chunk_id = format!("prompt_gc_chunk_{}", self.next_chunk_seq);
        self.next_chunk_seq += 1;
        self.units.push(PromptGcCapturedUnit {
            unit_key,
            chunk_id,
            kind: PromptGcUnitKind::Reasoning,
            approx_bytes: payload_text.len(),
            function_call_output_token_qty: None,
            payload_text,
            resolver: PromptGcUnitResolver::Reasoning {
                fingerprint: response_item_fingerprint(item),
            },
        });
    }

    fn push_tool_pair_unit(
        &mut self,
        call_id: &str,
        output: &FunctionCallOutputPayload,
        output_item: &ResponseItem,
        pending: PromptGcPendingCall,
    ) {
        let output_text = function_call_output_text(output);
        let payload_text = format!(
            "{}\n\ntool_output\ncall_id: {call_id}\noutput:\n{output_text}",
            pending.payload_text
        );
        let unit_key = self.next_unit_key;
        self.next_unit_key += 1;
        let chunk_id = format!("prompt_gc_chunk_{}", self.next_chunk_seq);
        self.next_chunk_seq += 1;
        self.units.push(PromptGcCapturedUnit {
            unit_key,
            chunk_id,
            kind: PromptGcUnitKind::ToolPair,
            approx_bytes: payload_text.len(),
            function_call_output_token_qty: match output_item {
                ResponseItem::FunctionCallOutput { .. } => function_call_output_token_qty(output),
                _ => None,
            },
            payload_text,
            resolver: PromptGcUnitResolver::ToolPair {
                call_id: call_id.to_string(),
                call_fingerprint: pending.fingerprint,
                output_fingerprint: response_item_fingerprint(output_item),
                call_name: pending.call_name,
            },
        });
    }

    fn push_tool_result_unit(&mut self, call_name: &str, item: &ResponseItem) {
        let payload_text = format!(
            "tool_output\nname: {call_name}\npayload:\n{}",
            response_item_payload_text(item)
        );
        let unit_key = self.next_unit_key;
        self.next_unit_key += 1;
        let chunk_id = format!("prompt_gc_chunk_{}", self.next_chunk_seq);
        self.next_chunk_seq += 1;
        self.units.push(PromptGcCapturedUnit {
            unit_key,
            chunk_id,
            kind: PromptGcUnitKind::ToolResult,
            approx_bytes: payload_text.len(),
            function_call_output_token_qty: None,
            payload_text,
            resolver: PromptGcUnitResolver::ToolResult {
                fingerprint: response_item_fingerprint(item),
                call_name: call_name.to_string(),
            },
        });
    }

    fn collect_selectable_unit_refs<'a>(
        &'a self,
        checkpoint: &PromptGcCheckpoint,
        max_units: usize,
        max_raw_bytes: usize,
    ) -> Vec<&'a PromptGcCapturedUnit> {
        let mut selected = Vec::new();
        let mut selected_bytes = 0usize;
        for unit in self.units.iter().take(checkpoint.eligible_unit_count) {
            if self.compacted_unit_keys.contains(&unit.unit_key) {
                continue;
            }
            let projected_bytes = selected_bytes.saturating_add(unit.approx_bytes);
            // Merge-safety anchor: if the first uncompacted unit alone exceeds the raw-byte cap,
            // keep it as a singleton selection instead of starving later checkpoints forever
            // behind one oversize transcript.
            if !selected.is_empty() && projected_bytes > max_raw_bytes {
                break;
            }
            selected.push(unit);
            selected_bytes = projected_bytes;
            if selected.len() >= max_units || selected_bytes >= max_raw_bytes {
                break;
            }
        }
        selected
    }
}

fn response_item_payload_text(item: &ResponseItem) -> String {
    serde_json::to_string_pretty(item)
        .unwrap_or_else(|error| format!("failed_to_serialize: {error}"))
}

fn reasoning_payload_text(item: &ResponseItem) -> Option<String> {
    let ResponseItem::Reasoning {
        summary, content, ..
    } = item
    else {
        return None;
    };
    let mut segments: Vec<String> = summary
        .iter()
        .filter_map(|summary_item| match summary_item {
            ReasoningItemReasoningSummary::SummaryText { text } => {
                let trimmed = text.trim();
                (!trimmed.is_empty()).then_some(trimmed.to_string())
            }
        })
        .collect();
    if segments.is_empty()
        && let Some(content_items) = content
    {
        for content_item in content_items {
            match content_item {
                ReasoningItemContent::ReasoningText { text }
                | ReasoningItemContent::Text { text } => {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        segments.push(trimmed.to_string());
                    }
                }
            }
        }
    }
    (!segments.is_empty()).then(|| segments.join("\n"))
}

fn response_item_fingerprint(item: &ResponseItem) -> String {
    serde_json::to_string(item).unwrap_or_else(|error| format!("failed_to_serialize:{error}"))
}

fn function_call_output_text(output: &FunctionCallOutputPayload) -> String {
    output
        .text_content()
        .map(ToOwned::to_owned)
        .or_else(|| serde_json::to_string_pretty(output).ok())
        .unwrap_or_default()
}

fn local_shell_action_text(action: &LocalShellAction) -> String {
    serde_json::to_string_pretty(action)
        .unwrap_or_else(|error| format!("failed_to_serialize: {error}"))
}

fn pop_pending_call(
    pending_calls: &mut HashMap<String, Vec<PromptGcPendingCall>>,
    call_id: &str,
) -> Option<PromptGcPendingCall> {
    let calls = pending_calls.get_mut(call_id)?;
    let pending = (!calls.is_empty()).then(|| calls.remove(0));
    if calls.is_empty() {
        pending_calls.remove(call_id);
    }
    pending
}

fn take_unambiguous_pending_function_call(
    pending_calls: &mut HashMap<String, Vec<PromptGcPendingCall>>,
    call_id: &str,
) -> Option<PromptGcPendingCall> {
    let calls = pending_calls.get(call_id)?;
    if calls.len() != 1 {
        return None;
    }
    pop_pending_call(pending_calls, call_id)
}

fn take_pending_custom_call_for_output(
    pending_calls: &mut HashMap<String, Vec<PromptGcPendingCall>>,
    call_id: &str,
    output_name: Option<&str>,
) -> Option<PromptGcPendingCall> {
    let mut calls = pending_calls.remove(call_id)?;
    if calls.len() == 1 {
        return calls.pop();
    }
    let output_name = output_name?;
    let mut matching_indices = calls
        .iter()
        .enumerate()
        .filter_map(|(index, pending)| (pending.call_name == output_name).then_some(index));
    let match_index = matching_indices.next()?;
    if matching_indices.next().is_some() {
        return None;
    }
    let pending = calls.remove(match_index);
    if !calls.is_empty() {
        pending_calls.insert(call_id.to_string(), calls);
    }
    Some(pending)
}

fn discard_pending_calls(
    pending_calls: &mut HashMap<String, Vec<PromptGcPendingCall>>,
    call_id: &str,
) {
    pending_calls.remove(call_id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::models::ContentItem;
    use codex_protocol::models::FunctionCallOutputBody;
    use codex_protocol::models::LocalShellExecAction;
    use codex_protocol::models::LocalShellStatus;
    use codex_protocol::models::WebSearchAction;
    use pretty_assertions::assert_eq;

    #[test]
    fn captures_tool_pairs_and_reasoning_before_phase_checkpoint() {
        let mut sidecar = PromptGcSidecar::default();
        sidecar.bind_turn("turn-1");

        let reasoning = ResponseItem::Reasoning {
            id: String::new(),
            summary: Vec::new(),
            content: Some(vec![
                codex_protocol::models::ReasoningItemContent::ReasoningText {
                    text: "captured reasoning".to_string(),
                },
            ]),
            encrypted_content: None,
        };
        sidecar.observe_recorded_item(&reasoning);

        let call = ResponseItem::FunctionCall {
            id: None,
            call_id: "call-1".to_string(),
            name: "exec_command".to_string(),
            namespace: None,
            arguments: "{\"cmd\":\"pwd\"}".to_string(),
        };
        sidecar.observe_recorded_item(&call);

        let output = ResponseItem::FunctionCallOutput {
            call_id: "call-1".to_string(),
            output: FunctionCallOutputPayload {
                body: FunctionCallOutputBody::Text("/tmp".to_string()),
                success: Some(true),
            },
        };
        sidecar.observe_recorded_item(&output);

        let phase_message = ResponseItem::Message {
            id: Some("msg-1".to_string()),
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: "phase done".to_string(),
            }],
            end_turn: None,
            phase: Some(MessagePhase::Commentary),
        };
        sidecar.observe_recorded_item(&phase_message);

        let checkpoint = sidecar.take_pending_checkpoint().expect("checkpoint");
        let units = sidecar
            .selectable_units(
                checkpoint.checkpoint_id.as_str(),
                MAX_UNITS_PER_RETRIEVE,
                MAX_RAW_BYTES_PER_RETRIEVE,
            )
            .expect("units");

        assert_eq!(checkpoint.eligible_unit_count, 2);
        assert_eq!(units.len(), 2);
        assert!(matches!(units[0].kind, PromptGcUnitKind::Reasoning));
        assert!(matches!(units[1].kind, PromptGcUnitKind::ToolPair));
        assert!(units[1].payload_text.contains("tool_output"));
    }

    #[test]
    fn captures_local_shell_and_single_item_tool_results() {
        let mut sidecar = PromptGcSidecar::default();
        sidecar.bind_turn("turn-1");

        let shell_call = ResponseItem::LocalShellCall {
            id: None,
            call_id: Some("shell-1".to_string()),
            status: LocalShellStatus::Completed,
            action: LocalShellAction::Exec(LocalShellExecAction {
                command: vec!["pwd".to_string()],
                working_directory: None,
                timeout_ms: None,
                env: None,
                user: None,
            }),
        };
        sidecar.observe_recorded_item(&shell_call);

        let shell_output = ResponseItem::FunctionCallOutput {
            call_id: "shell-1".to_string(),
            output: FunctionCallOutputPayload {
                body: FunctionCallOutputBody::Text("/tmp".to_string()),
                success: Some(true),
            },
        };
        sidecar.observe_recorded_item(&shell_output);

        let web_search = ResponseItem::WebSearchCall {
            id: Some("ws_1".to_string()),
            status: Some("completed".to_string()),
            action: Some(WebSearchAction::Search {
                query: Some("weather".to_string()),
                queries: None,
            }),
        };
        sidecar.observe_recorded_item(&web_search);

        let image_generation = ResponseItem::ImageGenerationCall {
            id: "ig_1".to_string(),
            status: "completed".to_string(),
            revised_prompt: Some("cat".to_string()),
            result: "image-ref".to_string(),
        };
        sidecar.observe_recorded_item(&image_generation);

        let phase_message = ResponseItem::Message {
            id: Some("msg-1".to_string()),
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: "phase done".to_string(),
            }],
            end_turn: None,
            phase: Some(MessagePhase::Commentary),
        };
        sidecar.observe_recorded_item(&phase_message);

        let checkpoint = sidecar.take_pending_checkpoint().expect("checkpoint");
        let units = sidecar
            .selectable_units(
                checkpoint.checkpoint_id.as_str(),
                MAX_UNITS_PER_RETRIEVE,
                MAX_RAW_BYTES_PER_RETRIEVE,
            )
            .expect("units");

        assert_eq!(checkpoint.eligible_unit_count, 3);
        assert_eq!(units.len(), 3);
        assert!(matches!(units[0].kind, PromptGcUnitKind::ToolPair));
        assert!(matches!(units[1].kind, PromptGcUnitKind::ToolResult));
        assert!(matches!(units[2].kind, PromptGcUnitKind::ToolResult));
        assert!(units[0].payload_text.contains("local_shell"));
        assert!(units[1].payload_text.contains("web_search"));
        assert!(units[2].payload_text.contains("image_generation"));
    }

    #[test]
    fn captures_tool_search_output_as_tool_result() {
        let mut sidecar = PromptGcSidecar::default();
        sidecar.bind_turn("turn-1");

        let tool_search_output = ResponseItem::ToolSearchOutput {
            call_id: Some("tool-search-1".to_string()),
            status: "completed".to_string(),
            execution: "ok".to_string(),
            tools: vec![serde_json::json!({"name": "ripgrep"})],
        };
        sidecar.observe_recorded_item(&tool_search_output);

        let phase_message = ResponseItem::Message {
            id: Some("msg-1".to_string()),
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: "phase done".to_string(),
            }],
            end_turn: None,
            phase: Some(MessagePhase::Commentary),
        };
        sidecar.observe_recorded_item(&phase_message);

        let checkpoint = sidecar.take_pending_checkpoint().expect("checkpoint");
        let units = sidecar
            .selectable_units(
                checkpoint.checkpoint_id.as_str(),
                MAX_UNITS_PER_RETRIEVE,
                MAX_RAW_BYTES_PER_RETRIEVE,
            )
            .expect("units");

        assert_eq!(units.len(), 1);
        assert!(matches!(units[0].kind, PromptGcUnitKind::ToolResult));
        assert!(units[0].payload_text.contains("tool_search"));
        assert!(units[0].payload_text.contains("tool-search-1"));
    }

    #[test]
    fn custom_tool_output_name_disambiguates_duplicate_call_id() {
        let mut sidecar = PromptGcSidecar::default();
        sidecar.bind_turn("turn-1");

        let first_call = ResponseItem::CustomToolCall {
            id: None,
            call_id: "shared".to_string(),
            name: "alpha_tool".to_string(),
            input: "{\"query\":\"alpha\"}".to_string(),
            status: None,
        };
        sidecar.observe_recorded_item(&first_call);

        let second_call = ResponseItem::CustomToolCall {
            id: None,
            call_id: "shared".to_string(),
            name: "beta_tool".to_string(),
            input: "{\"query\":\"beta\"}".to_string(),
            status: None,
        };
        sidecar.observe_recorded_item(&second_call);

        let output = ResponseItem::CustomToolCallOutput {
            call_id: "shared".to_string(),
            name: Some("beta_tool".to_string()),
            output: FunctionCallOutputPayload::from_text("beta result".to_string()),
        };
        sidecar.observe_recorded_item(&output);

        let phase_message = ResponseItem::Message {
            id: Some("msg-1".to_string()),
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: "phase done".to_string(),
            }],
            end_turn: None,
            phase: Some(MessagePhase::Commentary),
        };
        sidecar.observe_recorded_item(&phase_message);

        let checkpoint = sidecar.take_pending_checkpoint().expect("checkpoint");
        let units = sidecar
            .selectable_units(
                checkpoint.checkpoint_id.as_str(),
                MAX_UNITS_PER_RETRIEVE,
                MAX_RAW_BYTES_PER_RETRIEVE,
            )
            .expect("units");

        assert_eq!(units.len(), 1);
        assert!(matches!(units[0].kind, PromptGcUnitKind::ToolPair));
        assert!(units[0].payload_text.contains("name: beta_tool"));
        assert!(!units[0].payload_text.contains("name: alpha_tool"));
    }

    #[test]
    fn encrypted_only_reasoning_does_not_create_selectable_units() {
        let mut sidecar = PromptGcSidecar::default();
        sidecar.bind_turn("turn-1");

        let reasoning = ResponseItem::Reasoning {
            id: "reasoning-1".to_string(),
            summary: Vec::new(),
            content: None,
            encrypted_content: Some("opaque".repeat(512)),
        };
        sidecar.observe_recorded_item(&reasoning);

        let phase_message = ResponseItem::Message {
            id: Some("msg-1".to_string()),
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: "phase done".to_string(),
            }],
            end_turn: None,
            phase: Some(MessagePhase::Commentary),
        };
        sidecar.observe_recorded_item(&phase_message);

        let checkpoint = sidecar.take_pending_checkpoint().expect("checkpoint");
        let eligibility = sidecar
            .checkpoint_eligibility(checkpoint.checkpoint_id.as_str())
            .expect("eligibility");
        let units = sidecar
            .selectable_units(
                checkpoint.checkpoint_id.as_str(),
                MAX_UNITS_PER_RETRIEVE,
                MAX_RAW_BYTES_PER_RETRIEVE,
            )
            .expect("units");

        assert_eq!(checkpoint.eligible_unit_count, 0);
        assert_eq!(eligibility.selectable_unit_count, 0);
        assert_eq!(units.len(), 0);
    }

    #[test]
    fn checkpoint_eligibility_function_call_output_token_qty_over_200_triggers() {
        let mut sidecar = PromptGcSidecar::default();
        sidecar.bind_turn("turn-1");

        let call = ResponseItem::FunctionCall {
            id: None,
            call_id: "call-1".to_string(),
            name: "exec_command".to_string(),
            namespace: None,
            arguments: "{\"cmd\":\"pwd\"}".to_string(),
        };
        sidecar.observe_recorded_item(&call);

        let output = ResponseItem::FunctionCallOutput {
            call_id: "call-1".to_string(),
            output: FunctionCallOutputPayload {
                body: FunctionCallOutputBody::Text(
                    "Wall time: 0.1000 seconds\nToken qty: 201\nOutput:\nhello".to_string(),
                ),
                success: Some(true),
            },
        };
        sidecar.observe_recorded_item(&output);

        let phase_message = ResponseItem::Message {
            id: Some("msg-1".to_string()),
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: "phase done".to_string(),
            }],
            end_turn: None,
            phase: Some(MessagePhase::Commentary),
        };
        sidecar.observe_recorded_item(&phase_message);

        let checkpoint = sidecar.take_pending_checkpoint().expect("checkpoint");
        let eligibility = sidecar
            .checkpoint_eligibility(checkpoint.checkpoint_id.as_str())
            .expect("eligibility");
        assert_eq!(eligibility.triggering_function_call_output_count, 1);
    }

    #[test]
    fn checkpoint_eligibility_function_call_output_token_qty_at_200_stays_non_triggering() {
        let mut sidecar = PromptGcSidecar::default();
        sidecar.bind_turn("turn-1");

        let call = ResponseItem::FunctionCall {
            id: None,
            call_id: "call-1".to_string(),
            name: "exec_command".to_string(),
            namespace: None,
            arguments: "{\"cmd\":\"pwd\"}".to_string(),
        };
        sidecar.observe_recorded_item(&call);

        let output = ResponseItem::FunctionCallOutput {
            call_id: "call-1".to_string(),
            output: FunctionCallOutputPayload {
                body: FunctionCallOutputBody::Text(
                    "Wall time: 0.1000 seconds\nToken qty: 200\nOutput:\nhello".to_string(),
                ),
                success: Some(true),
            },
        };
        sidecar.observe_recorded_item(&output);

        let phase_message = ResponseItem::Message {
            id: Some("msg-1".to_string()),
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: "phase done".to_string(),
            }],
            end_turn: None,
            phase: Some(MessagePhase::Commentary),
        };
        sidecar.observe_recorded_item(&phase_message);

        let checkpoint = sidecar.take_pending_checkpoint().expect("checkpoint");
        let eligibility = sidecar
            .checkpoint_eligibility(checkpoint.checkpoint_id.as_str())
            .expect("eligibility");
        assert_eq!(eligibility.triggering_function_call_output_count, 0);
    }

    #[test]
    fn checkpoint_eligibility_ignores_custom_tool_output_token_qty_marker() {
        let mut sidecar = PromptGcSidecar::default();
        sidecar.bind_turn("turn-1");

        let call = ResponseItem::CustomToolCall {
            id: None,
            call_id: "call-1".to_string(),
            name: "exec_command".to_string(),
            input: "{\"cmd\":\"pwd\"}".to_string(),
            status: None,
        };
        sidecar.observe_recorded_item(&call);

        let output = ResponseItem::CustomToolCallOutput {
            call_id: "call-1".to_string(),
            name: None,
            output: FunctionCallOutputPayload::from_text(
                "Wall time: 0.1000 seconds\nToken qty: 900\nOutput:\nhello".to_string(),
            ),
        };
        sidecar.observe_recorded_item(&output);

        let phase_message = ResponseItem::Message {
            id: Some("msg-1".to_string()),
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: "phase done".to_string(),
            }],
            end_turn: None,
            phase: Some(MessagePhase::Commentary),
        };
        sidecar.observe_recorded_item(&phase_message);

        let checkpoint = sidecar.take_pending_checkpoint().expect("checkpoint");
        let eligibility = sidecar
            .checkpoint_eligibility(checkpoint.checkpoint_id.as_str())
            .expect("eligibility");
        assert_eq!(eligibility.triggering_function_call_output_count, 0);
    }

    #[test]
    fn selectable_units_skip_ambiguous_collision_when_same_checkpoint_has_valid_trigger() {
        let mut sidecar = PromptGcSidecar::default();
        sidecar.bind_turn("turn-1");

        let valid_call = ResponseItem::FunctionCall {
            id: None,
            call_id: "call-1".to_string(),
            name: "exec_command".to_string(),
            namespace: None,
            arguments: "{\"cmd\":\"pwd\"}".to_string(),
        };
        sidecar.observe_recorded_item(&valid_call);

        let valid_output = ResponseItem::FunctionCallOutput {
            call_id: "call-1".to_string(),
            output: FunctionCallOutputPayload::from_text(
                "Wall time: 0.1000 seconds\nToken qty: 900\nOutput:\nvalid".to_string(),
            ),
        };
        sidecar.observe_recorded_item(&valid_output);

        let ambiguous_exec_command = ResponseItem::FunctionCall {
            id: None,
            call_id: "shared".to_string(),
            name: "exec_command".to_string(),
            namespace: None,
            arguments: "{\"cmd\":\"pwd\"}".to_string(),
        };
        sidecar.observe_recorded_item(&ambiguous_exec_command);

        let ambiguous_local_shell = ResponseItem::LocalShellCall {
            id: None,
            call_id: Some("shared".to_string()),
            status: LocalShellStatus::Completed,
            action: LocalShellAction::Exec(LocalShellExecAction {
                command: vec!["pwd".to_string()],
                working_directory: None,
                timeout_ms: None,
                env: None,
                user: None,
            }),
        };
        sidecar.observe_recorded_item(&ambiguous_local_shell);

        let ambiguous_output = ResponseItem::FunctionCallOutput {
            call_id: "shared".to_string(),
            output: FunctionCallOutputPayload::from_text(
                "Wall time: 0.1000 seconds\nToken qty: 900\nOutput:\nambiguous".to_string(),
            ),
        };
        sidecar.observe_recorded_item(&ambiguous_output);

        let phase_message = ResponseItem::Message {
            id: Some("msg-1".to_string()),
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: "phase done".to_string(),
            }],
            end_turn: None,
            phase: Some(MessagePhase::Commentary),
        };
        sidecar.observe_recorded_item(&phase_message);

        let checkpoint = sidecar.take_pending_checkpoint().expect("checkpoint");
        let eligibility = sidecar
            .checkpoint_eligibility(checkpoint.checkpoint_id.as_str())
            .expect("eligibility");
        let units = sidecar
            .selectable_units(
                checkpoint.checkpoint_id.as_str(),
                MAX_UNITS_PER_RETRIEVE,
                MAX_RAW_BYTES_PER_RETRIEVE,
            )
            .expect("units");
        assert_eq!(eligibility.triggering_function_call_output_count, 1);
        assert_eq!(units.len(), 1);
        assert!(units[0].payload_text.contains("call-1"));
        assert!(!units[0].payload_text.contains("call_id: shared"));
    }

    #[test]
    fn ambiguous_collision_clears_pending_queue_before_same_turn_call_id_reuse() {
        let mut sidecar = PromptGcSidecar::default();
        sidecar.bind_turn("turn-1");

        let ambiguous_exec_command = ResponseItem::FunctionCall {
            id: None,
            call_id: "shared".to_string(),
            name: "exec_command".to_string(),
            namespace: None,
            arguments: "{\"cmd\":\"printf old\"}".to_string(),
        };
        sidecar.observe_recorded_item(&ambiguous_exec_command);

        let ambiguous_local_shell = ResponseItem::LocalShellCall {
            id: None,
            call_id: Some("shared".to_string()),
            status: LocalShellStatus::Completed,
            action: LocalShellAction::Exec(LocalShellExecAction {
                command: vec!["pwd".to_string()],
                working_directory: None,
                timeout_ms: None,
                env: None,
                user: None,
            }),
        };
        sidecar.observe_recorded_item(&ambiguous_local_shell);

        let ambiguous_output = ResponseItem::FunctionCallOutput {
            call_id: "shared".to_string(),
            output: FunctionCallOutputPayload::from_text(
                "Wall time: 0.1000 seconds\nToken qty: 900\nOutput:\nambiguous".to_string(),
            ),
        };
        sidecar.observe_recorded_item(&ambiguous_output);

        let later_exec_command = ResponseItem::FunctionCall {
            id: None,
            call_id: "shared".to_string(),
            name: "exec_command".to_string(),
            namespace: None,
            arguments: "{\"cmd\":\"printf later\"}".to_string(),
        };
        sidecar.observe_recorded_item(&later_exec_command);

        let later_output = ResponseItem::FunctionCallOutput {
            call_id: "shared".to_string(),
            output: FunctionCallOutputPayload::from_text(
                "Wall time: 0.1000 seconds\nToken qty: 900\nOutput:\nlater".to_string(),
            ),
        };
        sidecar.observe_recorded_item(&later_output);

        let phase_message = ResponseItem::Message {
            id: Some("msg-1".to_string()),
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: "phase done".to_string(),
            }],
            end_turn: None,
            phase: Some(MessagePhase::Commentary),
        };
        sidecar.observe_recorded_item(&phase_message);

        let checkpoint = sidecar.take_pending_checkpoint().expect("checkpoint");
        let eligibility = sidecar
            .checkpoint_eligibility(checkpoint.checkpoint_id.as_str())
            .expect("eligibility");
        let units = sidecar
            .selectable_units(
                checkpoint.checkpoint_id.as_str(),
                MAX_UNITS_PER_RETRIEVE,
                MAX_RAW_BYTES_PER_RETRIEVE,
            )
            .expect("units");
        assert_eq!(eligibility.triggering_function_call_output_count, 1);
        assert_eq!(units.len(), 1);
        assert!(units[0].payload_text.contains("printf later"));
        assert!(!units[0].payload_text.contains("printf old"));
        assert!(!units[0].payload_text.contains("local_shell"));
    }

    #[test]
    fn checkpoint_eligibility_triggers_non_exec_function_output_token_qty_marker() {
        let mut sidecar = PromptGcSidecar::default();
        sidecar.bind_turn("turn-1");

        let call = ResponseItem::FunctionCall {
            id: None,
            call_id: "call-1".to_string(),
            name: "other_tool".to_string(),
            namespace: None,
            arguments: "{\"cmd\":\"pwd\"}".to_string(),
        };
        sidecar.observe_recorded_item(&call);

        let output = ResponseItem::FunctionCallOutput {
            call_id: "call-1".to_string(),
            output: FunctionCallOutputPayload::from_text(
                "Wall time: 0.1000 seconds\nToken qty: 900\nOutput:\nhello".to_string(),
            ),
        };
        sidecar.observe_recorded_item(&output);

        let phase_message = ResponseItem::Message {
            id: Some("msg-1".to_string()),
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: "phase done".to_string(),
            }],
            end_turn: None,
            phase: Some(MessagePhase::Commentary),
        };
        sidecar.observe_recorded_item(&phase_message);

        let checkpoint = sidecar.take_pending_checkpoint().expect("checkpoint");
        let eligibility = sidecar
            .checkpoint_eligibility(checkpoint.checkpoint_id.as_str())
            .expect("eligibility");
        assert_eq!(eligibility.triggering_function_call_output_count, 1);
    }

    #[test]
    fn checkpoint_eligibility_ignores_ambiguous_exec_command_local_shell_collision() {
        let mut sidecar = PromptGcSidecar::default();
        sidecar.bind_turn("turn-1");

        let exec_command = ResponseItem::FunctionCall {
            id: None,
            call_id: "shared".to_string(),
            name: "exec_command".to_string(),
            namespace: None,
            arguments: "{\"cmd\":\"pwd\"}".to_string(),
        };
        sidecar.observe_recorded_item(&exec_command);

        let local_shell = ResponseItem::LocalShellCall {
            id: None,
            call_id: Some("shared".to_string()),
            status: LocalShellStatus::Completed,
            action: LocalShellAction::Exec(LocalShellExecAction {
                command: vec!["pwd".to_string()],
                working_directory: None,
                timeout_ms: None,
                env: None,
                user: None,
            }),
        };
        sidecar.observe_recorded_item(&local_shell);

        let output = ResponseItem::FunctionCallOutput {
            call_id: "shared".to_string(),
            output: FunctionCallOutputPayload::from_text(
                "Wall time: 0.1000 seconds\nToken qty: 900\nOutput:\nhello".to_string(),
            ),
        };
        sidecar.observe_recorded_item(&output);

        let phase_message = ResponseItem::Message {
            id: Some("msg-1".to_string()),
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: "phase done".to_string(),
            }],
            end_turn: None,
            phase: Some(MessagePhase::Commentary),
        };
        sidecar.observe_recorded_item(&phase_message);

        let checkpoint = sidecar.take_pending_checkpoint().expect("checkpoint");
        let eligibility = sidecar
            .checkpoint_eligibility(checkpoint.checkpoint_id.as_str())
            .expect("eligibility");
        assert_eq!(eligibility.triggering_function_call_output_count, 0);
    }

    #[test]
    fn checkpoint_eligibility_ignores_ambiguous_exec_command_function_collision() {
        let mut sidecar = PromptGcSidecar::default();
        sidecar.bind_turn("turn-1");

        let other_tool = ResponseItem::FunctionCall {
            id: None,
            call_id: "shared".to_string(),
            name: "other_tool".to_string(),
            namespace: None,
            arguments: "{\"cmd\":\"echo hi\"}".to_string(),
        };
        sidecar.observe_recorded_item(&other_tool);

        let exec_command = ResponseItem::FunctionCall {
            id: None,
            call_id: "shared".to_string(),
            name: "exec_command".to_string(),
            namespace: None,
            arguments: "{\"cmd\":\"pwd\"}".to_string(),
        };
        sidecar.observe_recorded_item(&exec_command);

        let output = ResponseItem::FunctionCallOutput {
            call_id: "shared".to_string(),
            output: FunctionCallOutputPayload::from_text(
                "Wall time: 0.1000 seconds\nToken qty: 900\nOutput:\nhello".to_string(),
            ),
        };
        sidecar.observe_recorded_item(&output);

        let phase_message = ResponseItem::Message {
            id: Some("msg-1".to_string()),
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: "phase done".to_string(),
            }],
            end_turn: None,
            phase: Some(MessagePhase::Commentary),
        };
        sidecar.observe_recorded_item(&phase_message);

        let checkpoint = sidecar.take_pending_checkpoint().expect("checkpoint");
        let eligibility = sidecar
            .checkpoint_eligibility(checkpoint.checkpoint_id.as_str())
            .expect("eligibility");
        assert_eq!(eligibility.triggering_function_call_output_count, 0);
    }

    #[test]
    fn selectable_units_keep_oversize_first_unit_as_singleton_selection() {
        let mut sidecar = PromptGcSidecar::default();
        sidecar.bind_turn("turn-1");

        let call = ResponseItem::FunctionCall {
            id: None,
            call_id: "call-1".to_string(),
            name: "shell".to_string(),
            namespace: None,
            arguments: "{\"cmd\":\"cat huge.log\"}".to_string(),
        };
        sidecar.observe_recorded_item(&call);

        let output = ResponseItem::FunctionCallOutput {
            call_id: "call-1".to_string(),
            output: FunctionCallOutputPayload {
                body: FunctionCallOutputBody::Text("x".repeat(MAX_RAW_BYTES_PER_RETRIEVE + 10_000)),
                success: Some(true),
            },
        };
        sidecar.observe_recorded_item(&output);

        let later_reasoning = ResponseItem::Reasoning {
            id: "reasoning-1".to_string(),
            summary: Vec::new(),
            content: Some(vec![
                codex_protocol::models::ReasoningItemContent::ReasoningText {
                    text: "y".repeat(2_000),
                },
            ]),
            encrypted_content: None,
        };
        sidecar.observe_recorded_item(&later_reasoning);

        let phase_message = ResponseItem::Message {
            id: Some("msg-1".to_string()),
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: "phase done".to_string(),
            }],
            end_turn: None,
            phase: Some(MessagePhase::Commentary),
        };
        sidecar.observe_recorded_item(&phase_message);

        let checkpoint = sidecar.take_pending_checkpoint().expect("checkpoint");
        let units = sidecar
            .selectable_units(
                checkpoint.checkpoint_id.as_str(),
                MAX_UNITS_PER_RETRIEVE,
                MAX_RAW_BYTES_PER_RETRIEVE,
            )
            .expect("units");

        assert_eq!(checkpoint.eligible_unit_count, 2);
        assert_eq!(units.len(), 1);
        assert!(matches!(units[0].kind, PromptGcUnitKind::ToolPair));
        assert!(units[0].approx_bytes > MAX_RAW_BYTES_PER_RETRIEVE);
    }

    #[test]
    fn checkpoint_eligibility_ignores_compacted_units_but_keeps_leftovers() {
        let mut sidecar = PromptGcSidecar::default();
        sidecar.bind_turn("turn-1");

        let first = ResponseItem::Reasoning {
            id: "reasoning-1".to_string(),
            summary: vec![
                codex_protocol::models::ReasoningItemReasoningSummary::SummaryText {
                    text: "first".to_string(),
                },
            ],
            content: None,
            encrypted_content: None,
        };
        let second = ResponseItem::Reasoning {
            id: "reasoning-2".to_string(),
            summary: vec![
                codex_protocol::models::ReasoningItemReasoningSummary::SummaryText {
                    text: "second".to_string(),
                },
            ],
            content: None,
            encrypted_content: None,
        };
        let phase_one = ResponseItem::Message {
            id: Some("msg-1".to_string()),
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: "phase one".to_string(),
            }],
            end_turn: None,
            phase: Some(MessagePhase::Commentary),
        };

        sidecar.observe_recorded_item(&first);
        sidecar.observe_recorded_item(&second);
        sidecar.observe_recorded_item(&phase_one);

        let checkpoint_one = sidecar.take_pending_checkpoint().expect("checkpoint one");
        let first_unit_key = sidecar.units[0].unit_key;
        sidecar.complete_cycle(PromptGcApplyOutcome {
            checkpoint_id: checkpoint_one.checkpoint_id,
            checkpoint_seq: checkpoint_one.checkpoint_seq,
            applied_unit_keys: vec![first_unit_key],
        });

        let phase_two = ResponseItem::Message {
            id: Some("msg-2".to_string()),
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: "phase two".to_string(),
            }],
            end_turn: None,
            phase: Some(MessagePhase::Commentary),
        };
        sidecar.observe_recorded_item(&phase_two);

        let checkpoint_two = sidecar.take_pending_checkpoint().expect("checkpoint two");
        let eligibility = sidecar
            .checkpoint_eligibility(checkpoint_two.checkpoint_id.as_str())
            .expect("eligibility");
        assert_eq!(eligibility.triggering_function_call_output_count, 0);
        assert_eq!(eligibility.selectable_unit_count, 1);
        assert!(eligibility.selectable_raw_bytes > 0);
    }

    #[test]
    fn captures_local_shell_legacy_id_fallback() {
        let mut sidecar = PromptGcSidecar::default();
        sidecar.bind_turn("turn-1");

        let shell_call = ResponseItem::LocalShellCall {
            id: Some("legacy-shell".to_string()),
            call_id: None,
            status: LocalShellStatus::Completed,
            action: LocalShellAction::Exec(LocalShellExecAction {
                command: vec!["pwd".to_string()],
                working_directory: None,
                timeout_ms: None,
                env: None,
                user: None,
            }),
        };
        sidecar.observe_recorded_item(&shell_call);

        let shell_output = ResponseItem::FunctionCallOutput {
            call_id: "legacy-shell".to_string(),
            output: FunctionCallOutputPayload {
                body: FunctionCallOutputBody::Text("/tmp".to_string()),
                success: Some(true),
            },
        };
        sidecar.observe_recorded_item(&shell_output);

        let phase_message = ResponseItem::Message {
            id: Some("msg-1".to_string()),
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: "phase done".to_string(),
            }],
            end_turn: None,
            phase: Some(MessagePhase::Commentary),
        };
        sidecar.observe_recorded_item(&phase_message);

        let checkpoint = sidecar.take_pending_checkpoint().expect("checkpoint");
        let units = sidecar
            .selectable_units(
                checkpoint.checkpoint_id.as_str(),
                MAX_UNITS_PER_RETRIEVE,
                MAX_RAW_BYTES_PER_RETRIEVE,
            )
            .expect("units");

        assert_eq!(checkpoint.eligible_unit_count, 1);
        assert_eq!(units.len(), 1);
        assert!(matches!(units[0].kind, PromptGcUnitKind::ToolPair));
        assert!(units[0].payload_text.contains("legacy-shell"));
    }

    #[test]
    fn clearing_pending_calls_prevents_rewrite_stale_pairing_on_reused_call_id() {
        let mut sidecar = PromptGcSidecar::default();
        sidecar.bind_turn("turn-1");

        let old_call = ResponseItem::FunctionCall {
            id: None,
            call_id: "call-1".to_string(),
            name: "shell".to_string(),
            namespace: None,
            arguments: "{\"cmd\":\"old\"}".to_string(),
        };
        sidecar.observe_recorded_item(&old_call);
        sidecar.clear_pending_calls_for_rewrite();

        let new_call = ResponseItem::FunctionCall {
            id: None,
            call_id: "call-1".to_string(),
            name: "shell".to_string(),
            namespace: None,
            arguments: "{\"cmd\":\"new\"}".to_string(),
        };
        sidecar.observe_recorded_item(&new_call);

        let output = ResponseItem::FunctionCallOutput {
            call_id: "call-1".to_string(),
            output: FunctionCallOutputPayload {
                body: FunctionCallOutputBody::Text("/tmp".to_string()),
                success: Some(true),
            },
        };
        sidecar.observe_recorded_item(&output);

        let phase_message = ResponseItem::Message {
            id: Some("msg-1".to_string()),
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: "phase done".to_string(),
            }],
            end_turn: None,
            phase: Some(MessagePhase::Commentary),
        };
        sidecar.observe_recorded_item(&phase_message);

        let checkpoint = sidecar.take_pending_checkpoint().expect("checkpoint");
        let units = sidecar
            .selectable_units(
                checkpoint.checkpoint_id.as_str(),
                MAX_UNITS_PER_RETRIEVE,
                MAX_RAW_BYTES_PER_RETRIEVE,
            )
            .expect("units");

        assert_eq!(units.len(), 1);
        assert!(units[0].payload_text.contains("{\"cmd\":\"new\"}"));
        assert!(!units[0].payload_text.contains("{\"cmd\":\"old\"}"));
    }

    #[test]
    fn blocked_turn_suppresses_future_checkpoints() {
        let mut sidecar = PromptGcSidecar::default();
        sidecar.bind_turn("turn-1");

        let phase_message = ResponseItem::Message {
            id: Some("msg-1".to_string()),
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: "phase done".to_string(),
            }],
            end_turn: None,
            phase: Some(MessagePhase::Commentary),
        };
        sidecar.observe_recorded_item(&phase_message);
        let checkpoint = sidecar.take_pending_checkpoint().expect("checkpoint");
        sidecar.block_remaining_turn(&checkpoint.checkpoint_id, "usage limit");

        let later_phase_message = ResponseItem::Message {
            id: Some("msg-2".to_string()),
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: "later phase".to_string(),
            }],
            end_turn: None,
            phase: Some(MessagePhase::FinalAnswer),
        };
        sidecar.observe_recorded_item(&later_phase_message);

        assert!(sidecar.take_pending_checkpoint().is_none());
        assert_eq!(
            sidecar.status.blocked_reason.as_deref(),
            Some("usage limit")
        );
    }

    #[test]
    fn blocked_turn_stops_recording_dead_units_and_pending_calls() {
        let mut sidecar = PromptGcSidecar::default();
        sidecar.bind_turn("turn-1");

        let call = ResponseItem::FunctionCall {
            id: None,
            call_id: "call-1".to_string(),
            name: "shell".to_string(),
            namespace: None,
            arguments: "{\"cmd\":\"pwd\"}".to_string(),
        };
        sidecar.observe_recorded_item(&call);

        let phase_message = ResponseItem::Message {
            id: Some("msg-1".to_string()),
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: "phase done".to_string(),
            }],
            end_turn: None,
            phase: Some(MessagePhase::Commentary),
        };
        sidecar.observe_recorded_item(&phase_message);
        let checkpoint = sidecar.take_pending_checkpoint().expect("checkpoint");
        sidecar.block_remaining_turn(&checkpoint.checkpoint_id, "usage limit");

        assert!(sidecar.pending_function_calls.is_empty());
        assert!(sidecar.pending_custom_calls.is_empty());

        let reasoning = ResponseItem::Reasoning {
            id: "reasoning-2".to_string(),
            summary: Vec::new(),
            content: None,
            encrypted_content: None,
        };
        sidecar.observe_recorded_item(&reasoning);

        let later_call = ResponseItem::FunctionCall {
            id: None,
            call_id: "call-2".to_string(),
            name: "shell".to_string(),
            namespace: None,
            arguments: "{\"cmd\":\"later\"}".to_string(),
        };
        sidecar.observe_recorded_item(&later_call);

        assert_eq!(sidecar.units.len(), 0);
        assert!(sidecar.pending_function_calls.is_empty());
        assert!(sidecar.pending_custom_calls.is_empty());
    }

    #[test]
    fn fail_cycle_clears_runtime_state_without_blocking_turn() {
        let mut sidecar = PromptGcSidecar::default();
        sidecar.bind_turn("turn-1");

        let phase_message = ResponseItem::Message {
            id: Some("msg-1".to_string()),
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: "phase done".to_string(),
            }],
            end_turn: None,
            phase: Some(MessagePhase::Commentary),
        };
        sidecar.observe_recorded_item(&phase_message);
        let checkpoint = sidecar.take_pending_checkpoint().expect("checkpoint");

        sidecar.fail_cycle(&checkpoint.checkpoint_id, "request failed");

        assert!(!sidecar.running);
        assert!(sidecar.active_checkpoint.is_none());
        assert_eq!(sidecar.status.blocked_reason, None);
    }

    #[test]
    fn skip_cycle_clears_runtime_state_without_poisoning_status() {
        let mut sidecar = PromptGcSidecar::default();
        sidecar.bind_turn("turn-1");

        let phase_message = ResponseItem::Message {
            id: Some("msg-1".to_string()),
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: "phase done".to_string(),
            }],
            end_turn: None,
            phase: Some(MessagePhase::Commentary),
        };
        sidecar.observe_recorded_item(&phase_message);
        let checkpoint = sidecar.take_pending_checkpoint().expect("checkpoint");

        sidecar.skip_cycle(&checkpoint.checkpoint_id);

        assert!(!sidecar.running);
        assert!(sidecar.active_checkpoint.is_none());
        assert_eq!(sidecar.status.blocked_reason, None);
    }
}
